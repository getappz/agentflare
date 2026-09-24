// Inter-agent message drain for a run's wait loop (`poll_agent_messages`),
// `include!`d into work_item_pipeline.rs so it shares that module's types.

/// At most this often a run's wait loop checks for inter-agent messages
/// addressed to its owner -- one indexed probe of `agentflare.db`.
const AGENT_MESSAGE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// A message is left this long for the in-turn paths (the running agent's
/// hooks / MCP result piggyback deliver it within one tool call) before
/// this between-turns fallback takes it.
const AGENT_MESSAGE_GRACE_SECS: i64 = 20;

/// Message texts headed for this run's next turn prompt. The flag flips
/// once a text is found in a context a step loaded, so its later absence
/// means a turn consumed it -- not that a step's stale write-back dropped it.
#[derive(Default)]
struct MessageOutbox {
    entries: Vec<(String, bool)>,
}

impl MessageOutbox {
    /// Applied (via `patch_run_data`) to every context the engine loads or
    /// writes back for the run: keeps each text in `pending_corrections`
    /// until a turn has had it, then lets it go.
    fn apply(&mut self, pending: &mut Vec<String>) {
        self.entries.retain_mut(|(text, seen)| {
            if pending.contains(text) {
                *seen = true;
                true
            } else if *seen {
                false
            } else {
                pending.push(text.clone());
                true
            }
        });
    }
}

type Outboxes = std::collections::HashMap<
    flare_workflow::WorkflowRunId,
    (
        std::time::Instant,
        std::sync::Arc<std::sync::Mutex<MessageOutbox>>,
    ),
>;

fn message_outboxes() -> &'static std::sync::Mutex<Outboxes> {
    static OUTBOXES: std::sync::OnceLock<std::sync::Mutex<Outboxes>> = std::sync::OnceLock::new();
    OUTBOXES.get_or_init(Default::default)
}

/// Drains inter-agent messages addressed to this run's `owner` (see
/// `crate::messages`) into `pending_corrections`, so the next turn's prompt
/// carries them. Throttled to [`AGENT_MESSAGE_POLL_INTERVAL`] per run, so
/// it's cheap to call from the wait loop's fast tick.
///
/// A plain `StateStore::update` would be undone by the in-flight turn's
/// write-back (a loop iteration writes back the context it loaded before
/// the update), so the texts go through `patch_run_data`, which the engine
/// re-applies to every context it loads and writes back. That patch slot is
/// shared with `adopt_existing_run`'s owner rebind and replaces it, so this
/// patch re-asserts `owner` too -- the same owner this waiter heartbeats for.
async fn poll_agent_messages(
    eng: &WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>,
    run_id: flare_workflow::WorkflowRunId,
    owner: &str,
) {
    let outbox = {
        let mut map = message_outboxes()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(run_id).or_insert_with(|| {
            (
                std::time::Instant::now() - AGENT_MESSAGE_POLL_INTERVAL,
                Default::default(),
            )
        });
        if entry.0.elapsed() < AGENT_MESSAGE_POLL_INTERVAL {
            return;
        }
        entry.0 = std::time::Instant::now();
        std::sync::Arc::clone(&entry.1)
    };
    let key = owner.to_string();
    let taken = tokio::task::spawn_blocking(move || {
        let conn = crate::messages::open_fast()?;
        if !crate::messages::has_undelivered(&conn, &key).ok()? {
            return None;
        }
        let now = crate::claims::now();
        crate::messages::take_undelivered_before(
            &conn,
            &key,
            crate::messages::MAX_BATCH,
            now - AGENT_MESSAGE_GRACE_SECS,
            now,
        )
        .ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    if taken.is_empty() {
        return;
    }
    let text = crate::messages::format_delivery(&taken);
    outbox
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entries
        .push((text.clone(), false));
    let owner = owner.to_string();
    let patch_outbox = std::sync::Arc::clone(&outbox);
    let patched = eng
        .patch_run_data(run_id, move |data: &mut WorkItemData| {
            data.owner = owner.clone();
            patch_outbox
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .apply(&mut data.pending_corrections);
        })
        .await;
    if patched.is_err() {
        outbox
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .retain(|(t, _)| *t != text);
        let ids: Vec<i64> = taken.iter().map(|m| m.id).collect();
        let _ = tokio::task::spawn_blocking(move || {
            crate::messages::open_fast().map(|c| crate::messages::requeue(&c, &ids))
        })
        .await;
    }
}

#[cfg(test)]
mod agent_message_outbox_tests {
    use super::MessageOutbox;

    #[test]
    fn outbox_survives_a_stale_write_back_and_lets_go_once_consumed() {
        let mut ob = MessageOutbox::default();
        ob.entries.push(("msg".into(), false));
        // Registration against the stored state: added.
        let mut stored = vec![];
        ob.apply(&mut stored);
        assert_eq!(stored, vec!["msg".to_string()]);
        // The in-flight turn writes back the context it loaded before: re-added.
        let mut stale = vec!["older correction".to_string()];
        ob.apply(&mut stale);
        assert!(stale.contains(&"msg".to_string()));
        // The next turn loads it (seen), consumes it, writes back empty: gone.
        let mut loaded = stale.clone();
        ob.apply(&mut loaded);
        let mut consumed: Vec<String> = vec![];
        ob.apply(&mut consumed);
        assert!(consumed.is_empty());
        assert!(ob.entries.is_empty());
    }
}

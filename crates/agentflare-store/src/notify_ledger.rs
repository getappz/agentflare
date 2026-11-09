//! Durable Telegram-notification ledger (item #458).
//!
//! Tracks which `(item_id, event_key)` pairs have already been sent as an
//! immediate card, and queues later notifications into a pending digest
//! table shared across processes and restarts. Not process memory — the
//! daemon and every MCP process open the same `store.db`.

use rusqlite::{OptionalExtension, params};

use crate::{Error, Store};

pub const LAST_DIGEST_KV_KEY: &str = "_notify_last_digest_at";

/// One pending (or taken) digest row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestEntry {
    pub item_id: String,
    pub event_key: String,
    pub latest_text: String,
    pub seq: i64,
    pub name: String,
    pub updated_at: i64,
    pub revision: i64,
    /// Optional live PR URL, supplied by the daemon's current-gate snapshot.
    pub link: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyDecision {
    /// First immediate for this item — caller should send the card now.
    Immediate,
    /// Item already had an immediate (any event); queue for the hourly digest.
    Digest,
}

/// Immediate-send budget an event draws from. Approval cards carry a button or
/// attachment that a digest line lacks, so each approval class has its own budget
/// and an earlier informational card cannot use it up. A host-gate state change
/// is budgeted per state (`held` and `resumed` are both worth sending). All
/// other events share one budget (`""`).
fn budget(event_key: &str) -> &str {
    for class in ["plan-approval", "pr-approval"] {
        if event_key
            .strip_prefix(class)
            .is_some_and(|r| r.starts_with(':'))
        {
            return class;
        }
    }
    if event_key.starts_with("host-gate:") {
        event_key
    } else {
        ""
    }
}

/// Decide whether `event_key` for `item_id` may fire immediately.
///
/// Rule: at most one immediate send per `(item_id, budget)` ever (see
/// [`budget`]). The first call records `(item_id, event_key)` and returns
/// [`NotifyDecision::Immediate`]; every later call drawing on the same budget
/// upserts a digest row and returns [`NotifyDecision::Digest`].
#[allow(clippy::too_many_arguments)]
pub fn decide_and_record(
    store: &Store,
    item_id: &str,
    event_key: &str,
    digest_text: &str,
    seq: i64,
    name: &str,
    now: i64,
) -> Result<NotifyDecision, Error> {
    let mut guard = store.conn();
    let conn = guard.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let sent: Vec<String> = {
        let mut stmt =
            conn.prepare("SELECT event_key FROM store_notify_sent WHERE item_id = ?1")?;
        let keys = stmt.query_map(params![item_id], |r| r.get(0))?;
        keys.collect::<Result<_, _>>()?
    };
    let already = sent.iter().any(|k| budget(k) == budget(event_key));
    if already {
        conn.execute(
            "UPDATE store_notify_sent SET latest_text = ?2, updated_at = ?3 WHERE item_id = ?1",
            params![item_id, digest_text, now],
        )?;
        conn.execute(
            "INSERT INTO store_notify_digest
                (item_id, event_key, latest_text, seq, name, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                 (SELECT COALESCE(MAX(revision), 0) + 1 FROM store_notify_digest))
             ON CONFLICT(item_id, event_key) DO UPDATE SET
                latest_text = excluded.latest_text,
                seq = excluded.seq,
                name = excluded.name,
                updated_at = excluded.updated_at,
                revision = excluded.revision",
            params![item_id, event_key, digest_text, seq, name, now],
        )?;
        conn.commit()?;
        return Ok(NotifyDecision::Digest);
    }
    conn.execute(
        "INSERT INTO store_notify_sent (item_id, event_key, first_sent_at, latest_text, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?3)",
        params![item_id, event_key, now, digest_text],
    )?;
    conn.commit()?;
    Ok(NotifyDecision::Immediate)
}

/// Last reported reason is retained after a digest is acknowledged, for
/// items that remain on a human gate without another notification call.
pub fn latest_reason(store: &Store, item_id: &str) -> Result<Option<(String, i64)>, Error> {
    Ok(store.conn().query_row(
        "SELECT latest_text, updated_at FROM store_notify_sent WHERE item_id = ?1 ORDER BY updated_at DESC LIMIT 1",
        [item_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?)
}

/// All pending digest rows, newest first. Does not clear.
pub fn list_digest(store: &Store) -> Result<Vec<DigestEntry>, Error> {
    read_digest(&store.conn())
}

fn read_digest(conn: &rusqlite::Connection) -> Result<Vec<DigestEntry>, Error> {
    let mut stmt = conn.prepare(
        "SELECT item_id, event_key, latest_text, seq, name, updated_at, revision
         FROM store_notify_digest
         ORDER BY updated_at DESC, revision DESC, item_id ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(DigestEntry {
            item_id: row.get(0)?,
            event_key: row.get(1)?,
            latest_text: row.get(2)?,
            seq: row.get(3)?,
            name: row.get(4)?,
            updated_at: row.get(5)?,
            revision: row.get(6)?,
            link: None,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Reserve the hourly send durably before calling the sender. A failed send
/// retains pending rows; a crash cannot cause another process to repeat the
/// send in the same hour. Acknowledgement only deletes reported versions.
pub fn send_digest(
    store: &Store,
    current: &[DigestEntry],
    now: i64,
    send: impl FnOnce(&str) -> bool,
) -> Result<bool, Error> {
    let entries;
    let text;
    {
        let mut guard = store.conn();
        let tx = guard.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let last: Option<i64> = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM store_kv WHERE key = ?1",
                [LAST_DIGEST_KV_KEY],
                |r| r.get(0),
            )
            .optional()?;
        if last.is_some_and(|last| now.saturating_sub(last) < 3600) {
            return Ok(false);
        }
        entries = read_digest(&tx)?;
        let mut all = entries.clone();
        all.extend_from_slice(current);
        text = render_digest_lines(&all).join("\n");
        if text.is_empty() {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO store_kv (key, value, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![LAST_DIGEST_KV_KEY, now.to_string().as_bytes(), now],
        )?;
        tx.commit()?;
    }
    if !send(&text) {
        return Ok(false);
    }
    let conn = store.conn();
    for entry in entries {
        conn.execute(
            "DELETE FROM store_notify_digest WHERE item_id = ?1 AND event_key = ?2
             AND revision = ?3",
            params![entry.item_id, entry.event_key, entry.revision],
        )?;
    }
    Ok(true)
}

/// Drain is only used by tests; production acknowledges the reported snapshot.
#[cfg(test)]
pub fn take_digest(store: &Store) -> Result<Vec<DigestEntry>, Error> {
    let entries = list_digest(store)?;
    store
        .conn()
        .execute("DELETE FROM store_notify_digest", [])?;
    Ok(entries)
}

pub fn last_digest_at(store: &Store) -> Result<Option<i64>, Error> {
    match store.kv_get(LAST_DIGEST_KV_KEY)? {
        Some(entry) => {
            let s = std::str::from_utf8(&entry.value).unwrap_or("");
            Ok(s.parse::<i64>().ok())
        }
        None => Ok(None),
    }
}

pub const ATTENTION_CHECK_KV_KEY: &str = "_notify_last_attention_check_at";

/// Claim the rolling-hour slot for querying current gates. `send_digest` only
/// records its own reservation when there is text to send, so without this an
/// empty digest would re-query GitHub per `in_review` item on every tick.
/// Returns `true` (and records `now`) at most once per hour across processes.
pub fn reserve_attention_check(store: &Store, now: i64) -> Result<bool, Error> {
    let mut guard = store.conn();
    let tx = guard.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let last: Option<i64> = tx
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM store_kv WHERE key = ?1",
            [ATTENTION_CHECK_KV_KEY],
            |r| r.get(0),
        )
        .optional()?;
    if last.is_some_and(|last| now.saturating_sub(last) < 3600) {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO store_kv (key, value, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![ATTENTION_CHECK_KV_KEY, now.to_string().as_bytes(), now],
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn set_last_digest_at(store: &Store, now: i64) -> Result<(), Error> {
    store.kv_set(LAST_DIGEST_KV_KEY, now.to_string().as_bytes())?;
    Ok(())
}

/// Collapse digest rows to one line per item (latest reason wins), newest first.
pub fn render_digest_lines(entries: &[DigestEntry]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    let mut sorted: Vec<_> = entries.iter().collect();
    sorted.sort_by_key(|e| {
        (
            std::cmp::Reverse(e.updated_at),
            std::cmp::Reverse(e.revision),
            &e.item_id,
        )
    });
    for e in sorted {
        if !seen.insert(e.item_id.clone()) {
            continue;
        }
        let link = entries
            .iter()
            .find(|entry| entry.item_id == e.item_id && entry.link.is_some())
            .and_then(|entry| entry.link.as_deref());
        let mut reason = e
            .latest_text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(link) = link
            && !reason.contains(link)
        {
            reason.push_str(&format!(" ({link})"));
        }
        lines.push(format!(
            "#{} {}: {}",
            e.seq,
            e.name.split_whitespace().collect::<Vec<_>>().join(" "),
            reason
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    #[test]
    fn concurrent_process_handles_claim_only_one_immediate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let stores: Vec<_> = (0..8).map(|_| Store::open_file(&path).unwrap()).collect();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(stores.len()));
        let workers: Vec<_> = stores
            .into_iter()
            .enumerate()
            .map(|(n, store)| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    decide_and_record(
                        &store,
                        "item",
                        &format!("event-{n}"),
                        "reason",
                        1,
                        "name",
                        10,
                    )
                    .unwrap()
                })
            })
            .collect();
        let immediate = workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|d| *d == NotifyDecision::Immediate)
            .count();
        assert_eq!(immediate, 1);
    }

    #[test]
    fn digest_empty_hourly_and_failed_sends_retain_pending() {
        let s = store();
        let mut sent = Vec::new();
        assert!(
            !send_digest(&s, &[], 10, |text| {
                sent.push(text.to_string());
                true
            })
            .unwrap()
        );
        decide_and_record(&s, "i", "a", "first", 1, "Name", 1).unwrap();
        decide_and_record(&s, "i", "b", "latest", 1, "Name", 2).unwrap();
        assert!(
            send_digest(&s, &[], 10, |text| {
                sent.push(text.to_string());
                true
            })
            .unwrap()
        );
        assert_eq!(sent, ["#1 Name: latest"]);
        assert!(list_digest(&s).unwrap().is_empty());
        decide_and_record(&s, "i", "c", "new", 1, "Name", 11).unwrap();
        assert!(!send_digest(&s, &[], 3609, |_| panic!("within hour")).unwrap());
        assert!(!send_digest(&s, &[], 3610, |_| false).unwrap());
        assert_eq!(list_digest(&s).unwrap().len(), 1);
        assert!(send_digest(&s, &[], 7210, |_| true).unwrap());
    }

    #[test]
    fn digest_includes_current_gates_and_deduplicates_latest() {
        let s = store();
        let current = vec![DigestEntry {
            item_id: "i".into(),
            event_key: "gate".into(),
            latest_text: "still waiting".into(),
            seq: 1,
            name: "N".into(),
            updated_at: 1,
            revision: 0,
            link: None,
        }];
        assert!(send_digest(&s, &current, 10, |text| text == "#1 N: still waiting").unwrap());
        assert!(!send_digest(&s, &current, 11, |_| panic!("duplicate digest")).unwrap());
        assert!(send_digest(&s, &current, 3610, |_| true).unwrap());
    }

    #[test]
    fn same_second_latest_update_wins_and_ack_preserves_new_updates() {
        let s = store();
        decide_and_record(&s, "i", "first", "first", 1, "N", 10).unwrap();
        decide_and_record(&s, "i", "a", "old", 1, "N", 10).unwrap();
        decide_and_record(&s, "i", "b", "second", 1, "N", 10).unwrap();
        decide_and_record(&s, "i", "a", "latest", 1, "N", 10).unwrap();
        assert!(
            send_digest(&s, &[], 20, |text| {
                assert_eq!(text, "#1 N: latest");
                decide_and_record(&s, "i", "a", "latest", 1, "N", 10).unwrap();
                true
            })
            .unwrap()
        );
        let pending = list_digest(&s).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_key, "a");
    }

    #[test]
    fn hourly_reservation_is_shared_by_fresh_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let s = Store::open_file(&path).unwrap();
        decide_and_record(&s, "i", "a", "first", 1, "N", 1).unwrap();
        decide_and_record(&s, "i", "b", "pending", 1, "N", 2).unwrap();
        let mut sends = 0;
        assert!(
            send_digest(&s, &[], 10, |_| {
                sends += 1;
                let other_daemon = Store::open_file(&path).unwrap();
                assert!(
                    !send_digest(&other_daemon, &[], 10, |_| panic!("duplicate send")).unwrap()
                );
                true
            })
            .unwrap()
        );
        assert_eq!(sends, 1);
    }

    #[test]
    fn pending_update_wins_same_second_current_snapshot_and_keeps_pr_link() {
        let s = store();
        decide_and_record(&s, "i", "first", "first", 1, "N", 10).unwrap();
        let current = vec![DigestEntry {
            item_id: "i".into(),
            event_key: "current".into(),
            latest_text: "stale snapshot".into(),
            seq: 1,
            name: "N".into(),
            updated_at: 10,
            revision: 0,
            link: Some("https://github.com/o/r/pull/1".into()),
        }];
        decide_and_record(&s, "i", "new", "latest reason", 1, "N", 10).unwrap();
        assert!(
            send_digest(&s, &current, 20, |text| {
                assert_eq!(text, "#1 N: latest reason (https://github.com/o/r/pull/1)");
                true
            })
            .unwrap()
        );
    }

    #[test]
    fn same_event_key_twice_one_immediate_then_digest() {
        let s = store();
        assert_eq!(
            decide_and_record(&s, "i1", "human-gate:a", "a", 1, "n", 10).unwrap(),
            NotifyDecision::Immediate
        );
        assert_eq!(
            decide_and_record(&s, "i1", "human-gate:a", "a again", 1, "n", 11).unwrap(),
            NotifyDecision::Digest
        );
        let dig = list_digest(&s).unwrap();
        assert_eq!(dig.len(), 1);
        assert_eq!(dig[0].latest_text, "a again");
    }

    #[test]
    fn dedup_survives_fresh_handle_on_same_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        {
            let s = Store::open_file(&path).unwrap();
            assert_eq!(
                decide_and_record(&s, "i1", "k", "first", 7, "n", 1).unwrap(),
                NotifyDecision::Immediate
            );
        }
        let s2 = Store::open_file(&path).unwrap();
        assert_eq!(
            decide_and_record(&s2, "i1", "k", "again", 7, "n", 2).unwrap(),
            NotifyDecision::Digest
        );
    }

    #[test]
    fn attention_check_is_reserved_once_per_hour() {
        let s = store();
        assert!(reserve_attention_check(&s, 1000).unwrap());
        assert!(!reserve_attention_check(&s, 1000 + 3599).unwrap());
        assert!(reserve_attention_check(&s, 1000 + 3600).unwrap());
    }

    #[test]
    fn approval_card_after_informational_card_is_still_immediate() {
        let s = store();
        let send = |key: &str, at| decide_and_record(&s, "i1", key, "t", 1, "n", at).unwrap();
        assert_eq!(send("human-gate:a", 10), NotifyDecision::Immediate);
        // The plan-approval card carries a button and plan.md: it must not be digested.
        assert_eq!(send("plan-approval:p1", 11), NotifyDecision::Immediate);
        assert_eq!(send("pr-approval:r:1:sha:x", 12), NotifyDecision::Immediate);
        // Repeats within a class, and further informational cards, still digest.
        assert_eq!(send("plan-approval:p1", 13), NotifyDecision::Digest);
        assert_eq!(send("plan-approval:p2", 14), NotifyDecision::Digest);
        assert_eq!(send("pr-approval:r:1:sha2:x", 15), NotifyDecision::Digest);
        assert_eq!(send("human-gate:b", 16), NotifyDecision::Digest);
    }

    #[test]
    fn host_gate_budget_is_per_state() {
        let s = store();
        let send = |key: &str, at| decide_and_record(&s, "host", key, "t", 0, "host", at).unwrap();
        assert_eq!(send("host-gate:held", 1), NotifyDecision::Immediate);
        assert_eq!(send("host-gate:resumed", 2), NotifyDecision::Immediate);
        assert_eq!(send("host-gate:held", 3), NotifyDecision::Digest);
    }

    #[test]
    fn different_event_key_after_first_goes_to_digest() {
        let s = store();
        assert_eq!(
            decide_and_record(&s, "i1", "human-gate:a", "a", 1, "n", 10).unwrap(),
            NotifyDecision::Immediate
        );
        assert_eq!(
            decide_and_record(&s, "i1", "human-gate:b", "b", 1, "n", 11).unwrap(),
            NotifyDecision::Digest
        );
        let dig = list_digest(&s).unwrap();
        assert_eq!(dig.len(), 1);
        assert_eq!(dig[0].event_key, "human-gate:b");
    }

    #[test]
    fn digest_one_line_per_item_latest_wins() {
        let s = store();
        decide_and_record(&s, "i1", "a", "old", 1, "Alpha", 10).unwrap();
        decide_and_record(&s, "i1", "b", "newer", 1, "Alpha", 20).unwrap();
        decide_and_record(&s, "i1", "c", "newest", 1, "Alpha", 30).unwrap();
        decide_and_record(&s, "i2", "x", "other", 2, "Beta", 25).unwrap();
        // i2 has no prior immediate — its first call was Immediate and left
        // no digest. Seed a digest for i2 explicitly via a second call:
        decide_and_record(&s, "i2", "y", "beta-latest", 2, "Beta", 40).unwrap();
        let lines = render_digest_lines(&list_digest(&s).unwrap());
        assert_eq!(
            lines,
            vec![
                "#2 Beta: beta-latest".to_string(),
                "#1 Alpha: newest".to_string(),
            ]
        );
    }

    #[test]
    fn take_digest_clears_pending() {
        let s = store();
        decide_and_record(&s, "i1", "a", "x", 1, "n", 1).unwrap();
        decide_and_record(&s, "i1", "b", "y", 1, "n", 2).unwrap();
        let taken = take_digest(&s).unwrap();
        assert!(!taken.is_empty());
        assert!(list_digest(&s).unwrap().is_empty());
    }

    #[test]
    fn last_digest_at_round_trips() {
        let s = store();
        assert_eq!(last_digest_at(&s).unwrap(), None);
        set_last_digest_at(&s, 12345).unwrap();
        assert_eq!(last_digest_at(&s).unwrap(), Some(12345));
    }
}

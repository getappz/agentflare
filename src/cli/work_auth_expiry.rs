/// Applied to an item whose dispatch failed on `auth_runner::is_auth_expired`
/// -- a distinct, queryable/durable status (item #173's own title) instead of
/// the failure blending into generic step-failure noise. Auto-created in the
/// resolved project the first time it's needed; see `ensure_auth_expired_label`.
const AUTH_EXPIRED_LABEL: &str = "auth-expired";

/// Looks up `AUTH_EXPIRED_LABEL` in the item's resolved project, creating it
/// on first use -- mirrors `supervisor.rs`'s `label_id_by_name` pattern
/// (list-then-match) but this call site has no pre-built map, and unlike
/// that daemon-tick path, there's no seeding step to have created this label
/// ahead of time. Best-effort: `None` on any backend error just skips
/// labeling, never blocks reporting the underlying failure.
fn ensure_auth_expired_label(mcp: &AgentflareMcp) -> Option<String> {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).ok()?;
        if let Ok(labels) = agentflare_backend::label::list_by_project(conn, &project.id)
            && let Some(existing) = labels.into_iter().find(|l| l.name == AUTH_EXPIRED_LABEL)
        {
            return Some(existing.id);
        }
        agentflare_backend::label::create(
            conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project.id.clone()),
                workspace_id: project.workspace_id.clone(),
                name: AUTH_EXPIRED_LABEL.to_string(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .ok()
        .map(|l| l.id)
    })
    .ok()
    .flatten()
}

/// Handles an `auth_runner::is_auth_expired`-shaped dispatch failure: unlike
/// a rate limit, the same expired credential fails identically on every
/// retry, so this fails the item straight to terminal (`fatal: true`, no
/// `classify_and_cooldown` retry-after) instead of burning the retry budget
/// against a guaranteed-useless retry -- exactly what item #164's 15h
/// incident did before this fix. Applies `AUTH_EXPIRED_LABEL`, logs a
/// dedicated error-level line (distinct from the generic `crate::ui::error`
/// every failure already gets, so this doesn't blend into that noise), and
/// reuses the existing human-notify path (`notify_human_gate` /
/// `channels::send_message`) for an immediate alert instead of this being
/// discovered hours later. Returns `None` when `msg` isn't auth-expiry
/// shaped, so the caller falls through to the normal rate-limit/generic path.
fn handle_auth_expired(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    msg: &str,
    log: &mut dyn std::io::Write,
) -> Option<WorkOutcome> {
    if !crate::auth_runner::is_auth_expired(msg) {
        return None;
    }
    let alert = format!(
        "agentflare: error: item #{} ({}) hit an expired-auth failure — failing immediately \
         instead of retrying against a credential that will fail the same way every time; a \
         human must re-authenticate: {msg}",
        item.sequence_id, item.id
    );
    eprintln!("{alert}");
    let _ = writeln!(log, "{alert}");
    if let Some(label_id) = ensure_auth_expired_label(mcp) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(label_id),
            ..Default::default()
        });
    }
    crate::supervisor::notify_human_gate(item, &format!("agent auth expired — {msg}"));
    Some(WorkOutcome {
        exit_code: 1,
        retry_after_secs: None,
        fatal: true,
    })
}

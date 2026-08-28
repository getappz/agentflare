//! `item(action="doctor")` -- split out of `item.rs` to keep it under the
//! repo's LOC gate. MCP surface for `agentflare git doctor [--reclaim]
//! [--force]` (item #235) -- until now that worktree-hygiene scan/reclaim
//! only existed as a CLI command, so an agent hitting the `git worktree
//! remove/prune` shim denial (see `classify::classify_pure`'s
//! teardown-deny message) had no MCP-reachable way to actually clean up
//! stale/orphaned worktrees (item #465).

use super::*;

impl AgentflareMcp {
    /// `sequence_id` (as a string, matching `LaneHealth`) -> its state's
    /// `group_name` -- used by `item_doctor` to flag a worktree as orphaned
    /// when the item behind it is done but the worktree wasn't cleaned up.
    /// Mirrors `cli::git::item_state_groups`'s query, adapted to the MCP
    /// server's own DB-connection accessor. Best-effort like that sibling:
    /// any DB error just yields an empty map, so orphan detection silently
    /// finds nothing rather than failing the whole scan.
    fn item_state_groups_for_doctor(&self) -> std::collections::HashMap<String, String> {
        self.with_backend_db(|conn| {
            let mut stmt = match conn.prepare(
                "SELECT i.sequence_id, s.group_name FROM items i \
                 JOIN states s ON i.state_id = s.id WHERE i.deleted_at IS NULL",
            ) {
                Ok(s) => s,
                Err(_) => return std::collections::HashMap::new(),
            };
            match stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
                Ok(rows) => rows
                    .filter_map(|r| r.ok())
                    .map(|(seq, group)| (seq.to_string(), group))
                    .collect(),
                Err(_) => std::collections::HashMap::new(),
            }
        })
        .unwrap_or_default()
    }

    pub(crate) fn item_doctor(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let repo_root = self.worktree_repo_root();
        let staleness_days = req.staleness_days.unwrap_or(14).max(0) as u64;
        let item_states = self.item_state_groups_for_doctor();
        let report = flare_git_core::doctor::scan(&repo_root, staleness_days, &item_states);
        let reclaim = req.reclaim.unwrap_or(false);
        let force = req.force.unwrap_or(false);
        // Safety guard for the 2026-08-16 incident: an unscoped force-reclaim
        // (`force=true` with no `worktree`) does not just allow the intended
        // lane to be force-deleted -- it force-deletes EVERY dirty lane in the
        // report, including other agents' active worktrees with uncommitted
        // changes. Refuse that combination unless the caller explicitly opts
        // in to repo-wide intent via `repo_wide=true`.
        if reclaim && force && req.worktree.is_none() && !req.repo_wide.unwrap_or(false) {
            return Err(ErrorData::invalid_params(
                "doctor: `reclaim=true` + `force=true` with no `worktree` is a repo-wide \
                 sweep that force-deletes EVERY dirty lane, including other agents' \
                 uncommitted work. Pass `worktree` to fix one broken worktree, or \
                 `repo_wide=true` to explicitly confirm a repo-wide force-reclaim.",
                None,
            ));
        }
        let reclaimed = if reclaim {
            flare_git_core::doctor::reclaim_scoped(
                &repo_root,
                &report,
                force,
                req.worktree.as_deref(),
            )
        } else {
            Vec::new()
        };
        let mut value = serde_json::to_value(&report).unwrap_or_default();
        if let Some(obj) = value.as_object_mut() {
            obj.insert("reclaimed".to_string(), serde_json::json!(reclaimed));
        }
        Ok(serde_json::to_string_pretty(&value).unwrap_or_default())
    }
}

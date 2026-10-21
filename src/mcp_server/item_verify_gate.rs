//! Completion gate for `[verify] enforce` / `metadata.require_verify`.

use super::*;

impl AgentflareMcp {
    /// Completion gate (`[verify] enforce` / `metadata.require_verify`): when
    /// the item's acceptance criteria are not all passed with evidence, park
    /// it in `needs_input`, comment the per-criterion verdict, gate it for a
    /// human, and return true. Always false when verification isn't required.
    pub(super) fn hold_unverified(&self, item_id: &str, owner: &str) -> Result<bool, ErrorData> {
        let repo_root = self.worktree_repo_root();
        let Some((item, report)) = self.with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, item_id).map_err(map_backend_err)?;
            let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap_or_default();
            if !crate::verify::required(crate::verify::enforce_enabled(&repo_root), &meta) {
                return Ok::<_, ErrorData>(None);
            }
            Ok(
                crate::review::verify::completion_gate(&item.metadata, &item.description)
                    .map(|r| (item, r)),
            )
        })??
        else {
            return Ok(false);
        };
        self.with_backend_db(|conn| {
            let state = agentflare_backend::state::ensure_needs_input(conn, &item.project_id)
                .map_err(map_backend_err)?;
            agentflare_backend::item::update_state(conn, item_id, &state.id)
                .map_err(map_backend_err)?;
            if !owner.is_empty() {
                let _ = agentflare_backend::claim::release(conn, item_id, owner);
            }
            let _ = crate::job_controls::set_item_label(conn, &item, "needs-human-gate", true);
            Ok::<_, ErrorData>(())
        })??;
        self.post_item_comment(
            item_id,
            format!(
                "## verification blocked completion\n\nItem {item_id} moved to needs_input; acceptance criteria are not all verified:\n\n{report}"
            ),
        );
        Ok(true)
    }
}

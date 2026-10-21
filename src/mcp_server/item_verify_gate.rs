//! Completion gate for `[verify] enforce` / `metadata.require_verify`.

use super::*;
use crate::supervisor::NEEDS_HUMAN_GATE_LABEL;

impl AgentflareMcp {
    /// When the item's acceptance criteria are not all passed with evidence,
    /// park it in `needs_input`, release its claim, comment the per-criterion
    /// verdict, gate it for a human, and return the report. `None` when
    /// verification isn't required or the item passes. Re-running on an item
    /// already in `needs_input` still returns the report but doesn't repeat
    /// the comment or notification.
    pub(super) fn hold_unverified(&self, item_id: &str) -> Result<Option<String>, ErrorData> {
        let repo_root = self.worktree_repo_root();
        let Some((item, report)) = self.with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, item_id).map_err(map_backend_err)?;
            // Nothing left to promote on a finished item.
            let group = agentflare_backend::state::get(conn, &item.state_id)
                .map_err(map_backend_err)?
                .group_name;
            if matches!(group.as_str(), "completed" | "cancelled") {
                return Ok::<_, ErrorData>(None);
            }
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
            return Ok(None);
        };
        let labels = self.with_backend_db(|conn| {
            let state = agentflare_backend::state::ensure_needs_input(conn, &item.project_id)
                .map_err(map_backend_err)?;
            if item.state_id == state.id {
                return Ok::<_, ErrorData>(None);
            }
            agentflare_backend::item::update_state(conn, item_id, &state.id)
                .map_err(map_backend_err)?;
            if let Some(holder) = agentflare_backend::claim::current_owner(conn, item_id) {
                let _ = agentflare_backend::claim::release(conn, item_id, &holder);
            }
            let by_name: std::collections::HashMap<String, String> =
                agentflare_backend::label::list_by_project(conn, &item.project_id)
                    .map_err(map_backend_err)?
                    .into_iter()
                    .map(|l| (l.name, l.id))
                    .collect();
            if !by_name.contains_key(NEEDS_HUMAN_GATE_LABEL) {
                // First gate in this project: create the label (and attach it).
                let _ =
                    crate::job_controls::set_item_label(conn, &item, NEEDS_HUMAN_GATE_LABEL, true);
                return Ok(Some(None));
            }
            Ok(Some(Some(by_name)))
        })??;
        let Some(labels) = labels else {
            return Ok(Some(report));
        };
        if let Some(by_name) = labels {
            crate::supervisor::review_bots::ensure_needs_human_item_gate(self, &item, &by_name);
        }
        self.post_item_comment(
            item_id,
            format!(
                "## verification blocked completion\n\nItem {item_id} moved to needs_input; acceptance criteria are not all verified:\n\n{report}"
            ),
        );
        crate::supervisor::notify_human_gate(
            &item,
            &format!("verification blocked completion:\n{report}"),
        );
        Ok(Some(report))
    }
}

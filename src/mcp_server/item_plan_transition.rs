//! Plan-gate transition-field strip/restore for MCP `item` create/update.
//! Split out of `item.rs` to keep that file under the frozen LOC gate (#300).

/// Strips the plan-gate transition fields (`plan_status`, `plan_approved_by`,
/// `plan_approved_at`, `plan_rejection_reason`) out of caller-supplied
/// `create`/`update` metadata before it's persisted. Those fields are
/// server-authoritative: only `item_submit_plan`/`set_plan_status` (approve/
/// reject) may set them, since they write through `agentflare_backend::item`
/// directly rather than this MCP entry point. Without this, a caller could
/// `item(action="create", metadata={"plan_required":true,"plan_approver":
/// "human","plan_status":"approved"})` and start an item already
/// "approved", walking straight past the gate it claims to have
/// (CodeRabbit finding on item #573's PR). `plan_required`/`plan_approver`
/// themselves are left untouched -- those are the caller's explicit gating
/// choice, handled separately by `default_plan_gate_patch`.
pub(crate) fn strip_plan_transition_fields(metadata_str: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(metadata_str) else {
        return metadata_str.to_string();
    };
    if let Some(obj) = value.as_object_mut() {
        for key in PLAN_TRANSITION_FIELDS {
            obj.remove(key);
        }
    }
    value.to_string()
}

/// Server-authoritative plan-gate transition fields -- see
/// `strip_plan_transition_fields`/`restore_plan_transition_fields`.
const PLAN_TRANSITION_FIELDS: [&str; 4] = [
    "plan_status",
    "plan_approved_by",
    "plan_approved_at",
    "plan_rejection_reason",
];

/// `item_update`'s counterpart to `strip_plan_transition_fields`: an update
/// (unlike a create) is patching metadata onto an item that may already
/// carry a real, human-set approval, so blindly deleting these keys from the
/// outgoing metadata doesn't just refuse a *forged* value -- it destroys a
/// legitimate one every single time, because `UpdateItem::metadata` replaces
/// the column wholesale rather than merging (see `merge_metadata_patch`'s doc
/// comment). Any internal read-merge-write helper that copies the item's
/// current metadata forward while patching in an unrelated key (e.g.
/// `work_item_pipeline::persist_run_id` adding `workflow_run_id`,
/// `persist_comment_cursor` advancing `last_seen_comment_at`,
/// `supervisor::persist_repair_track`'s CodeRabbit bookkeeping -- all three
/// go through this same `item_update` entry point) would otherwise silently
/// wipe out `plan_status`/`plan_approved_by`/`plan_approved_at` the moment it
/// ran after a human approved the plan (live incident, item #281: a human's
/// approval was erased minutes after the supervisor's own dispatch persisted
/// its `workflow_run_id`).
///
/// Instead of deleting the caller-supplied value, each field is forced to
/// whatever the item's row in the DB currently holds *right now*, discarding
/// whatever the caller/helper put there -- exactly like the delete used to,
/// EXCEPT when the current value is the caller's own unrelated pass-through
/// copy, in which case this makes the write a no-op for that field instead of
/// a destructive one. The only two writers allowed to actually change these
/// fields (`item_submit_plan`, `set_plan_status` -- approve/reject) both
/// write through `agentflare_backend::item::update` directly, bypassing this
/// MCP entry point entirely, so they are unaffected.
pub(crate) fn restore_plan_transition_fields(metadata_str: &str, current_metadata: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(metadata_str) else {
        return metadata_str.to_string();
    };
    let current = serde_json::from_str::<serde_json::Value>(current_metadata).ok();
    if let Some(obj) = value.as_object_mut() {
        for key in PLAN_TRANSITION_FIELDS {
            match current.as_ref().and_then(|c| c.get(key)) {
                Some(v) => {
                    obj.insert(key.to_string(), v.clone());
                }
                None => {
                    obj.remove(key);
                }
            }
        }
    }
    value.to_string()
}


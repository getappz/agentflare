use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PlanGateMeta {
    #[serde(default)]
    pub plan_required: bool,
    #[serde(default)]
    pub plan_approver: Option<String>,
    #[serde(default)]
    pub plan_asset_id: Option<String>,
    #[serde(default)]
    pub plan_status: Option<String>,
    #[serde(default)]
    pub plan_approved_by: Option<String>,
    #[serde(default)]
    pub plan_approved_at: Option<i64>,
    #[serde(default)]
    pub plan_rejection_reason: Option<String>,
}

/// Reads only the plan-gate fields out of an item's `metadata` JSON blob.
/// Unknown/unrelated keys are ignored, so this is safe to call on metadata
/// carrying arbitrary other fields (`size`, `model`, ...).
///
/// Extracts each field independently rather than deserializing straight into
/// `PlanGateMeta` -- a single whole-struct `serde_json::from_str` fails (and
/// falls back to `PlanGateMeta::default()`, `plan_required: false`) the
/// moment ANY recognized field has the wrong JSON type, which would silently
/// open the gate on an otherwise-valid `plan_required: true` just because an
/// unrelated field (e.g. `plan_approved_at`) was malformed (CodeRabbit
/// finding on item #573's PR). Field-by-field extraction means a malformed
/// field simply reads as absent for itself, without corrupting the rest.
pub fn read_plan_gate(metadata: &str) -> PlanGateMeta {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(metadata) else {
        return PlanGateMeta::default();
    };
    let obj = value.as_object();
    let str_field = |key: &str| {
        obj.and_then(|o| o.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    PlanGateMeta {
        plan_required: obj
            .and_then(|o| o.get("plan_required"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        plan_approver: str_field("plan_approver"),
        plan_asset_id: str_field("plan_asset_id"),
        plan_status: str_field("plan_status"),
        plan_approved_by: str_field("plan_approved_by"),
        plan_approved_at: obj
            .and_then(|o| o.get("plan_approved_at"))
            .and_then(serde_json::Value::as_i64),
        plan_rejection_reason: str_field("plan_rejection_reason"),
    }
}

/// Read-parse-patch-reserialize: the ONLY safe way to write into an item's
/// `metadata` column, which `UpdateItem`/`crud::update` replace wholesale
/// rather than merge (see this plan's Global Constraints). `patch`'s
/// top-level keys overwrite `existing`'s; every other key in `existing`
/// is preserved untouched.
pub fn merge_metadata_patch(existing: &str, patch: serde_json::Value) -> String {
    let mut base: serde_json::Value = serde_json::from_str(existing)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if let (Some(base_obj), serde_json::Value::Object(patch_obj)) = (base.as_object_mut(), patch) {
        for (k, v) in patch_obj {
            base_obj.insert(k, v);
        }
    }
    base.to_string()
}

/// Backend-level immutability guard for a recorded human approval, applied by
/// `crud::update` to every wholesale `metadata` write. Many writers
/// (orphan reconcile, supervisor, handoff, failover) read-modify-write a
/// stale snapshot; if that snapshot predates `approve-plan`, the write
/// silently reverts `plan_status` to "pending" and drops
/// `plan_approved_at`/`plan_approved_by` (live incident, item #281).
///
/// When the stored row is `"approved"`, the incoming metadata keeps the
/// stored `plan_status`/`plan_approved_*` unless it is an explicit
/// transition: `"rejected"` (reject) or `"pending"` with a *present* and
/// different `plan_asset_id` (resubmit). A pending write that omits
/// `plan_asset_id` is not a resubmit. A fresh approval after a non-approved
/// status may replace `plan_approved_*`. Unparseable or non-object input is
/// returned unchanged.
pub fn preserve_plan_approval(incoming: &str, current: &str) -> String {
    let Ok(mut new) = serde_json::from_str::<serde_json::Value>(incoming) else {
        return incoming.to_string();
    };
    let Some(cur) = serde_json::from_str::<serde_json::Value>(current)
        .ok()
        .filter(|c| c.get("plan_approved_at").is_some() || c["plan_status"] == "approved")
    else {
        return incoming.to_string();
    };
    let Some(obj) = new.as_object_mut() else {
        return incoming.to_string();
    };
    let was_approved = cur["plan_status"] == "approved";
    let new_status = obj
        .get("plan_status")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    let new_status = new_status.as_deref();
    // Resubmit only when incoming carries a non-empty plan_asset_id that
    // differs from the stored one. A pending write that omits plan_asset_id
    // (or leaves it unchanged) is treated as a stale partial replace — the
    // live #281 shape — and must not clear the approval.
    let explicit_transition = match new_status {
        Some("rejected") => true,
        Some("pending") => {
            let new_asset = obj.get("plan_asset_id").and_then(|v| v.as_str());
            let cur_asset = cur.get("plan_asset_id").and_then(|v| v.as_str());
            match (new_asset, cur_asset) {
                (Some(n), Some(c)) => n != c,
                (Some(_), None) => true,
                _ => false,
            }
        }
        _ => false,
    };
    if was_approved && !explicit_transition {
        if let Some(v) = cur.get("plan_status") {
            obj.insert("plan_status".into(), v.clone());
        }
    }
    let fresh_approval = !was_approved && new_status == Some("approved");
    if !fresh_approval {
        for key in ["plan_approved_at", "plan_approved_by"] {
            if let Some(v) = cur.get(key) {
                obj.insert(key.into(), v.clone());
            }
        }
    }
    new.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanGateStatus {
    Open,
    /// `"none"` (never submitted), `"pending"`, or `"rejected"`.
    Blocked(String),
}

/// Whether an item may currently be claimed/dispatched. `plan_required`
/// unset or false is always `Open` — this gate is opt-in per item.
pub fn plan_gate_status(metadata: &str) -> PlanGateStatus {
    let gate = read_plan_gate(metadata);
    if !gate.plan_required {
        return PlanGateStatus::Open;
    }
    match gate.plan_status.as_deref() {
        Some("approved") => PlanGateStatus::Open,
        Some(other) => PlanGateStatus::Blocked(other.to_string()),
        None => PlanGateStatus::Blocked("none".to_string()),
    }
}

/// Default plan-gate policy for an item that hasn't explicitly set
/// `plan_required`/`plan_approver` in its metadata. Tunable by design —
/// see this plan's Global Constraints. Returns `None` when the item
/// doesn't meet any gating criterion (caller leaves the item ungated).
pub fn default_policy(priority: &str, metadata: &str) -> Option<(bool, &'static str)> {
    if matches!(priority, "urgent" | "high") {
        return Some((true, "human"));
    }
    let gate = read_plan_gate(metadata);
    // `size` isn't part of PlanGateMeta (it's a pre-existing, unrelated
    // metadata key) -- read it directly off the raw JSON instead.
    let size = serde_json::from_str::<serde_json::Value>(metadata)
        .ok()
        .and_then(|v| v.get("size").and_then(|s| s.as_str().map(str::to_string)));
    let _ = gate; // keeps read_plan_gate exercised if metadata shape changes later
    if size.as_deref() == Some("L") {
        return Some((true, "human"));
    }
    None
}

#[cfg(test)]
mod tests {
    const APPROVED: &str = r#"{"plan_status":"approved","plan_approved_at":5,"plan_approved_by":"h","plan_asset_id":"a"}"#;

    #[test]
    fn stale_write_cannot_revert_approval() {
        let out = preserve_plan_approval(
            r#"{"plan_status":"pending","plan_asset_id":"a","workflow_run_id":"x"}"#,
            APPROVED,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["plan_status"], "approved");
        assert_eq!(v["plan_approved_at"], 5);
        assert_eq!(v["plan_approved_by"], "h");
        assert_eq!(v["workflow_run_id"], "x");
    }

    #[test]
    fn reject_and_resubmit_still_transition() {
        let v: serde_json::Value = serde_json::from_str(&preserve_plan_approval(
            r#"{"plan_status":"rejected","plan_approved_at":5}"#,
            APPROVED,
        ))
        .unwrap();
        assert_eq!(v["plan_status"], "rejected");
        let v: serde_json::Value = serde_json::from_str(&preserve_plan_approval(
            r#"{"plan_status":"pending","plan_asset_id":"b"}"#,
            APPROVED,
        ))
        .unwrap();
        assert_eq!(v["plan_status"], "pending");
        assert_eq!(v["plan_approved_at"], 5);
    }

    #[test]
    fn fresh_approval_after_reject_replaces_approved_at() {
        let cur = r#"{"plan_status":"rejected","plan_approved_at":5}"#;
        let v: serde_json::Value = serde_json::from_str(&preserve_plan_approval(
            r#"{"plan_status":"approved","plan_approved_at":9,"plan_approved_by":"h2"}"#,
            cur,
        ))
        .unwrap();
        assert_eq!(v["plan_approved_at"], 9);
    }

    #[test]
    fn pending_without_plan_asset_id_is_not_a_resubmit() {
        // A partial/stale replace that drops plan_asset_id must not count as
        // "new plan" just because None != Some(old_id).
        let out = preserve_plan_approval(
            r#"{"plan_status":"pending","workflow_run_id":"x"}"#,
            APPROVED,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["plan_status"], "approved");
        assert_eq!(v["plan_approved_at"], 5);
        assert_eq!(v["plan_approved_by"], "h");
    }

    use super::*;

    #[test]
    fn merge_metadata_patch_preserves_unrelated_keys() {
        let existing = r#"{"size":"L","model":"opus"}"#;
        let merged = merge_metadata_patch(existing, serde_json::json!({"plan_status": "pending"}));
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["size"], "L");
        assert_eq!(value["model"], "opus");
        assert_eq!(value["plan_status"], "pending");
    }

    #[test]
    fn merge_metadata_patch_handles_empty_existing() {
        let merged = merge_metadata_patch("", serde_json::json!({"plan_status": "pending"}));
        let value: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["plan_status"], "pending");
    }

    #[test]
    fn read_plan_gate_defaults_when_fields_absent() {
        let gate = read_plan_gate(r#"{"size":"L"}"#);
        assert!(!gate.plan_required);
        assert_eq!(gate.plan_status, None);
    }

    #[test]
    fn read_plan_gate_reads_present_fields() {
        let gate = read_plan_gate(
            r#"{"plan_required":true,"plan_approver":"human","plan_status":"pending"}"#,
        );
        assert!(gate.plan_required);
        assert_eq!(gate.plan_approver.as_deref(), Some("human"));
        assert_eq!(gate.plan_status.as_deref(), Some("pending"));
    }

    #[test]
    fn read_plan_gate_ignores_a_malformed_unrelated_field_instead_of_defaulting_everything() {
        // A wrong-typed `plan_approved_at` (a string instead of i64) must not
        // wipe out a valid, present `plan_required: true` -- that would
        // silently open the gate (CodeRabbit finding on item #573's PR).
        let gate = read_plan_gate(r#"{"plan_required":true,"plan_approved_at":"invalid"}"#);
        assert!(
            gate.plan_required,
            "a malformed plan_approved_at must not clear plan_required"
        );
        assert_eq!(gate.plan_approved_at, None);
    }

    #[test]
    fn plan_gate_status_blocked_when_required_and_plan_approved_at_is_malformed() {
        let status = plan_gate_status(r#"{"plan_required":true,"plan_approved_at":"invalid"}"#);
        assert!(matches!(status, PlanGateStatus::Blocked(ref s) if s == "none"));
    }

    #[test]
    fn plan_gate_status_open_when_not_required() {
        assert!(matches!(plan_gate_status(r#"{}"#), PlanGateStatus::Open));
    }

    #[test]
    fn plan_gate_status_blocked_none_when_required_but_no_submission() {
        let status = plan_gate_status(r#"{"plan_required":true}"#);
        assert!(matches!(status, PlanGateStatus::Blocked(ref s) if s == "none"));
    }

    #[test]
    fn plan_gate_status_blocked_pending() {
        let status = plan_gate_status(r#"{"plan_required":true,"plan_status":"pending"}"#);
        assert!(matches!(status, PlanGateStatus::Blocked(ref s) if s == "pending"));
    }

    #[test]
    fn plan_gate_status_blocked_rejected() {
        let status = plan_gate_status(r#"{"plan_required":true,"plan_status":"rejected"}"#);
        assert!(matches!(status, PlanGateStatus::Blocked(ref s) if s == "rejected"));
    }

    #[test]
    fn plan_gate_status_open_when_approved() {
        let status = plan_gate_status(r#"{"plan_required":true,"plan_status":"approved"}"#);
        assert!(matches!(status, PlanGateStatus::Open));
    }

    #[test]
    fn default_policy_gates_urgent_and_high_priority() {
        assert_eq!(default_policy("urgent", "{}"), Some((true, "human")));
        assert_eq!(default_policy("high", "{}"), Some((true, "human")));
    }

    #[test]
    fn default_policy_gates_large_size() {
        assert_eq!(
            default_policy("medium", r#"{"size":"L"}"#),
            Some((true, "human"))
        );
    }

    #[test]
    fn default_policy_leaves_ordinary_items_ungated() {
        assert_eq!(default_policy("medium", r#"{"size":"M"}"#), None);
        assert_eq!(default_policy("low", "{}"), None);
    }
}

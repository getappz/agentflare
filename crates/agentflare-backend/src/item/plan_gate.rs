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
/// Unknown/unrelated keys are ignored (serde's default struct behavior),
/// so this is safe to call on metadata carrying arbitrary other fields
/// (`size`, `model`, ...).
pub fn read_plan_gate(metadata: &str) -> PlanGateMeta {
    serde_json::from_str(metadata).unwrap_or_default()
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

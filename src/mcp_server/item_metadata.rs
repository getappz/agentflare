//! Metadata normalization and caller-supplied update patches.

use rmcp::model::ErrorData;

/// Unwraps already-encoded JSON strings without changing create's handling
/// of plain strings or other JSON values.
pub(super) fn metadata_to_json_string(value: serde_json::Value) -> String {
    match &value {
        serde_json::Value::String(s) if serde_json::from_str::<serde_json::Value>(s).is_ok() => {
            s.clone()
        }
        _ => value.to_string(),
    }
}

/// Merges an update patch, deleting only keys explicitly sent as null.
/// Accepts the same double-encoded objects as create; rejects non-objects.
pub(super) fn merge_user_metadata_patch(
    current_metadata: &str,
    patch: serde_json::Value,
) -> Result<String, ErrorData> {
    // Legacy rows may hold the object double-encoded as a JSON string.
    let current_metadata = serde_json::from_str(current_metadata)
        .map_or_else(|_| "{}".to_string(), metadata_to_json_string);
    let current_metadata = current_metadata.as_str();
    let patch: serde_json::Value = serde_json::from_str(&metadata_to_json_string(patch))
        .map_err(|e| ErrorData::invalid_params(format!("metadata is not valid JSON: {e}"), None))?;
    let serde_json::Value::Object(patch_obj) = patch else {
        return Err(ErrorData::invalid_params(
            "metadata must be a JSON object; send {\"key\": null} to delete a key",
            None,
        ));
    };
    let deleted: Vec<String> = patch_obj
        .iter()
        .filter(|(_, v)| v.is_null())
        .map(|(k, _)| k.clone())
        .collect();
    let merged = agentflare_backend::item::plan_gate::merge_metadata_patch(
        current_metadata,
        serde_json::Value::Object(patch_obj),
    );
    if deleted.is_empty() {
        return Ok(merged);
    }
    let mut value: serde_json::Value =
        serde_json::from_str(&merged).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = value.as_object_mut() {
        for key in &deleted {
            obj.remove(key);
        }
    }
    Ok(value.to_string())
}

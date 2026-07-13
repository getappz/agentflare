// PreToolUse redirect classifier — nudges the agent toward agentflare-backend's
// own tools instead of ad-hoc file-based tracking. Flow ported from lean-ctx's
// hook_handlers (classify -> fail-open timeout -> dual JSON decision): a
// synchronous classify step run under a hard wall-clock budget, so a future
// redirect rule that needs IO (e.g. a backend DB lookup) can never wedge the
// host's tool call — it just falls through to allow instead.
use serde_json::{Value, json};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Hard wall-clock budget for classify_and_decide. Sized well under the 5s
/// timeout `init.rs` wires into `~/.claude/settings.json`'s PreToolUse entry,
/// so a hang here can never eat the whole hook budget.
const GATING_TIMEOUT: Duration = Duration::from_millis(2000);

/// Run `work` under a hard timeout, returning `None` (allow-passthrough) if
/// it doesn't finish in time. `work` only sends to a channel, never prints,
/// so a timed-out worker can't double-write stdout once it eventually
/// finishes.
fn decide_with_timeout<F>(timeout: Duration, work: F) -> Option<Value>
where
    F: FnOnce() -> Option<Value> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(timeout).unwrap_or(None)
}

fn is_spec_like_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/specs/") && normalized.ends_with(".md")
}

/// Classify one PreToolUse payload into a redirect reason, if any. Returns
/// `None` for every tool call that isn't one of agentflare's own redirect
/// targets.
fn classify(tool_name: &str, tool_input: Option<&Value>) -> Option<String> {
    match tool_name {
        "TodoWrite" => Some(
            "agentflare-backend's item tracker is wired up for this repo — use the `item` MCP tool (action=create) instead of TodoWrite for anything that should survive past this session.".to_string(),
        ),
        "Write" | "Edit" => {
            let path = tool_input
                .and_then(|v| v.get("file_path").or_else(|| v.get("path")))
                .and_then(Value::as_str)?;
            is_spec_like_path(path).then(|| {
                format!(
                    "specs/design docs/plans belong in agentflare artifacts (artifact_publish), not committed to the repo at '{path}' — see CLAUDE.md's 'Plans & specs live in agentflare artifacts' rule."
                )
            })
        }
        _ => None,
    }
}

/// Build the PreToolUse deny decision for a classified redirect, or `None` to
/// let the call through unchanged.
pub fn redirect_decision(tool_name: &str, tool_input: Option<&Value>) -> Option<Value> {
    let tool_name = tool_name.to_string();
    let tool_input = tool_input.cloned();
    decide_with_timeout(GATING_TIMEOUT, move || {
        let reason = classify(&tool_name, tool_input.as_ref())?;
        Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_redirects_todo_write() {
        let reason = classify("TodoWrite", None).unwrap();
        assert!(reason.contains("`item` MCP tool"));
    }

    #[test]
    fn classify_redirects_spec_path_write() {
        let input = json!({ "file_path": "docs/superpowers/specs/2026-07-13-foo.md" });
        let reason = classify("Write", Some(&input)).unwrap();
        assert!(reason.contains("artifact_publish"));
    }

    #[test]
    fn classify_redirects_spec_path_edit_on_windows_backslashes() {
        let input = json!({ "file_path": "docs\\superpowers\\specs\\foo.md" });
        assert!(classify("Edit", Some(&input)).is_some());
    }

    #[test]
    fn classify_ignores_non_spec_write() {
        let input = json!({ "file_path": "src/main.rs" });
        assert!(classify("Write", Some(&input)).is_none());
    }

    #[test]
    fn classify_ignores_unrelated_tools() {
        assert!(classify("Read", None).is_none());
        assert!(classify("Bash", None).is_none());
    }

    #[test]
    fn classify_write_with_no_path_falls_through() {
        assert!(classify("Write", None).is_none());
        assert!(classify("Write", Some(&json!({}))).is_none());
    }

    #[test]
    fn redirect_decision_builds_deny_shape_for_todo_write() {
        let decision = redirect_decision("TodoWrite", None).unwrap();
        assert_eq!(
            decision["hookSpecificOutput"]["hookEventName"],
            "PreToolUse"
        );
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            decision["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("item")
        );
    }

    #[test]
    fn redirect_decision_is_none_for_unmatched_tool() {
        assert!(redirect_decision("Grep", None).is_none());
    }

    #[test]
    fn decide_with_timeout_fails_open_on_slow_work() {
        let out = decide_with_timeout(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Some(json!({ "should": "never observe this" }))
        });
        assert!(
            out.is_none(),
            "a worker slower than the timeout must fail open to None"
        );
    }
}

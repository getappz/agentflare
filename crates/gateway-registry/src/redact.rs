//! Strips connection details, file paths, and credential-shaped text from
//! downstream error messages before they reach the LLM. A raw
//! `GatewayError::Upstream`/`Connection` message is whatever the downstream
//! server (or the OS, on a spawn failure) produced verbatim — that can
//! include a local file path, a connection string, or (if a downstream
//! server is misbehaving or compromised) a credential it echoed back.
//!
//! Same category of guard forgemax's ARCHITECTURE.md documents
//! ("Error Redaction Philosophy"); written fresh here, not ported — see
//! `error.rs`'s note on forgemax's FSL license. Deliberately preserves tool
//! names, "not found" messages, and validation errors (e.g. "field
//! 'pattern' is required") since those are exactly what an LLM needs to
//! self-correct; only strips what a caller can't act on anyway.

use regex::Regex;
use std::sync::LazyLock;

static URL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s'")]+"#).unwrap());
static IPV4_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b").unwrap());
static UNIX_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/(?:home|Users|etc|var|tmp|opt|usr|root)(?:/[\w.\-]+)+").unwrap());
static WINDOWS_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z]:\\[\w.\\ \-]+").unwrap());
static CREDENTIAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(bearer\s+\S+|(?:api[_-]?key|token|password|secret)\s*[:=]\s*\S+)").unwrap()
});
static STACK_FRAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(at\s+.+|Caused by:.*)$").unwrap());

/// Redact a downstream error message for LLM consumption. Order matters:
/// credentials first (a `token=...` fragment might otherwise get mangled by
/// the path/URL passes), then URLs, then paths, then bare IPs, then
/// stack-trace lines.
pub fn redact_error_for_llm(error: &str) -> String {
    let mut msg = error.to_string();
    msg = CREDENTIAL_RE.replace_all(&msg, "[redacted]").to_string();
    msg = URL_RE.replace_all(&msg, "[url]").to_string();
    msg = WINDOWS_PATH_RE.replace_all(&msg, "[path]").to_string();
    msg = UNIX_PATH_RE.replace_all(&msg, "[path]").to_string();
    msg = IPV4_RE.replace_all(&msg, "[addr]").to_string();
    msg = STACK_FRAME_RE.replace_all(&msg, "").to_string();
    msg.lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_http_and_https_urls() {
        let result = redact_error_for_llm("connect to http://internal.corp:9876/api failed");
        assert!(result.contains("[url]"), "{result}");
        assert!(!result.contains("internal.corp"), "{result}");
    }

    #[test]
    fn redacts_ipv4_with_port() {
        let result = redact_error_for_llm("connection refused: 127.0.0.1:5432");
        assert!(result.contains("[addr]"), "{result}");
        assert!(!result.contains("127.0.0.1"), "{result}");
    }

    #[test]
    fn redacts_unix_paths() {
        let result = redact_error_for_llm("no such file /home/shiva/.secrets/config.toml");
        assert!(result.contains("[path]"), "{result}");
        assert!(!result.contains("shiva"), "{result}");
    }

    #[test]
    fn redacts_windows_paths() {
        let result = redact_error_for_llm(r"spawn failed: C:\Users\shiva\.agentflare\gateway.toml not found");
        assert!(result.contains("[path]"), "{result}");
        assert!(!result.contains("shiva"), "{result}");
    }

    #[test]
    fn redacts_bearer_tokens_and_key_value_credentials() {
        let result = redact_error_for_llm("auth failed: Bearer sk-abc123XYZ, api_key=zzz999");
        assert!(!result.contains("sk-abc123XYZ"), "{result}");
        assert!(!result.contains("zzz999"), "{result}");
        assert!(result.contains("[redacted]"), "{result}");
    }

    #[test]
    fn strips_stack_trace_lines() {
        let msg = "call failed\n  at handler (index.js:42)\nCaused by: root cause\nreal message";
        let result = redact_error_for_llm(msg);
        assert!(!result.contains("index.js"), "{result}");
        assert!(!result.contains("Caused by"), "{result}");
        assert!(result.contains("real message"), "{result}");
    }

    #[test]
    fn preserves_tool_names_and_validation_errors() {
        let result = redact_error_for_llm("field 'pattern' is required for tool 'find_symbols'");
        assert_eq!(result, "field 'pattern' is required for tool 'find_symbols'");
    }

    #[test]
    fn preserves_not_found_messages() {
        let result = redact_error_for_llm("tool 'grep' not found on server 'narsil'");
        assert_eq!(result, "tool 'grep' not found on server 'narsil'");
    }
}

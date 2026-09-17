//! Redaction for anything the gate persists or broadcasts to a channel.
//!
//! Everything written to the store or sent as an approval card goes through
//! here first: home paths (username leakage), secret-shaped tokens
//! (API keys, bearer tokens, passwords), and known-sensitive JSON fields.
//! This is a first-class module, not a formatting afterthought — a leaked
//! path or token in a durable audit row or a Slack/Telegram card is exactly
//! the failure this gate exists to prevent.

use serde_json::{Map, Value};

/// Cap on a persisted/broadcast command string, inclusive of the ellipsis
/// marker — mirrors the 512-char cap used for execution error text so a
/// pathological command can't slowly fill the audit log or blow up a
/// channel message.
const MAX_COMMAND_LEN: usize = 512;

/// Known secret-token prefixes worth masking outright even without a
/// labelled `key=value` context (a bare Slack/OpenAI/GitHub-shaped token
/// pasted into a command argument).
const SECRET_PREFIXES: &[&str] = &[
    "sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "xox", "AKIA", "AIza",
];

/// Flag/env-var names whose value must never be persisted verbatim.
const SENSITIVE_FLAG_NAMES: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "token",
    "secret",
    "api_key",
    "apikey",
    "auth",
    "authorization",
    "access_key",
    "access_token",
    "private_key",
    "client_secret",
];

/// Produce a redacted, length-capped copy of a command string safe to
/// persist in the approval store or send to a channel.
pub fn redact_command(command: &str) -> String {
    let scrubbed = scrub_paths(command);
    let masked = mask_secrets(&scrubbed);
    cap(&masked)
}

fn cap(s: &str) -> String {
    if s.chars().count() <= MAX_COMMAND_LEN {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX_COMMAND_LEN - 1).collect();
    format!("{head}…")
}

/// Word-based secret masking. Not a full shell parser — operates on
/// whitespace-split tokens, which is sufficient for the `--flag value` /
/// `--flag=value` / `KEY=value` shapes secrets actually show up in.
fn mask_secrets(input: &str) -> String {
    let words: Vec<&str> = input.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut redact_next = false;
    for word in &words {
        if redact_next {
            out.push("[REDACTED]".to_string());
            redact_next = false;
            continue;
        }
        if let Some((name, value)) = word.split_once('=') {
            let bare_name = name.trim_start_matches('-');
            if is_sensitive_name(bare_name) {
                out.push(format!("{name}=[REDACTED]"));
                continue;
            }
            if !value.is_empty() && has_secret_prefix(value) {
                out.push(format!("{name}=[REDACTED]"));
                continue;
            }
        }
        let bare_flag = word.trim_start_matches('-');
        if !bare_flag.is_empty() && is_sensitive_name(bare_flag) {
            out.push((*word).to_string());
            redact_next = true;
            continue;
        }
        if has_secret_prefix(word) {
            out.push("[REDACTED]".to_string());
            continue;
        }
        out.push((*word).to_string());
    }
    out.join(" ")
}

fn is_sensitive_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SENSITIVE_FLAG_NAMES.iter().any(|s| *s == lower)
}

fn has_secret_prefix(word: &str) -> bool {
    // Strip common wrapping punctuation (quotes) before matching.
    let trimmed = word.trim_matches(|c| c == '"' || c == '\'' || c == ',');
    SECRET_PREFIXES.iter().any(|p| trimmed.starts_with(p)) && trimmed.len() >= 12
}

/// Strip absolute home paths so a redacted string cannot leak the user's
/// username. Handles Unix (`/Users/<name>/…`, `/home/<name>/…`) and Windows
/// (`C:\Users\<name>\…`) shapes.
pub fn scrub_paths(input: &str) -> String {
    if !input.contains("Users") && !input.contains("home") {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if let Some(prefix_len) = match_home_prefix(&input[i..]) {
            out.push_str("<HOME>");
            i += prefix_len;
            let rest = &input[i..];
            match rest.find(['/', '\\']) {
                Some(end) => i += end,
                None => i = input.len(),
            }
        } else {
            let ch = input[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn match_home_prefix(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let starts_with_ci = |needle: &str| -> bool {
        bytes.len() >= needle.len() && bytes[..needle.len()].eq_ignore_ascii_case(needle.as_bytes())
    };
    if starts_with_ci("/Users/") {
        return Some(7);
    }
    if starts_with_ci("/home/") {
        return Some(6);
    }
    if bytes.len() >= 9
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'\\'
        && bytes[3..9].eq_ignore_ascii_case(b"Users\\")
    {
        return Some(9);
    }
    None
}

/// JSON-field redaction for structured tool arguments, when a caller has
/// them (kept alongside the command-string path above so a future
/// structured-args gate call site doesn't need a second redaction module).
const SENSITIVE_JSON_KEYS: &[&str] = &[
    "body",
    "content",
    "message",
    "messages",
    "text",
    "note",
    "password",
    "token",
    "api_key",
    "secret",
    "authorization",
    "auth",
    "email",
    "phone",
    "address",
];

pub fn redact_json(value: &Value) -> Value {
    walk(value)
}

fn walk(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(walk_object(map)),
        Value::Array(items) => Value::Array(items.iter().map(walk).collect()),
        Value::String(s) => Value::String(scrub_paths(s)),
        other => other.clone(),
    }
}

fn walk_object(map: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::with_capacity(map.len());
    for (k, v) in map {
        if SENSITIVE_JSON_KEYS
            .iter()
            .any(|s| s.eq_ignore_ascii_case(k))
        {
            out.insert(k.clone(), redact_value(v));
        } else {
            out.insert(k.clone(), walk(v));
        }
    }
    out
}

fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            Value::String(format!("<redacted: string ({} chars)>", s.chars().count()))
        }
        Value::Array(items) => Value::String(format!("<redacted: array ({} items)>", items.len())),
        Value::Object(map) => Value::String(format!("<redacted: object ({} keys)>", map.len())),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unix_home_path_is_scrubbed() {
        let out = scrub_paths("/Users/oxoxdev/work/repo");
        assert!(!out.contains("oxoxdev"));
        assert!(out.contains("<HOME>"));
        assert!(out.ends_with("/work/repo"));
    }

    #[test]
    fn windows_home_path_is_scrubbed() {
        let out = scrub_paths("C:\\Users\\oxoxdev\\work\\repo");
        assert!(!out.contains("oxoxdev"));
        assert!(out.contains("<HOME>"));
    }

    #[test]
    fn linux_home_path_is_scrubbed() {
        let out = scrub_paths("/home/jane/project");
        assert!(!out.contains("jane"));
        assert!(out.contains("<HOME>"));
    }

    #[test]
    fn labelled_password_flag_is_masked() {
        let out = redact_command("mysql -u root --password=hunter2 -h db");
        assert!(!out.contains("hunter2"));
        assert!(out.contains("--password=[REDACTED]"));
    }

    #[test]
    fn separate_token_flag_and_value_are_masked() {
        let out = redact_command("curl -H \"Authorization\" --token abc123def456 http://x");
        assert!(!out.contains("abc123def456"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn known_secret_prefix_is_masked_even_without_a_labelled_flag() {
        let out = redact_command("echo sk-live-abcdef1234567890abcdef1234567890");
        assert!(!out.contains("sk-live-abcdef1234567890abcdef1234567890"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn safe_command_passes_through_unchanged() {
        assert_eq!(redact_command("git status"), "git status");
    }

    #[test]
    fn long_command_is_capped() {
        let long = "echo ".to_string() + &"x".repeat(1000);
        let out = redact_command(&long);
        assert_eq!(out.chars().count(), 512);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn json_sensitive_fields_are_redacted() {
        let args = json!({ "body": "hello", "action": "execute" });
        let red = redact_json(&args);
        assert_eq!(red["action"], json!("execute"));
        assert!(red["body"].as_str().unwrap().starts_with("<redacted"));
    }

    #[test]
    fn json_home_path_in_unredacted_field_is_scrubbed() {
        let args = json!({ "cwd": "/Users/alice/project" });
        let red = redact_json(&args);
        assert!(!red["cwd"].as_str().unwrap().contains("alice"));
    }
}

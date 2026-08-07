use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct ShortCircuitConfig {
    pub fast_prefix_detection: bool,
    pub enable_network_probe_mock: bool,
    pub enable_title_generation_skip: bool,
    pub enable_suggestion_mode_skip: bool,
    pub enable_filepath_extraction_mock: bool,
}

impl ShortCircuitConfig {
    pub fn from_env() -> Self {
        Self {
            fast_prefix_detection: env_bool("FAST_PREFIX_DETECTION", true),
            enable_network_probe_mock: env_bool("ENABLE_NETWORK_PROBE_MOCK", true),
            enable_title_generation_skip: env_bool("ENABLE_TITLE_GENERATION_SKIP", true),
            enable_suggestion_mode_skip: env_bool("ENABLE_SUGGESTION_MODE_SKIP", true),
            enable_filepath_extraction_mock: env_bool("ENABLE_FILEPATH_EXTRACTION_MOCK", true),
        }
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => v.eq_ignore_ascii_case("true") || v == "1",
        Err(_) => default,
    }
}

pub enum ShortCircuitOutcome {
    Match(Value),
    NoMatch,
}

pub fn try_short_circuit(body: &Value, config: &ShortCircuitConfig) -> ShortCircuitOutcome {
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if config.enable_network_probe_mock && is_quota_check(body) {
        eprintln!("shortcircuit: quota probe mock");
        return ShortCircuitOutcome::Match(build_message(model, "Quota check passed.", 10, 5));
    }

    if config.fast_prefix_detection {
        if let Some(prefix) = detect_prefix(body) {
            let mock_prefix = extract_command_prefix(&prefix);
            eprintln!("shortcircuit: prefix detection -> {mock_prefix}");
            return ShortCircuitOutcome::Match(build_message(model, &mock_prefix, 100, 5));
        }
    }

    if config.enable_title_generation_skip && is_title_generation(body) {
        eprintln!("shortcircuit: title generation skip");
        return ShortCircuitOutcome::Match(build_message(model, "Conversation", 100, 5));
    }

    if config.enable_suggestion_mode_skip && is_suggestion_mode(body) {
        eprintln!("shortcircuit: suggestion mode skip");
        return ShortCircuitOutcome::Match(build_message(model, "", 100, 1));
    }

    if config.enable_filepath_extraction_mock {
        if let Some((cmd, output)) = detect_filepath_extraction(body) {
            let filepaths = extract_filepaths(&cmd, &output);
            eprintln!("shortcircuit: filepath extraction mock");
            return ShortCircuitOutcome::Match(build_message(model, &filepaths, 100, 10));
        }
    }

    ShortCircuitOutcome::NoMatch
}

fn is_quota_check(body: &Value) -> bool {
    if body.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(0) != 1 {
        return false;
    }
    let msgs = match body.get("messages").and_then(|v| v.as_array()) {
        Some(m) => m,
        None => return false,
    };
    if msgs.len() != 1 {
        return false;
    }
    if msgs[0].get("role").and_then(|v| v.as_str()).unwrap_or("") != "user" {
        return false;
    }
    let text = extract_text(&msgs[0]);
    text.to_lowercase().contains("quota")
}

fn detect_prefix(body: &Value) -> Option<String> {
    let msgs = body.get("messages").and_then(|v| v.as_array())?;
    if msgs.len() != 1 {
        return None;
    }
    if msgs[0].get("role").and_then(|v| v.as_str()).unwrap_or("") != "user" {
        return None;
    }
    let content = extract_text(&msgs[0]);
    if !content.contains("<policy_spec>") || !content.contains("Command:") {
        return None;
    }
    let cmd_start = content.rfind("Command:")? + "Command:".len();
    Some(content[cmd_start..].trim().to_string())
}

fn is_title_generation(body: &Value) -> bool {
    let system = match body.get("system") {
        Some(s) => system_text(s),
        None => return false,
    };
    let sys_lower = system.to_lowercase();
    if !sys_lower.contains("new conversation topic") || !sys_lower.contains("title") {
        return false;
    }
    if body
        .get("tools")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        return false;
    }
    true
}

fn is_suggestion_mode(body: &Value) -> bool {
    let msgs = match body.get("messages").and_then(|v| v.as_array()) {
        Some(m) => m,
        None => return false,
    };
    for msg in msgs {
        if msg.get("role").and_then(|v| v.as_str()).unwrap_or("") == "user" {
            let text = extract_text(msg);
            if text.contains("[SUGGESTION MODE:") {
                return true;
            }
        }
    }
    false
}

fn detect_filepath_extraction(body: &Value) -> Option<(String, String)> {
    let msgs = body.get("messages").and_then(|v| v.as_array())?;
    if msgs.len() != 1 {
        return None;
    }
    if msgs[0].get("role").and_then(|v| v.as_str()).unwrap_or("") != "user" {
        return None;
    }
    if body
        .get("tools")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        return None;
    }
    let content = extract_text(&msgs[0]);
    if !content.contains("Command:") || !content.contains("Output:") {
        return None;
    }

    let user_has_filepaths = content.to_lowercase().contains("filepaths");

    let system_has_extract = body
        .get("system")
        .map(|s| {
            let t = system_text(s).to_lowercase();
            t.contains("extract any file paths") || t.contains("file paths that this command")
        })
        .unwrap_or(false);

    if !user_has_filepaths && !system_has_extract {
        return None;
    }

    let cmd_start = content.find("Command:")? + "Command:".len();
    let output_marker = content[cmd_start..].find("Output:")?;
    let abs_output_start = cmd_start + output_marker;

    let command = content[cmd_start..abs_output_start].trim().to_string();
    let mut output = content[abs_output_start + "Output:".len()..]
        .trim()
        .to_string();

    for marker in &["<", "\n\n"] {
        if let Some(pos) = output.find(marker) {
            output = output[..pos].trim().to_string();
        }
    }

    Some((command, output))
}

/// Non-streaming Anthropic Messages response, matching the shape the CLI
/// expects for these internal bookkeeping calls — they are not sent with
/// `stream: true` upstream, so answering with an SSE body (as the rest of
/// this proxy always does) breaks the caller's JSON parse.
fn build_message(model: &str, text: &str, input_tokens: u64, output_tokens: u64) -> Value {
    json!({
        "id": new_id(),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": [{ "type": "text", "text": text }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": input_tokens, "output_tokens": output_tokens }
    })
}

fn extract_command_prefix(command: &str) -> String {
    if command.contains('`') || command.contains("$(") {
        return "command_injection_detected".into();
    }

    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return "none".into();
    }

    let mut cmd_start = 0;
    for part in &parts {
        if part.contains('=') && !part.starts_with('-') {
            cmd_start += 1;
        } else {
            break;
        }
    }

    let cmd_parts: &[&str] = &parts[cmd_start..];
    if cmd_parts.is_empty() {
        return "none".into();
    }

    let first_word = cmd_parts[0];
    let two_word_commands = [
        "git", "npm", "docker", "kubectl", "cargo", "go", "pip", "yarn",
    ];

    if two_word_commands.contains(&first_word) && cmd_parts.len() > 1 {
        let second = cmd_parts[1];
        if !second.starts_with('-') {
            return format!("{first_word} {second}");
        }
        return first_word.to_string();
    }

    first_word.to_string()
}

/// True for shell redirection/control-operator tokens (`>`, `>>`, `<`,
/// `2>`, `2>&1`, `&>`, `|`, `||`, `&&`, `;`) that a naive whitespace split
/// leaves as their own token (e.g. `2>/dev/null`) or attached to a target
/// (e.g. `>out.txt`) -- neither is a filepath the command reads/writes by
/// name, so both must be excluded rather than mistaken for one.
fn is_shell_operator_token(part: &str) -> bool {
    let trimmed = part.trim_start_matches(|c: char| c.is_ascii_digit());
    matches!(trimmed.chars().next(), Some('>' | '<' | '&' | '|' | ';'))
}

/// True when `part` is a redirection operator with no target fused onto it
/// (e.g. `>`, `2>`, `>>`) -- the shell takes the *next* whitespace-separated
/// token as the redirect target, so that token must also be dropped rather
/// than kept as a filepath the command reads.
fn is_bare_redirect_operator(part: &str) -> bool {
    let trimmed = part.trim_start_matches(|c: char| c.is_ascii_digit());
    matches!(trimmed, ">" | ">>" | "<" | "<<" | "&>" | "&>>")
}

fn extract_filepaths(command: &str, _output: &str) -> String {
    let listing_commands = [
        "ls", "dir", "find", "tree", "pwd", "cd", "mkdir", "rmdir", "rm",
    ];
    let reading_commands = ["cat", "head", "tail", "less", "more", "bat", "type"];

    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return "<filepaths>\n</filepaths>".into();
    }

    let bin = parts[0]
        .split(&['/', '\\'][..])
        .next_back()
        .unwrap_or(parts[0]);
    let base_cmd = bin.to_lowercase();

    if listing_commands.contains(&base_cmd.as_str()) {
        return "<filepaths>\n</filepaths>".into();
    }

    if reading_commands.contains(&base_cmd.as_str()) {
        let mut filepaths: Vec<&str> = Vec::new();
        let mut skip_next = false;
        for part in &parts[1..] {
            if skip_next {
                skip_next = false;
                continue;
            }
            if part.starts_with('-') {
                continue;
            }
            if is_shell_operator_token(part) {
                if is_bare_redirect_operator(part) {
                    skip_next = true;
                }
                continue;
            }
            filepaths.push(part);
        }
        if filepaths.is_empty() {
            return "<filepaths>\n</filepaths>".into();
        }
        return format!("<filepaths>\n{}\n</filepaths>", filepaths.join("\n"));
    }

    if base_cmd == "grep" {
        let flags_with_args = ["-e", "-f", "-m", "-A", "-B", "-C"];
        let mut pattern_via_flag = false;
        let mut positional: Vec<&str> = Vec::new();
        let mut skip_next = false;

        for part in &parts[1..] {
            if skip_next {
                skip_next = false;
                continue;
            }
            if part.starts_with('-') {
                if flags_with_args.contains(part) {
                    if *part == "-e" || *part == "-f" {
                        pattern_via_flag = true;
                    }
                    skip_next = true;
                }
                continue;
            }
            if is_shell_operator_token(part) {
                if is_bare_redirect_operator(part) {
                    skip_next = true;
                }
                continue;
            }
            positional.push(part);
        }

        let filepaths = if pattern_via_flag {
            &positional[..]
        } else if positional.len() > 1 {
            &positional[1..]
        } else {
            &[]
        };

        if filepaths.is_empty() {
            return "<filepaths>\n</filepaths>".into();
        }
        return format!("<filepaths>\n{}\n</filepaths>", filepaths.join("\n"));
    }

    "<filepaths>\n</filepaths>".into()
}

fn system_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn extract_text(msg: &Value) -> String {
    let content = match msg.get("content") {
        Some(c) => c,
        None => return String::new(),
    };
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(|v| v.as_str()) {
                Some("text") => b.get("text").and_then(|v| v.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn new_id() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("msg_{ts}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_quota_check_detects() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "check quota status"}]
        });
        assert!(is_quota_check(&body));
    }

    #[test]
    fn test_quota_check_not_quota() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "hello world"}]
        });
        assert!(!is_quota_check(&body));
    }

    #[test]
    fn test_quota_check_max_tokens_not_one() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "check quota"}]
        });
        assert!(!is_quota_check(&body));
    }

    #[test]
    fn test_detect_prefix_finds_command() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "<policy_spec>something</policy_spec>\nCommand: git status"}]
        });
        let result = detect_prefix(&body);
        assert_eq!(result.as_deref(), Some("git status"));
    }

    #[test]
    fn test_detect_prefix_no_match() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hello world"}]
        });
        assert!(detect_prefix(&body).is_none());
    }

    #[test]
    fn test_detect_prefix_requires_single_message() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "second"}
            ]
        });
        assert!(detect_prefix(&body).is_none());
    }

    #[test]
    fn test_title_generation_detects() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You help generate a new conversation topic and title.",
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(is_title_generation(&body));
    }

    #[test]
    fn test_title_generation_no_system() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert!(!is_title_generation(&body));
    }

    #[test]
    fn test_title_generation_with_tools() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You help generate a new conversation topic and title.",
            "messages": [{"role": "user", "content": "hello"}],
            "tools": [{"name": "test", "input_schema": {"type": "object"}}]
        });
        assert!(!is_title_generation(&body));
    }

    #[test]
    fn test_suggestion_mode_detects() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "something [SUGGESTION MODE: on]"}]
        });
        assert!(is_suggestion_mode(&body));
    }

    #[test]
    fn test_suggestion_mode_no_match() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "regular message"}]
        });
        assert!(!is_suggestion_mode(&body));
    }

    #[test]
    fn test_filepath_extraction_detects() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Command: cat foo.txt\nOutput: contents\n<filepaths>"}],
            "system": "extract any file paths"
        });
        let result = detect_filepath_extraction(&body);
        assert!(result.is_some());
        let (cmd, _) = result.unwrap();
        assert_eq!(cmd, "cat foo.txt");
    }

    #[test]
    fn test_filepath_extraction_no_match_no_filepath_hints() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Command: ls\nOutput: file1.txt"}]
        });
        assert!(detect_filepath_extraction(&body).is_none());
    }

    #[test]
    fn test_short_circuit_all_disabled() {
        let config = ShortCircuitConfig {
            fast_prefix_detection: false,
            enable_network_probe_mock: false,
            enable_title_generation_skip: false,
            enable_suggestion_mode_skip: false,
            enable_filepath_extraction_mock: false,
        };
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "check quota status"}]
        });
        match try_short_circuit(&body, &config) {
            ShortCircuitOutcome::NoMatch => {}
            _ => panic!("expected NoMatch when all disabled"),
        }
    }

    #[test]
    fn test_short_circuit_quota_mock_enabled() {
        let config = ShortCircuitConfig {
            fast_prefix_detection: false,
            enable_network_probe_mock: true,
            enable_title_generation_skip: false,
            enable_suggestion_mode_skip: false,
            enable_filepath_extraction_mock: false,
        };
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "check quota status"}]
        });
        match try_short_circuit(&body, &config) {
            ShortCircuitOutcome::Match(msg) => {
                assert_eq!(msg["type"], "message");
                assert_eq!(msg["role"], "assistant");
                assert_eq!(msg["content"][0]["type"], "text");
                assert_eq!(msg["content"][0]["text"], "Quota check passed.");
                assert_eq!(msg["stop_reason"], "end_turn");
                assert_eq!(msg["usage"]["input_tokens"], 10);
                assert_eq!(msg["usage"]["output_tokens"], 5);
            }
            _ => panic!("expected Match"),
        }
    }

    fn only(flag: &str) -> ShortCircuitConfig {
        ShortCircuitConfig {
            fast_prefix_detection: flag == "prefix",
            enable_network_probe_mock: flag == "quota",
            enable_title_generation_skip: flag == "title",
            enable_suggestion_mode_skip: flag == "suggestion",
            enable_filepath_extraction_mock: flag == "filepath",
        }
    }

    #[test]
    fn test_short_circuit_prefix_detection_enabled() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "<policy_spec>x</policy_spec>\nCommand: git status"}]
        });
        match try_short_circuit(&body, &only("prefix")) {
            ShortCircuitOutcome::Match(msg) => {
                assert_eq!(msg["content"][0]["text"], "git status");
                assert_eq!(msg["usage"]["input_tokens"], 100);
                assert_eq!(msg["usage"]["output_tokens"], 5);
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_short_circuit_title_generation_enabled() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You help generate a new conversation topic and title.",
            "messages": [{"role": "user", "content": "hello"}]
        });
        match try_short_circuit(&body, &only("title")) {
            ShortCircuitOutcome::Match(msg) => {
                assert_eq!(msg["content"][0]["text"], "Conversation");
                assert_eq!(msg["usage"]["output_tokens"], 5);
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_short_circuit_suggestion_mode_enabled() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "something [SUGGESTION MODE: on]"}]
        });
        match try_short_circuit(&body, &only("suggestion")) {
            ShortCircuitOutcome::Match(msg) => {
                assert_eq!(msg["content"][0]["text"], "");
                assert_eq!(msg["usage"]["output_tokens"], 1);
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_short_circuit_filepath_extraction_enabled() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Command: cat foo.txt\nOutput: contents\n<filepaths>"}],
            "system": "extract any file paths"
        });
        match try_short_circuit(&body, &only("filepath")) {
            ShortCircuitOutcome::Match(msg) => {
                assert_eq!(
                    msg["content"][0]["text"],
                    "<filepaths>\nfoo.txt\n</filepaths>"
                );
                assert_eq!(msg["usage"]["output_tokens"], 10);
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_extract_command_prefix_basic() {
        assert_eq!(extract_command_prefix("git status"), "git status");
        assert_eq!(extract_command_prefix("npm install axios"), "npm install");
        assert_eq!(extract_command_prefix("ls -la"), "ls");
        assert_eq!(extract_command_prefix(""), "none");
    }

    #[test]
    fn test_extract_command_prefix_injection() {
        assert_eq!(
            extract_command_prefix("echo `cat /etc/passwd`"),
            "command_injection_detected"
        );
        assert_eq!(
            extract_command_prefix("echo $(id)"),
            "command_injection_detected"
        );
    }

    #[test]
    fn test_extract_command_prefix_env() {
        let r = extract_command_prefix("VAR=value git push");
        assert!(r.contains("git"));
    }

    #[test]
    fn test_extract_filepaths_reading_command() {
        let result = extract_filepaths("cat src/main.rs", "some content");
        assert!(result.contains("src/main.rs"));
    }

    #[test]
    fn test_extract_filepaths_listing_command() {
        let result = extract_filepaths("ls -la", "file1 file2");
        assert_eq!(result, "<filepaths>\n</filepaths>");
    }

    #[test]
    fn test_extract_filepaths_grep() {
        let result = extract_filepaths("grep -e pattern src/main.rs", "matches");
        assert!(result.contains("src/main.rs"));
    }

    #[test]
    fn test_extract_filepaths_grep_file_pattern() {
        let result = extract_filepaths("grep pattern src/main.rs tests/test.rs", "matches");
        assert!(result.contains("tests/test.rs"));
    }

    #[test]
    fn test_extract_filepaths_ignores_stderr_redirect() {
        let result = extract_filepaths("tail -5 src/main.rs 2>/dev/null", "some content");
        assert_eq!(result, "<filepaths>\nsrc/main.rs\n</filepaths>");
    }

    #[test]
    fn test_extract_filepaths_ignores_stdout_redirect() {
        let result = extract_filepaths("cat src/main.rs > out.txt", "some content");
        assert_eq!(result, "<filepaths>\nsrc/main.rs\n</filepaths>");
    }

    #[test]
    fn test_extract_filepaths_grep_ignores_redirect() {
        let result = extract_filepaths("grep pattern src/main.rs 2>/dev/null", "matches");
        assert_eq!(result, "<filepaths>\nsrc/main.rs\n</filepaths>");
    }

    #[test]
    fn test_extract_filepaths_empty() {
        let result = extract_filepaths("", "");
        assert_eq!(result, "<filepaths>\n</filepaths>");
    }

    #[test]
    fn test_build_message_shape() {
        let msg = build_message("claude-sonnet-4-20250514", "hi", 7, 3);
        assert_eq!(msg["type"], "message");
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["model"], "claude-sonnet-4-20250514");
        assert_eq!(msg["stop_sequence"], Value::Null);
        assert_eq!(msg["usage"]["input_tokens"], 7);
        assert_eq!(msg["usage"]["output_tokens"], 3);
    }
}

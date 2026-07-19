use regex::Regex;

/// Attempt to extract a structured tool call from free-tier model output
/// that does not natively support function calling.
#[derive(Debug)]
pub struct HeuristicToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub id: String,
}

/// Try to parse a JSON tool-call block from text. Free-tier models often
/// emit tool calls as:
///   - `<invoke_meal name="tool_name">{"arg": "val"}</invoke_meal>`
///   - JSON in a code fence
///   - `Tool: get_weather({"city": "London"})`
///   - Named JSON block `{"name": "tool", "arguments": {...}}`
pub fn try_extract_tool_call(text: &str) -> Option<HeuristicToolCall> {
    if let Some(call) = extract_invoke_meal(text) {
        return Some(call);
    }
    if let Some(call) = extract_json_tool_block(text) {
        return Some(call);
    }
    if let Some(call) = extract_code_fence_json(text) {
        return Some(call);
    }
    None
}

fn extract_invoke_meal(text: &str) -> Option<HeuristicToolCall> {
    let re =
        Regex::new(r#"(?s)<invoke_meal\s+name="([^"]+)"\s*>\s*(\{.*?\})\s*</invoke_meal>"#).ok()?;
    let cap = re.captures(text)?;
    let name = cap.get(1)?.as_str().to_string();
    let args_str = cap.get(2)?.as_str();
    let args: serde_json::Value = serde_json::from_str(args_str).ok()?;
    let id = format!("call_{}", nanoid::nanoid!());
    Some(HeuristicToolCall { name, args, id })
}

fn extract_code_fence_json(text: &str) -> Option<HeuristicToolCall> {
    let re =
        Regex::new(r#"(?s)```(?:json)?\s*\n?(\{.*?"name"\s*:\s*"[^"]+".*?\})\s*\n?```"#).ok()?;
    let cap = re.captures(text)?;
    let json_str = cap.get(1)?.as_str();
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let name = parsed.get("name")?.as_str()?.to_string();
    let args = parsed
        .get("arguments")
        .or_else(|| parsed.get("args"))?
        .clone();
    let id = format!("call_{}", nanoid::nanoid!());
    Some(HeuristicToolCall { name, args, id })
}

fn extract_json_tool_block(text: &str) -> Option<HeuristicToolCall> {
    let re =
        Regex::new(r#"\{(?:\s*)"name"\s*:\s*"(?:[^"\\]|\\.)*"\s*,\s*"arguments"\s*:\s*(\{|\[)"#)
            .ok()?;
    let m = re.find(text)?;
    let block = &text[m.start()..];
    let end = find_balanced_brace(block)?;
    let json_str = &block[..=end];
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let name = parsed.get("name")?.as_str()?.to_string();
    let args = parsed.get("arguments")?.clone();
    let id = format!("call_{}", nanoid::nanoid!());
    Some(HeuristicToolCall { name, args, id })
}

fn find_balanced_brace(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut started = false;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in s.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                depth += 1;
                started = true;
            }
            '}' => {
                depth -= 1;
                if started && depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Check if output needs heuristic tool parsing (free-tier models often
/// lack native function calling).
pub fn needs_heuristic_tools(model: &str) -> bool {
    let model = model.to_lowercase();
    model.contains("llama")
        || model.contains("deepseek")
        || model.contains("qwen")
        || model.contains("mistral")
        || model.contains("mixtral")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_invoke_meal() {
        let text = r#"I'll check the weather. <invoke_meal name="get_weather">{"city": "London"}</invoke_meal>"#;
        let call = try_extract_tool_call(text).unwrap();
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.args["city"], "London");
    }

    #[test]
    fn test_extract_code_fence_tool() {
        let text = r#"Here's the result:
```json
{"name": "search_db", "arguments": {"query": "SELECT * FROM users"}}
```"#;
        let call = try_extract_tool_call(text).unwrap();
        assert_eq!(call.name, "search_db");
        assert_eq!(call.args["query"], "SELECT * FROM users");
    }

    #[test]
    fn test_extract_code_fence_tool_multiline_json() {
        let text = "Here's the result:\n```json\n{\n  \"name\": \"search_db\",\n  \"arguments\": {\n    \"query\": \"SELECT * FROM users\"\n  }\n}\n```";
        let call = try_extract_tool_call(text).unwrap();
        assert_eq!(call.name, "search_db");
        assert_eq!(call.args["query"], "SELECT * FROM users");
    }

    #[test]
    fn test_extract_bare_json_tool_block() {
        let text =
            r#"I'll use a tool: {"name": "get_weather", "arguments": {"city": "London"}} done."#;
        let call = try_extract_tool_call(text).unwrap();
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.args["city"], "London");
    }

    #[test]
    fn test_no_tool_call() {
        assert!(try_extract_tool_call("Just a regular response.").is_none());
    }

    #[test]
    fn test_needs_heuristic_tools_case_insensitive() {
        assert!(needs_heuristic_tools("Meta-Llama-3-70B"));
        assert!(needs_heuristic_tools("DeepSeek-V3"));
    }
}

//! Native Gemini upstream. Translates Anthropic's Messages API shape
//! directly to Gemini's `generateContent`/`streamGenerateContent` shape.
//!
//! Scoped to text + tool_calls only (no image/audio/video/file parts) —
//! flare-proxy's traffic is Claude Code CLI chat/tool-use, not multimodal
//! generation.

use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Build a Gemini `generateContent`-shaped request body from an incoming
/// Anthropic Messages request.
pub fn build_request_body(anthropic_body: &Value) -> Value {
    let mut contents = Vec::new();
    // tool_use id -> tool name, so a later tool_result block (which only
    // carries the id) can be translated to Gemini's named functionResponse.
    let mut tool_names: HashMap<String, String> = HashMap::new();

    if let Some(messages) = anthropic_body.get("messages").and_then(Value::as_array) {
        for msg in messages {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = msg.get("content").unwrap_or(&Value::Null);
            match role {
                "assistant" => {
                    let parts = assistant_parts(content, &mut tool_names);
                    if !parts.is_empty() {
                        contents.push(json!({"role": "model", "parts": parts}));
                    }
                }
                _ => {
                    let parts = user_parts(content, &tool_names);
                    if !parts.is_empty() {
                        contents.push(json!({"role": "user", "parts": parts}));
                    }
                }
            }
        }
    }
    if contents.is_empty() {
        contents.push(json!({"role": "user", "parts": [{"text": " "}]}));
    }

    let mut body = json!({ "contents": contents });

    if let Some(system_text) = system_instruction_text(anthropic_body.get("system")) {
        body["system_instruction"] = json!({ "parts": [{ "text": system_text }] });
    }

    let mut generation_config = Map::new();
    if let Some(temp) = anthropic_body.get("temperature") {
        generation_config.insert("temperature".into(), temp.clone());
    }
    if let Some(max_tokens) = anthropic_body.get("max_tokens") {
        generation_config.insert("maxOutputTokens".into(), max_tokens.clone());
    }
    if !generation_config.is_empty() {
        body["generationConfig"] = Value::Object(generation_config);
    }

    if let Some(tools) = anthropic_body.get("tools").and_then(Value::as_array) {
        let declarations: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?;
                Some(json!({
                    "name": name,
                    "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                    "parameters": t.get("input_schema").cloned().unwrap_or_else(|| json!({"type": "object"})),
                }))
            })
            .collect();
        if !declarations.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": declarations }]);
        }
    }

    body
}

fn system_instruction_text(system: Option<&Value>) -> Option<String> {
    let system = system?;
    let text = match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

fn assistant_parts(content: &Value, tool_names: &mut HashMap<String, String>) -> Vec<Value> {
    let mut parts = Vec::new();
    let Some(blocks) = content.as_array() else {
        if let Some(text) = content.as_str() {
            if !text.is_empty() {
                parts.push(json!({"text": text}));
            }
        }
        return parts;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(json!({"text": text}));
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                if !id.is_empty() {
                    tool_names.insert(id.to_string(), name.to_string());
                }
                let args = block.get("input").cloned().unwrap_or_else(|| json!({}));
                parts.push(json!({"functionCall": {"name": name, "args": args}}));
            }
            _ => {}
        }
    }
    parts
}

fn user_parts(content: &Value, tool_names: &HashMap<String, String>) -> Vec<Value> {
    let mut parts = Vec::new();
    let Some(blocks) = content.as_array() else {
        if let Some(text) = content.as_str() {
            if !text.is_empty() {
                parts.push(json!({"text": text}));
            }
        }
        return parts;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(json!({"text": text}));
                }
            }
            Some("tool_result") => {
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = tool_names
                    .get(tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| tool_use_id.to_string());
                let response = tool_result_response(block.get("content"));
                parts.push(json!({"functionResponse": {"name": name, "response": response}}));
            }
            _ => {}
        }
    }
    parts
}

fn tool_result_response(content: Option<&Value>) -> Value {
    let text = match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return v;
        }
    }
    json!({ "content": text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_plain_text_turn() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let body = build_request_body(&anthropic);
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Hello");
    }

    #[test]
    fn carries_system_instruction_and_generation_config() {
        let anthropic = json!({
            "system": "Be terse.",
            "max_tokens": 512,
            "temperature": 0.4,
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let body = build_request_body(&anthropic);
        assert_eq!(body["system_instruction"]["parts"][0]["text"], "Be terse.");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 512);
        assert_eq!(body["generationConfig"]["temperature"], 0.4);
    }

    #[test]
    fn translates_tool_use_and_tool_result_round_trip() {
        let anthropic = json!({
            "messages": [
                {"role": "user", "content": "What's the weather in Paris?"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "{\"tempC\": 18}"}
                ]}
            ],
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
            }]
        });
        let body = build_request_body(&anthropic);
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "get_weather"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"]["tempC"],
            18
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );
    }
}

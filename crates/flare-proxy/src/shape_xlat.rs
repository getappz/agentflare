use serde_json::{json, Value};

pub fn messages_to_chat(anthropic: &Value) -> Option<Value> {
    let model = anthropic.get("model")?.as_str()?;
    let mut messages = Vec::new();

    if let Some(s) = anthropic.get("system") {
        messages.push(json!({
            "role": "system",
            "content": system_text(s)
        }));
    }

    let anthropic_messages = anthropic.get("messages")?.as_array()?;
    for msg in anthropic_messages {
        let role = msg.get("role")?.as_str()?;
        match role {
            "user" => {
                let content = msg.get("content")?;
                messages.push(json!({
                    "role": "user",
                    "content": translate_user_content(content)
                }));
            }
            "assistant" => {
                let content = msg.get("content")?;
                messages.push(json!({
                    "role": "assistant",
                    "content": translate_assistant_content(content)
                }));
            }
            _ => {}
        }
    }

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });

    if let Some(max_tokens) = anthropic.get("max_tokens") {
        body["max_tokens"] = max_tokens.clone();
    }
    if let Some(temp) = anthropic.get("temperature") {
        body["temperature"] = temp.clone();
    }
    if let Some(stop) = anthropic.get("stop_sequences") {
        body["stop"] = stop.clone();
    }
    if let Some(tc) = anthropic.get("tool_choice") {
        body["tool_choice"] = translate_tool_choice(tc);
    }
    if let Some(tools) = anthropic.get("tools") {
        body["tools"] = translate_tools(tools);
    }

    Some(body)
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

fn translate_user_content(content: &Value) -> Value {
    match content {
        Value::String(s) => json!(s),
        Value::Array(blocks) => {
            let parts: Vec<Value> = blocks
                .iter()
                .filter_map(|block| {
                    let type_ = block.get("type")?.as_str()?;
                    match type_ {
                        "text" => {
                            let text = block.get("text")?.as_str()?;
                            Some(json!({ "type": "text", "text": text }))
                        }
                        "image" => {
                            let source = block.get("source")?;
                            let media_type =
                                source.get("media_type")?.as_str().unwrap_or("image/png");
                            let data = source.get("data")?.as_str()?;
                            Some(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data)
                                }
                            }))
                        }
                        "tool_result" => {
                            let tool_use_id = block.get("tool_use_id")?.as_str()?;
                            let content_val = block.get("content")?;
                            let text = match content_val {
                                Value::String(s) => s.clone(),
                                Value::Array(arr) => arr
                                    .iter()
                                    .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                _ => String::new(),
                            };
                            Some(json!({
                                "type": "text",
                                "text": format!("[tool_result id={}]\n{}", tool_use_id, text)
                            }))
                        }
                        _ => None,
                    }
                })
                .collect();
            if parts.len() == 1 {
                parts.into_iter().next().unwrap()
            } else {
                json!(parts)
            }
        }
        _ => json!(""),
    }
}

fn translate_assistant_content(content: &Value) -> Value {
    match content {
        Value::String(s) => json!(s),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                let type_ = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match type_ {
                    "text" => parts.push(block["text"].as_str().unwrap_or("").to_string()),
                    "tool_use" => {
                        let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let input = block.get("input").unwrap_or(&Value::Null);
                        parts.push(format!(
                            "<invoke_meal name=\"{}\">\n{}</invoke_meal>",
                            name,
                            serde_json::to_string(input).unwrap_or_default()
                        ));
                    }
                    _ => {}
                }
            }
            json!(parts.join(""))
        }
        _ => json!(""),
    }
}

fn translate_tool_choice(tc: &Value) -> Value {
    let type_ = tc.get("type").and_then(|v| v.as_str()).unwrap_or("auto");
    match type_ {
        "any" => json!({ "type": "required" }),
        "tool" => {
            let name = tc.get("name").and_then(|v| v.as_str()).unwrap_or("");
            json!({ "type": "function", "function": { "name": name } })
        }
        _ => json!({ "type": type_ }),
    }
}

fn translate_tools(tools: &Value) -> Value {
    let arr = tools.as_array().map_or(vec![], |tools| {
        tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?;
                let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let input_schema = t.get("input_schema")?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": desc,
                        "parameters": input_schema
                    }
                }))
            })
            .collect()
    });
    json!(arr)
}

pub fn chat_to_messages(openai: &Value) -> Option<Value> {
    let choice = openai.get("choices")?.as_array()?.first()?;
    let delta = choice.get("message").or_else(|| choice.get("delta"))?;

    let mut content = Vec::new();

    if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({
                "type": "text",
                "text": text
            }));
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            if let (Some(name), Some(arguments)) = (
                tc.pointer("/function/name").and_then(|v| v.as_str()),
                tc.pointer("/function/arguments").and_then(|v| v.as_str()),
            ) {
                content.push(json!({
                    "type": "tool_use",
                    "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "name": name,
                    "input": serde_json::from_str::<Value>(arguments).unwrap_or(json!({}))
                }));
            }
        }
    }

    let stop_reason = choice
        .get("finish_reason")
        .and_then(|v| v.as_str())
        .map(|r| match r {
            "stop" => "end_turn",
            "length" => "max_tokens",
            "tool_calls" => "tool_use",
            _ => "end_turn",
        })
        .unwrap_or("end_turn");

    let mut resp = json!({
        "id": openai.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        "type": "message",
        "role": "assistant",
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "model": openai.get("model").and_then(|v| v.as_str()).unwrap_or(""),
        "usage": {
            "input_tokens": openai.pointer("/usage/prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            "output_tokens": openai.pointer("/usage/completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
        }
    });

    if let Some(ct) = openai.pointer("/usage/cache_creation_input_tokens") {
        resp["usage"]["cache_creation_input_tokens"] = ct.clone();
    }
    if let Some(cr) = openai.pointer("/usage/cache_read_input_tokens") {
        resp["usage"]["cache_read_input_tokens"] = cr.clone();
    }

    Some(resp)
}

pub fn error_to_anthropic(openai: &Value) -> Value {
    let msg = openai
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": msg
        }
    })
}

// ── Stream translation ──

pub fn openai_chunk_to_anthropic_sse(chunk: &Value, buffer: &mut AnthropicStreamBuffer) -> Vec<u8> {
    let mut out = Vec::new();

    let choices = match chunk.get("choices").and_then(|v| v.as_array()) {
        Some(c) => c,
        None => return out,
    };

    let delta = match choices.first().and_then(|c| c.get("delta")) {
        Some(d) => d,
        None => return out,
    };

    if !buffer.started {
        buffer.started = true;
        buffer.open_indices.insert(0);
        buffer.next_index = 1;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let msg_id = format!("msg_{}", ts);
        buffer.message_id = Some(msg_id.clone());
        let block_id = format!("cb_{}", ts);
        buffer.block_id = Some(block_id.clone());

        emit_event(
            &mut out,
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "id": msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": chunk.get("model"),
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": chunk.pointer("/usage/prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
                        "output_tokens": 0
                    }
                }
            }),
        );
        emit_event(
            &mut out,
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "text",
                    "text": ""
                }
            }),
        );
        emit_event(&mut out, "ping", &json!({ "type": "ping" }));
    }

    if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            emit_event(
                &mut out,
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "text_delta",
                        "text": text
                    }
                }),
            );
        }
    }

    if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let openai_idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let anth_idx = *buffer.tool_index_map.entry(openai_idx).or_insert_with(|| {
                let i = buffer.next_index;
                buffer.next_index += 1;
                i
            });
            let newly_opened = buffer.open_indices.insert(anth_idx);

            if newly_opened {
                if let Some(name) = tc.pointer("/function/name").and_then(|v| v.as_str()) {
                    let tc_id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    emit_event(
                        &mut out,
                        "content_block_start",
                        &json!({
                            "type": "content_block_start",
                            "index": anth_idx,
                            "content_block": {
                                "type": "tool_use",
                                "id": tc_id,
                                "name": name,
                                "input": {}
                            }
                        }),
                    );
                }
            }
            if let Some(args) = tc.pointer("/function/arguments").and_then(|v| v.as_str()) {
                if !args.is_empty() {
                    emit_event(
                        &mut out,
                        "content_block_delta",
                        &json!({
                            "type": "content_block_delta",
                            "index": anth_idx,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": args
                            }
                        }),
                    );
                }
            }
        }
    }

    out
}

/// Close every open content block and emit message_delta/message_stop.
/// Callers doing extra out-of-band block injection (e.g. heuristic tool-call
/// extraction) must do so — and register/close their own indices — before
/// calling this, since it ends the message.
pub fn finish_stream(chunk: &Value, buffer: &mut AnthropicStreamBuffer) -> Vec<u8> {
    let mut out = Vec::new();

    let finish_reason = chunk
        .pointer("/choices/0/finish_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("stop");

    for idx in std::mem::take(&mut buffer.open_indices) {
        emit_event(
            &mut out,
            "content_block_stop",
            &json!({
                "type": "content_block_stop",
                "index": idx
            }),
        );
    }

    let sr = match finish_reason {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    };
    emit_event(
        &mut out,
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": sr,
                "stop_sequence": null
            },
            "usage": {
                "output_tokens": chunk.pointer("/usage/completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
            }
        }),
    );
    emit_event(
        &mut out,
        "message_stop",
        &json!({
            "type": "message_stop"
        }),
    );

    out
}

#[derive(Default)]
pub struct AnthropicStreamBuffer {
    pub started: bool,
    pub message_id: Option<String>,
    pub block_id: Option<String>,
    pub next_index: usize,
    pub open_indices: std::collections::BTreeSet<usize>,
    pub tool_index_map: std::collections::HashMap<u64, usize>,
}

fn emit_event(out: &mut Vec<u8>, event: &str, data: &Value) {
    out.extend_from_slice(b"event: ");
    out.extend_from_slice(event.as_bytes());
    out.extend_from_slice(b"\ndata: ");
    let json_str = serde_json::to_string(data).unwrap_or_default();
    out.extend_from_slice(json_str.as_bytes());
    out.extend_from_slice(b"\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_user_content_image_and_tool_result_not_quoted() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": { "media_type": "image/png", "data": "abc123" }
                    },
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_01",
                        "content": "result text"
                    }
                ]
            }]
        });
        let openai = messages_to_chat(&anthropic).unwrap();
        let parts = openai["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["image_url"]["url"], "data:image/png;base64,abc123");
        assert_eq!(parts[1]["text"], "[tool_result id=toolu_01]\nresult text");
    }

    #[test]
    fn test_messages_to_chat_basic() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let openai = messages_to_chat(&anthropic).unwrap();
        assert_eq!(openai["model"], "claude-sonnet-4-20250514");
        assert_eq!(openai["stream"], true);
        assert_eq!(openai["messages"][0]["role"], "user");
        assert_eq!(openai["messages"][0]["content"], "Hello");
    }

    #[test]
    fn test_messages_to_chat_with_system() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are helpful.",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let openai = messages_to_chat(&anthropic).unwrap();
        assert_eq!(openai["messages"][0]["role"], "system");
        assert_eq!(openai["messages"][0]["content"], "You are helpful.");
        assert_eq!(openai["messages"][1]["role"], "user");
        assert_eq!(openai["messages"][1]["content"], "Hi");
    }

    #[test]
    fn test_chat_to_messages_basic() {
        let openai = json!({
            "id": "chatcmpl-123",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello there!"
                },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 }
        });
        let anthropic = chat_to_messages(&openai).unwrap();
        assert_eq!(anthropic["content"][0]["text"], "Hello there!");
        assert_eq!(anthropic["stop_reason"], "end_turn");
    }

    #[test]
    fn test_chat_to_messages_tool_calls() {
        let openai = json!({
            "id": "chatcmpl-456",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"London\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 10 }
        });
        let anthropic = chat_to_messages(&openai).unwrap();
        assert_eq!(anthropic["stop_reason"], "tool_use");
        assert_eq!(anthropic["content"][0]["type"], "tool_use");
        assert_eq!(anthropic["content"][0]["name"], "get_weather");
    }

    #[test]
    fn test_messages_to_chat_with_tools() {
        let anthropic = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "What's the weather?"}],
            "tools": [{
                "name": "get_weather",
                "description": "Get weather",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "city": {"type": "string"}
                    }
                }
            }],
            "tool_choice": {"type": "auto"}
        });
        let openai = messages_to_chat(&anthropic).unwrap();
        assert_eq!(openai["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(openai["tool_choice"]["type"], "auto");
    }

    #[test]
    fn test_stream_native_tool_call_does_not_collide_with_text_index() {
        let mut buffer = AnthropicStreamBuffer::default();

        let start = json!({"choices": [{"delta": {}, "index": 0}]});
        openai_chunk_to_anthropic_sse(&start, &mut buffer);

        // Native tool_calls whose own `index` is 0 (as most providers emit
        // for the first tool call) must not reuse content-block index 0,
        // which the eagerly-opened text block already claims.
        let tool_delta = json!({
            "choices": [{
                "delta": { "tool_calls": [{ "index": 0, "id": "call_1", "function": { "name": "get_weather" } }] }
            }]
        });
        let bytes = openai_chunk_to_anthropic_sse(&tool_delta, &mut buffer);
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.contains("\"index\":1"),
            "tool block should get a fresh index, got: {text}"
        );
        assert!(buffer.open_indices.contains(&0));
        assert!(buffer.open_indices.contains(&1));

        let finish = json!({"choices": [{"finish_reason": "tool_calls"}]});
        let out = String::from_utf8(finish_stream(&finish, &mut buffer)).unwrap();
        assert_eq!(
            out.matches("event: content_block_stop").count(),
            2,
            "expected both blocks closed, got: {out}"
        );
        assert!(buffer.open_indices.is_empty());
        let stop_pos = out.find("message_stop").unwrap();
        let last_block_stop_pos = out.rfind("content_block_stop").unwrap();
        assert!(
            last_block_stop_pos < stop_pos,
            "content_block_stop must precede message_stop"
        );
    }
}

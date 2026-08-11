//! Native Anthropic upstream: since flare-proxy's own inbound wire format
//! *is* the Anthropic Messages API, routing here needs almost no
//! translation — unlike the OpenAI-compatible and Gemini paths, which both
//! translate into a different upstream shape and then translate the
//! response back.

use serde_json::Value;

/// Anthropic requires `max_tokens`; the incoming request may already carry
/// one, but callers that omit it (some Claude Code CLI internal calls) need
/// a default so the upstream call doesn't fail on a field flare-proxy's own
/// wire format doesn't strictly require.
const DEFAULT_MAX_TOKENS: u64 = 4096;

/// Build the upstream request body: the incoming Anthropic-shaped body,
/// with the model swapped to the resolved upstream model id and streaming
/// forced on (mirrors how the OpenAI-compatible and Gemini paths always
/// stream — flare-proxy's own response is always SSE).
pub fn build_request_body(anthropic_body: &Value, upstream_model: &str) -> Value {
    let mut body = anthropic_body.clone();
    body["model"] = Value::String(upstream_model.to_string());
    body["stream"] = Value::Bool(true);
    if body.get("max_tokens").and_then(Value::as_u64).is_none() {
        body["max_tokens"] = Value::from(DEFAULT_MAX_TOKENS);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn swaps_model_and_forces_stream() {
        let incoming = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 1024,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let body = build_request_body(&incoming, "claude-sonnet-4-5-20250929");
        assert_eq!(body["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["content"], "hi");
    }

    #[test]
    fn fills_missing_max_tokens() {
        let incoming = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": []
        });
        let body = build_request_body(&incoming, "claude-sonnet-4-5-20250929");
        assert_eq!(body["max_tokens"], DEFAULT_MAX_TOKENS);
    }
}

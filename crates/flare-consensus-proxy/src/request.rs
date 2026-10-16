//! Build the Anthropic Messages API request body `flare-proxy`'s
//! `/proxy/v1/messages` endpoint expects — the same shape it forwards
//! (translating as needed) to Anthropic, OpenAI-compatible, and Gemini
//! upstreams alike.

use flare_consensus::ModelCallRequest;
use serde_json::{Value, json};

/// `model` carries `request.model_id` verbatim — the caller must have
/// registered a matching `ModelRoute` for it (see
/// [`crate::FlareProxyModelCaller::new`]), since that's how flare-proxy
/// resolves which provider/upstream-model a request actually goes to.
pub fn build_request_body(request: &ModelCallRequest) -> Value {
    json!({
        "model": request.model_id,
        "max_tokens": request.max_output_tokens,
        "system": request.system,
        "messages": [{ "role": "user", "content": request.user }],
        "temperature": request.temperature,
        "stream": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flare_consensus::Phase;

    fn req(model_id: &str) -> ModelCallRequest {
        ModelCallRequest {
            participant_id: "p".to_string(),
            model_id: model_id.to_string(),
            round: 1,
            phase: Phase::InitialAnalysis,
            system: "sys".to_string(),
            user: "hello".to_string(),
            temperature: 0.7,
            max_output_tokens: 1500,
            cancellation: None,
            on_token: None,
        }
    }

    #[test]
    fn builds_anthropic_shaped_body() {
        let body = build_request_body(&req("gpt-5.2"));
        assert_eq!(body["model"], "gpt-5.2");
        assert_eq!(body["max_tokens"], 1500);
        assert_eq!(body["system"], "sys");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["stream"], true);
    }
}

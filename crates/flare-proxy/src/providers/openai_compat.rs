//! Generic OpenAI-compatible upstream (NVIDIA NIM, OpenRouter, LM Studio,
//! and any future `/chat/completions`-shaped endpoint). Request/response
//! translation is `shape_xlat`'s existing Anthropic<->OpenAI logic; this
//! module only owns per-provider auth/header wiring.

use crate::providers::ProviderEntry;
use reqwest::RequestBuilder;

/// Attach bearer auth (when the provider has an API key) plus any static
/// extra headers the provider entry carries (e.g. OpenRouter's
/// HTTP-Referer/X-Title).
pub fn apply_auth(
    mut builder: RequestBuilder,
    provider: &ProviderEntry,
    api_key: &str,
) -> RequestBuilder {
    if !api_key.is_empty() {
        builder = builder.header("Authorization", format!("Bearer {api_key}"));
    }
    builder = builder.header("Content-Type", "application/json");
    for (k, v) in &provider.extra_headers {
        builder = builder.header(k, v);
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderKind;

    #[test]
    fn applies_bearer_and_extra_headers() {
        let provider = ProviderEntry {
            id: "openrouter".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key_env: Some("OPENROUTER_API_KEY".into()),
            default_model: None,
            models: vec![],
            extra_headers: vec![("X-Title".into(), "agentflare".into())],
        };
        let client = reqwest::Client::new();
        let builder = apply_auth(
            client.post("https://example.invalid/chat/completions"),
            &provider,
            "sk-test",
        );
        let req = builder.build().unwrap();
        assert_eq!(
            req.headers().get("Authorization").unwrap(),
            "Bearer sk-test"
        );
        assert_eq!(req.headers().get("X-Title").unwrap(), "agentflare");
    }

    #[test]
    fn skips_auth_header_when_no_api_key() {
        let provider = ProviderEntry {
            id: "lm-studio".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:1234/v1".into(),
            api_key_env: None,
            default_model: None,
            models: vec![],
            extra_headers: vec![],
        };
        let client = reqwest::Client::new();
        let builder = apply_auth(
            client.post("https://example.invalid/chat/completions"),
            &provider,
            "",
        );
        let req = builder.build().unwrap();
        assert!(req.headers().get("Authorization").is_none());
    }
}

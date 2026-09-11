//! # flare-consensus-proxy
//!
//! Bridges [`flare_consensus::ModelCaller`] to `flare-proxy`'s in-process
//! Anthropic-shaped router — the one new piece needed to run Suprmind-style
//! multi-model consensus using providers `flare-proxy` already speaks to
//! (Anthropic, OpenAI, Gemini, xAI, Perplexity, ...), with zero new
//! per-provider HTTP code.
//!
//! `flare-proxy`'s own `router()` is hardcoded to `ProviderConfig::from_env()`
//! (`MODEL`/`MODEL_OPUS`/`MODEL_SONNET`/`MODEL_HAIKU` — one active model per
//! role, the shape a CLI-routing shim needs). Consensus needs the opposite:
//! several explicit, simultaneously-active `(provider, upstream_model)`
//! pairs, one per participant, picked by the caller rather than the
//! environment. `flare_proxy::router_with_config` (added alongside this
//! crate) accepts an explicit `ProviderConfig` for exactly that; this crate
//! builds one from a small list of [`ModelBinding`]s using
//! `flare_proxy::providers::provider_entry` for the provider data, so the
//! base URLs and API-key env var names live in flare-proxy's registry in
//! exactly one place.

mod request;
mod sse;

pub use request::build_request_body;
pub use sse::{ParsedTurn, SseAccumulator};

use async_trait::async_trait;
use axum::body::Body;
use flare_consensus::{ModelCallError, ModelCallRequest, ModelCallResponse, ModelCaller};
use flare_proxy::providers::{ModelRoute, ProviderConfig, ProviderEntry, provider_entry};
use futures::StreamExt;
use http::{Method, Request};
use tower::ServiceExt;

/// One participant's model handle → the real provider it resolves to.
/// `model_id` must match the `Participant::model_id` /
/// `ModelCallRequest::model_id` the consensus engine sends — that string is
/// what `flare-proxy` looks up in its routing table, so it doubles as the
/// synthetic "Anthropic model name" for this run.
pub struct ModelBinding {
    pub model_id: String,
    /// `MODEL`-prefix from `flare-proxy`'s provider registry, e.g.
    /// `"openai"`, `"anthropic"`, `"gemini"`, `"xai"`, `"perplexity"`.
    pub provider_prefix: String,
    /// The real model id on that provider, e.g. `"gpt-5.2"`,
    /// `"claude-opus-4-5"`, `"grok-4"`.
    pub upstream_model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("unknown flare-proxy provider prefix: {0}")]
    UnknownProvider(String),
}

pub struct FlareProxyModelCaller {
    router: axum::Router,
}

impl FlareProxyModelCaller {
    /// Build a caller wired to exactly the participants' model bindings —
    /// one `ProviderEntry` per distinct `provider_prefix` (deduped, looked
    /// up via `flare_proxy::providers::provider_entry`), one `ModelRoute`
    /// per binding.
    pub fn new(bindings: &[ModelBinding]) -> Result<Self, BuildError> {
        let mut providers: Vec<ProviderEntry> = Vec::new();
        let mut routing: Vec<ModelRoute> = Vec::new();

        for binding in bindings {
            if !providers.iter().any(|p| p.id == binding.provider_prefix) {
                let entry = provider_entry(&binding.provider_prefix)
                    .ok_or_else(|| BuildError::UnknownProvider(binding.provider_prefix.clone()))?;
                providers.push(entry);
            }
            routing.push(ModelRoute {
                anthropic_model: binding.model_id.clone(),
                provider_id: binding.provider_prefix.clone(),
                upstream_model: binding.upstream_model.clone(),
                // Consensus participants never declare tools — a spurious
                // heuristic tool-call match in a confidence-bearing prose
                // response would corrupt `parser::extract_confidence`.
                requires_heuristic_tools: false,
                requires_think_parsing: false,
            });
        }

        let config = ProviderConfig {
            providers,
            routing,
            model: None,
            model_opus: None,
            model_sonnet: None,
            model_haiku: None,
        };
        Ok(Self {
            router: flare_proxy::router_with_config(config),
        })
    }
}

#[async_trait]
impl ModelCaller for FlareProxyModelCaller {
    async fn call(&self, request: ModelCallRequest) -> Result<ModelCallResponse, ModelCallError> {
        let body = request::build_request_body(&request);
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| ModelCallError(format!("encode request: {e}")))?;

        let http_request = Request::builder()
            .method(Method::POST)
            .uri("/proxy/v1/messages")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(body_bytes))
            .map_err(|e| ModelCallError(format!("build request: {e}")))?;

        // `Router` is cheap to clone (internally Arc-backed) and
        // `ServiceExt::oneshot` needs an owned `Service` — same per-call
        // clone pattern axum itself uses for `Router<()>: Service`.
        let response = self
            .router
            .clone()
            .oneshot(http_request)
            .await
            .unwrap_or_else(|e: std::convert::Infallible| match e {});

        let status = response.status();
        if !status.is_success() {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            return Err(ModelCallError(format!(
                "flare-proxy returned {status}: {bytes}"
            )));
        }

        let mut stream = response.into_body().into_data_stream();
        let mut acc = sse::SseAccumulator::new();
        let on_token = request.on_token.clone();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ModelCallError(format!("stream error: {e}")))?;
            let done = acc.feed(&chunk, |token| {
                if let Some(sink) = &on_token {
                    sink(token);
                }
            });
            if done {
                break;
            }
        }

        let turn = acc.finish();
        Ok(ModelCallResponse {
            content: turn.content,
            usage: turn.usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deliberately no test exercises a real `FlareProxyModelCaller::call` —
    // doing so would either need a live provider API key (never touch real
    // network/spend from a test) or environment-dependent assertions (a
    // machine with keys set for real agentflare use would behave
    // differently than one without, which is not a property a test suite
    // should depend on). `request`/`sse` cover the two testable halves of
    // the request/response translation without any I/O.

    #[test]
    fn unknown_provider_prefix_fails_to_build() {
        let result = FlareProxyModelCaller::new(&[ModelBinding {
            model_id: "made-up".to_string(),
            provider_prefix: "not-a-real-provider".to_string(),
            upstream_model: "whatever".to_string(),
        }]);
        match result {
            Err(BuildError::UnknownProvider(p)) => assert_eq!(p, "not-a-real-provider"),
            Ok(_) => panic!("expected BuildError::UnknownProvider"),
        }
    }

    #[test]
    fn dedupes_provider_entries_across_bindings_on_the_same_provider() {
        // Two bindings on the same registered provider ("anthropic" ships
        // in the embedded registry with no required env-var template, so
        // `provider_entry` always resolves it) must not fail or duplicate
        // the provider entry.
        let bindings = [
            ModelBinding {
                model_id: "claude-opus-4-5".to_string(),
                provider_prefix: "anthropic".to_string(),
                upstream_model: "claude-opus-4-5".to_string(),
            },
            ModelBinding {
                model_id: "claude-sonnet-4-5".to_string(),
                provider_prefix: "anthropic".to_string(),
                upstream_model: "claude-sonnet-4-5".to_string(),
            },
        ];
        assert!(FlareProxyModelCaller::new(&bindings).is_ok());
    }
}

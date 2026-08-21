use crate::providers::{cline_login, openai_compat, ProviderConfig, ProviderEntry, ProviderKind};
use crate::shape_xlat::{self, AnthropicStreamBuffer};
use axum::body::Body;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::stream::StreamExt;
use serde_json::{json, Value};

pub async fn proxy_request(
    anthropic_body: Value,
    config: &ProviderConfig,
    client: &reqwest::Client,
) -> Response {
    let model = anthropic_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4-20250514");

    let route = match config.resolve_model(model) {
        Some(r) => r,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!("no route for model: {model}"),
            )
                .into_response()
        }
    };

    let provider = match config.provider(&route.provider_id) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!("unknown provider: {}", route.provider_id),
            )
                .into_response()
        }
    };

    let api_key = match &provider.api_key_env {
        Some(env_var) => match std::env::var(env_var) {
            Ok(v) => v,
            Err(_) => {
                // Fall back to the Cline CLI's own logged-in credentials
                // (~/.cline/data/settings/providers.json), refreshing them
                // when expired.
                match cline_login::cli_credential(client, &provider.id).await {
                    Some(token) => token,
                    None => {
                        return (
                            StatusCode::BAD_REQUEST,
                            format!(
                                "{env_var} not set and no usable Cline CLI login found \
                                 (run the `cline` CLI to sign in)"
                            ),
                        )
                            .into_response()
                    }
                }
            }
        },
        None => String::new(),
    };

    match provider.kind {
        ProviderKind::Anthropic => {
            proxy_anthropic(
                client,
                provider,
                &api_key,
                anthropic_body,
                &route.upstream_model,
            )
            .await
        }
        ProviderKind::Gemini => {
            proxy_gemini(
                client,
                provider,
                &api_key,
                anthropic_body,
                &route.upstream_model,
            )
            .await
        }
        ProviderKind::OpenAiCompatible => {
            proxy_openai_compat(
                client,
                provider,
                &api_key,
                anthropic_body,
                &route.upstream_model,
                route.requires_heuristic_tools,
                route.requires_think_parsing,
            )
            .await
        }
    }
}

/// Native Anthropic upstream: the response is already Anthropic-shaped SSE,
/// so it's forwarded byte-for-byte — no translation, no chunk buffering.
async fn proxy_anthropic(
    client: &reqwest::Client,
    provider: &ProviderEntry,
    api_key: &str,
    anthropic_body: Value,
    upstream_model: &str,
) -> Response {
    let body = crate::providers::anthropic::build_request_body(&anthropic_body, upstream_model);
    let url = provider.base_url.trim_end_matches('/').to_string() + "/v1/messages";

    let mut builder = client
        .post(url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json");
    builder = apply_extra_headers(builder, provider);

    let resp = match builder.json(&body).send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    };

    let resp = match check_status(resp).await {
        Ok(resp) => resp,
        Err(err) => return *err,
    };

    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(Body::from_stream(resp.bytes_stream()))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR).into_response())
}

async fn proxy_gemini(
    client: &reqwest::Client,
    provider: &ProviderEntry,
    api_key: &str,
    anthropic_body: Value,
    upstream_model: &str,
) -> Response {
    let body = crate::providers::gemini::build_request_body(&anthropic_body);
    let url = format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        provider.base_url.trim_end_matches('/'),
        upstream_model
    );

    let mut builder = client
        .post(url)
        .header("x-goog-api-key", api_key)
        .header("Content-Type", "application/json");
    builder = apply_extra_headers(builder, provider);

    let resp = match builder.json(&body).send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    };

    match check_status(resp).await {
        Ok(resp) => stream_translated_sse(
            resp,
            shape_xlat::gemini_chunk_to_anthropic_sse,
            shape_xlat::gemini_finish_stream,
            false,
            false,
        ),
        Err(err) => *err,
    }
}

async fn proxy_openai_compat(
    client: &reqwest::Client,
    provider: &ProviderEntry,
    api_key: &str,
    anthropic_body: Value,
    upstream_model: &str,
    needs_heuristic: bool,
    needs_think: bool,
) -> Response {
    let mut openai_req = match shape_xlat::messages_to_chat(&anthropic_body) {
        Some(r) => r,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                String::from("failed to translate request"),
            )
                .into_response()
        }
    };
    openai_req["model"] = json!(upstream_model);

    let url = provider.base_url.trim_end_matches('/').to_string() + "/chat/completions";
    let builder = client.post(url).json(&openai_req);
    let builder = openai_compat::apply_auth(builder, provider, api_key);

    let resp = match builder.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    };

    match check_status(resp).await {
        Ok(resp) => stream_translated_sse(
            resp,
            shape_xlat::openai_chunk_to_anthropic_sse,
            shape_xlat::finish_stream,
            needs_heuristic,
            needs_think,
        ),
        Err(err) => *err,
    }
}

/// Apply a provider's static extra headers (e.g. Cloudflare AI Gateway's
/// `cf-aig-authorization`) to a request builder. The OpenAI-compatible path
/// gets this via `openai_compat::apply_auth`; native Anthropic/Gemini build
/// their own headers inline, so they call this directly.
fn apply_extra_headers(
    mut builder: reqwest::RequestBuilder,
    provider: &ProviderEntry,
) -> reqwest::RequestBuilder {
    for (k, v) in &provider.extra_headers {
        builder = builder.header(k, v);
    }
    builder
}

/// Verify the upstream response succeeded, converting a non-2xx into an
/// Anthropic-shaped error `Response`. On success, hands back the still-open
/// `reqwest::Response` so the caller can stream its body.
///
/// `Response` (`axum::http::Response<axum::body::Body>`) is >=128 bytes, so
/// it's boxed in the `Err` variant rather than inflating every `Result`
/// return by that much even on the success path (`clippy::result_large_err`).
async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, Box<Response>> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let err_val: Value = serde_json::from_str(&body).unwrap_or(json!({"error": {"message": body}}));
    Err(Box::new(
        (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&shape_xlat::error_to_anthropic(&err_val)).unwrap_or_default(),
        )
            .into_response(),
    ))
}

/// Shared SSE loop for providers whose raw stream needs translating into
/// Anthropic's SSE shape (OpenAI-compatible and Gemini). Native Anthropic
/// upstream skips this entirely — see `proxy_anthropic`.
fn stream_translated_sse(
    resp: reqwest::Response,
    translate_chunk: fn(&Value, &mut AnthropicStreamBuffer) -> Vec<u8>,
    finish: fn(&Value, &mut AnthropicStreamBuffer) -> Vec<u8>,
    needs_heuristic: bool,
    _needs_think: bool,
) -> Response {
    let stream = resp.bytes_stream();
    let mut buffer = AnthropicStreamBuffer::default();
    let mut accumulated_text = String::new();
    let mut line_buf: Vec<u8> = Vec::new();

    let sse_stream = stream.filter_map(move |chunk_result| {
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(_) => return futures::future::ready(None),
        };

        line_buf.extend_from_slice(&chunk);
        let split_at = line_buf
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let complete_bytes: Vec<u8> = line_buf.drain(..split_at).collect();
        let complete = String::from_utf8_lossy(&complete_bytes).into_owned();

        let mut out = Vec::new();

        for line in complete.lines() {
            if !line.starts_with("data: ") {
                continue;
            }
            let data = &line[6..];
            if data == "[DONE]" {
                continue;
            }

            let val: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // api.cline.bot wraps OpenAI-shaped payloads in
            // {success, data:{...}}; unwrap so the pointer lookups below hit
            // the actual completion. No-op for every other provider.
            let val = shape_xlat::unwrap_success_envelope(&val);

            if let Some(content) = val.pointer("/choices/0/delta/content") {
                accumulated_text.push_str(&shape_xlat::content_text(content));
            }
            if let Some(text) = val
                .pointer("/candidates/0/content/parts/0/text")
                .and_then(|v| v.as_str())
            {
                accumulated_text.push_str(text);
            }

            let is_finish = val
                .pointer("/choices/0/finish_reason")
                .and_then(|v| v.as_str())
                .is_some()
                || val
                    .pointer("/candidates/0/finishReason")
                    .and_then(|v| v.as_str())
                    .is_some();

            let translated = translate_chunk(val, &mut buffer);
            out.extend_from_slice(&translated);

            if is_finish {
                if needs_heuristic && !accumulated_text.is_empty() {
                    if let Some(tc) = crate::heuristic::try_extract_tool_call(&accumulated_text) {
                        let idx = buffer.next_index;
                        buffer.next_index += 1;
                        let tool_block = json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.args
                        });
                        let tool_json = serde_json::to_string(&tool_block).unwrap_or_default();
                        let inject = format!(
                            "event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":{idx},\"content_block\":{tool_json}}}\n\n"
                        );
                        out.extend_from_slice(inject.as_bytes());
                        out.extend_from_slice(
                            format!(
                                "event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":{idx}}}\n\n"
                            )
                            .as_bytes(),
                        );
                    }
                }

                // TODO(think-tag suppression): `_needs_think` is currently
                // unused. The raw deltas above are already streamed out via
                // `translate_chunk` before this point runs, so stripping
                // think tags from `accumulated_text` here cannot
                // retroactively remove them from what the client already
                // received — that requires buffering deltas and delaying
                // emission, a larger change tracked separately.

                let finish_bytes = finish(val, &mut buffer);
                out.extend_from_slice(&finish_bytes);
            }
        }

        if out.is_empty() {
            futures::future::ready(None)
        } else {
            futures::future::ready(Some(Ok::<_, std::convert::Infallible>(out)))
        }
    });

    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(Body::from_stream(sse_stream))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR).into_response())
}

use crate::providers::{ProviderConfig, ProviderKind};
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
            Ok(k) => k,
            Err(_) => {
                return (StatusCode::BAD_REQUEST, format!("{} not set", env_var)).into_response()
            }
        },
        None => String::new(),
    };

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
    // `messages_to_chat` copies the incoming Anthropic model string as-is;
    // swap in the provider-native model the route resolved to, or upstream
    // APIs reject/ignore an unrecognized Anthropic model id.
    openai_req["model"] = json!(route.upstream_model);

    let upstream_model = &route.upstream_model;
    openai_req["model"] = json!(upstream_model);

    let needs_heuristic = route.requires_heuristic_tools;
    let needs_think = route.requires_think_parsing;

    let mut req_builder = client
        .post(provider.base_url.trim_end_matches('/').to_string() + "/chat/completions")
        .json(&openai_req);

    match provider.kind {
        ProviderKind::NvidiaNim => {
            req_builder = req_builder
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json");
        }
        ProviderKind::OpenRouter => {
            req_builder = req_builder
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Content-Type", "application/json")
                .header("HTTP-Referer", "https://agentflare.dev")
                .header("X-Title", "agentflare");
        }
        ProviderKind::LmStudio => {
            req_builder = req_builder.header("Content-Type", "application/json");
        }
    }

    let resp = match req_builder.send().await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("upstream error: {e}")).into_response(),
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let err_val: Value =
            serde_json::from_str(&body).unwrap_or(json!({"error": {"message": body}}));
        return (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&shape_xlat::error_to_anthropic(&err_val)).unwrap_or_default(),
        )
            .into_response();
    }

    // Streaming SSE response
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

            if let Some(delta) = val.pointer("/choices/0/delta/content").and_then(|v| v.as_str()) {
                accumulated_text.push_str(delta);
            }

            let is_finish = val
                .pointer("/choices/0/finish_reason")
                .and_then(|v| v.as_str())
                .is_some();

            let anthropic_sse = shape_xlat::openai_chunk_to_anthropic_sse(&val, &mut buffer);
            out.extend_from_slice(&anthropic_sse);

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

                // Think tag stripping on accumulated text. NOTE: the raw
                // deltas above are already streamed out via
                // openai_chunk_to_anthropic_sse before this point runs, so
                // this pass over accumulated_text cannot retroactively
                // remove think-tag content from what the client already
                // received. Properly suppressing think tags requires
                // buffering deltas and delaying emission, which is a larger
                // change tracked separately; this block intentionally does
                // not claim to do that suppression.
                if needs_think && !accumulated_text.is_empty() {
                    let _ = crate::think::strip_think_tags(&accumulated_text);
                }

                let finish_bytes = shape_xlat::finish_stream(&val, &mut buffer);
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

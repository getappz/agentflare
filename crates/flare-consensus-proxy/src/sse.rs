//! Incremental parser for the Anthropic Messages streaming SSE format that
//! `flare-proxy`'s `/proxy/v1/messages` always emits, regardless of which
//! upstream actually served the request (native Anthropic is forwarded
//! byte-for-byte; OpenAI-compatible and Gemini upstreams are translated
//! into this same shape by `flare_proxy::shape_xlat`).
//!
//! Line-buffering technique mirrors `flare_proxy::forward::stream_translated_sse`
//! (split each incoming chunk on the last `\n`, carry the remainder into the
//! next chunk) so a chunk boundary landing mid-event never drops or splits a
//! `data: ` line.

use flare_consensus::TokenUsage;

#[derive(Default)]
pub struct SseAccumulator {
    line_buf: Vec<u8>,
    content: String,
    input_tokens: u32,
    output_tokens: u32,
    got_usage: bool,
    done: bool,
}

pub struct ParsedTurn {
    pub content: String,
    pub usage: Option<TokenUsage>,
}

impl SseAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk of raw bytes from the stream. Calls `on_token` with
    /// each `content_block_delta` text delta as it's found, in order.
    /// Returns `true` once `message_stop` has been seen — the caller should
    /// stop reading the stream at that point.
    pub fn feed(&mut self, chunk: &[u8], mut on_token: impl FnMut(&str)) -> bool {
        self.line_buf.extend_from_slice(chunk);
        let split_at = self
            .line_buf
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let complete_bytes: Vec<u8> = self.line_buf.drain(..split_at).collect();
        let complete = String::from_utf8_lossy(&complete_bytes).into_owned();

        for line in complete.lines() {
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };

            match val.get("type").and_then(|v| v.as_str()) {
                Some("message_start") => {
                    if let Some(n) = val
                        .pointer("/message/usage/input_tokens")
                        .and_then(|v| v.as_u64())
                    {
                        self.input_tokens = n as u32;
                        self.got_usage = true;
                    }
                }
                Some("content_block_delta") => {
                    if let Some(text) = val.pointer("/delta/text").and_then(|v| v.as_str()) {
                        self.content.push_str(text);
                        on_token(text);
                    }
                }
                Some("message_delta") => {
                    if let Some(n) = val.pointer("/usage/output_tokens").and_then(|v| v.as_u64()) {
                        self.output_tokens = n as u32;
                        self.got_usage = true;
                    }
                }
                Some("message_stop") => {
                    self.done = true;
                }
                _ => {}
            }
        }

        self.done
    }

    pub fn finish(self) -> ParsedTurn {
        let usage = self.got_usage.then_some(TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.input_tokens + self.output_tokens,
        });
        ParsedTurn {
            content: self.content,
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One full Anthropic streaming transcript, matching what
    /// `shape_xlat::openai_chunk_to_anthropic_sse`/`finish_stream` (or a
    /// native Anthropic upstream) actually emits on the wire.
    fn transcript(text_parts: &[&str], input_tokens: u64, output_tokens: u64) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"msg_1\",\"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":0}}}}}}\n\n"
        ));
        out.push_str("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n");
        for part in text_parts {
            out.push_str(&format!(
                "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{part}\"}}}}\n\n"
            ));
        }
        out.push_str(
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        );
        out.push_str(&format!(
            "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{output_tokens}}}}}\n\n"
        ));
        out.push_str("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
        out
    }

    #[test]
    fn parses_a_full_transcript_fed_as_one_chunk() {
        let bytes = transcript(&["Hello", ", ", "world"], 12, 3).into_bytes();
        let mut acc = SseAccumulator::new();
        let mut tokens = Vec::new();
        let done = acc.feed(&bytes, |t| tokens.push(t.to_string()));

        assert!(done);
        assert_eq!(tokens, vec!["Hello", ", ", "world"]);
        let turn = acc.finish();
        assert_eq!(turn.content, "Hello, world");
        let usage = turn.usage.expect("usage should be present");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn reassembles_a_data_line_split_across_chunk_boundaries() {
        let full = transcript(&["Hello world"], 1, 1);
        let bytes = full.as_bytes();
        // Split mid-way through a `data: ` line, not on a `\n` boundary.
        let split_at = bytes.iter().position(|&b| b == b'H').unwrap() + 2;
        let (first, second) = bytes.split_at(split_at);

        let mut acc = SseAccumulator::new();
        let mut tokens = Vec::new();
        let done_after_first = acc.feed(first, |t| tokens.push(t.to_string()));
        assert!(
            !done_after_first,
            "must not report done before the split data line completes"
        );
        let done_after_second = acc.feed(second, |t| tokens.push(t.to_string()));
        assert!(done_after_second);

        assert_eq!(tokens, vec!["Hello world"]);
        assert_eq!(acc.finish().content, "Hello world");
    }

    #[test]
    fn ignores_non_data_lines_and_malformed_json() {
        let mut acc = SseAccumulator::new();
        let mut tokens = Vec::new();
        let done = acc.feed(b"event: ping\ndata: not json at all\n\n", |t| {
            tokens.push(t.to_string())
        });
        assert!(!done);
        assert!(tokens.is_empty());
    }

    #[test]
    fn missing_usage_yields_none() {
        let mut acc = SseAccumulator::new();
        acc.feed(b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n", |_| {});
        acc.feed(b"data: {\"type\":\"message_stop\"}\n\n", |_| {});
        let turn = acc.finish();
        assert_eq!(turn.content, "hi");
        assert!(turn.usage.is_none());
    }
}

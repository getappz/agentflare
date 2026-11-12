//! One headless turn's structured output, read line by line as the agent
//! streams it. Every [`ReplyFormat`] feeds the same [`TurnState`]: the
//! capture reader calls [`TurnState::feed`] on each stdout chunk, so the
//! session id, usage and tool activity are known before the process exits,
//! and the turn's terminal event can end a run whose process lingers.

use super::HeadlessReply;
use agent_registry::ReplyFormat;
use serde_json::Value;

/// A tool call seen in the stream: enough for progress/liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub finished: bool,
}

/// What one turn's stream has said so far.
#[derive(Debug, Clone)]
pub struct TurnState {
    format: ReplyFormat,
    pub reply: HeadlessReply,
    pub tools: Vec<ToolCall>,
    /// The agent's own end-of-turn event arrived. Whether that may end the
    /// run is the agent's call (`Structured::ends_at_result`).
    pub terminal: bool,
    /// A reply-bearing event was seen; without one, stdout is the reply.
    answered: bool,
    /// A tool ran since the last streamed text, so the next text chunk
    /// starts a new message (the reply is the agent's last message).
    text_closed: bool,
}

impl TurnState {
    pub fn new(format: ReplyFormat) -> Self {
        Self {
            format,
            reply: HeadlessReply::default(),
            tools: Vec::new(),
            terminal: false,
            answered: false,
            text_closed: false,
        }
    }

    /// The whole of an already-captured stdout, fed as the reader would.
    pub fn from_stdout(format: ReplyFormat, stdout: &str) -> Self {
        let mut turn = Self::new(format);
        turn.feed(stdout.as_bytes(), &mut 0);
        turn
    }

    /// Feeds each complete line of `buf[*consumed..]`, then a trailing
    /// unterminated line once it parses (a CLI may print its last event
    /// without a newline and linger), advancing `*consumed` past them. Raw
    /// bytes, so a chunk split inside a UTF-8 code point stays intact.
    pub fn feed(&mut self, buf: &[u8], consumed: &mut usize) {
        while let Some(newline) = buf[*consumed..].iter().position(|b| *b == b'\n') {
            let end = *consumed + newline + 1;
            self.feed_line(&buf[*consumed..end]);
            *consumed = end;
        }
        let tail = &buf[*consumed..];
        if tail.trim_ascii_end().last() == Some(&b'}') && self.feed_line(tail) {
            *consumed = buf.len();
        }
    }

    fn feed_line(&mut self, line: &[u8]) -> bool {
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            return false;
        };
        match self.format {
            ReplyFormat::ResultObject => self.result_object_line(&event),
            ReplyFormat::CodexEvents => self.codex_line(&event),
            ReplyFormat::OpencodeEvents => self.opencode_line(&event),
            // Fed through `push_text`/`end_turn` by the ACP client.
            ReplyFormat::AcpUpdates => {}
        }
        true
    }

    /// The reply text a streamed transcript carries, `None` when `self`
    /// saw no event of its format. Never looks at a whole-stdout object, so
    /// an already-unwrapped reply that is itself JSON (a judge's
    /// `{"action":..,"response":..}`) is not mistaken for one.
    pub fn streamed_text(self) -> Option<String> {
        self.answered.then_some(self.reply.text)
    }

    /// The finished reply. A whole-reply `json` object (one object, maybe
    /// pretty-printed, or the last line) is read here when no line was a
    /// typed event; anything unstructured comes back as raw text.
    pub fn into_reply(mut self, raw: &str) -> HeadlessReply {
        if !self.answered
            && self.format == ReplyFormat::ResultObject
            && let Some(object) = final_json_object(raw)
        {
            self.read_result(&object);
        }
        if !self.answered {
            self.reply.text = raw.to_string();
        }
        self.reply
    }

    /// Claude Code / Cursor `stream-json`: the session id from any line,
    /// tool calls from assistant/user messages, the reply from `result`.
    fn result_object_line(&mut self, event: &Value) {
        if let Some(id) = str_at(event, "/session_id") {
            self.reply.session_id = Some(id.to_string());
        }
        let blocks = || {
            event
                .pointer("/message/content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        };
        match str_at(event, "/type") {
            Some("assistant") => {
                for block in blocks().filter(|b| str_at(b, "/type") == Some("tool_use")) {
                    let id = str_at(block, "/id").unwrap_or_default();
                    self.tool_started(id, str_at(block, "/name").unwrap_or_default());
                }
            }
            Some("user") => {
                for block in blocks().filter(|b| str_at(b, "/type") == Some("tool_result")) {
                    self.tool_finished(str_at(block, "/tool_use_id").unwrap_or_default());
                }
            }
            Some("result") => {
                self.read_result(event);
                self.terminal |= is_complete_result(event);
            }
            _ => {}
        }
    }

    /// A `result` object (`{"type":"result","subtype":..,"is_error":..,
    /// "num_turns":..,"result":..,"session_id":..,"total_cost_usd":..,
    /// "usage":{..},"structured_output":..}`), or agy's untyped
    /// `{"response":..,"conversation_id":..}`. An object with neither reply
    /// text nor a session is not a reply.
    fn read_result(&mut self, value: &Value) {
        let is_result_event = str_at(value, "/type") == Some("result")
            || value.get("session_id").is_some()
            || value.get("conversation_id").is_some();
        let text = value
            .get("result")
            .or_else(|| value.get("response"))
            .and_then(Value::as_str);
        if text.is_none() && !is_result_event {
            return;
        }
        self.answered = true;
        self.reply.text = text.unwrap_or_default().to_string();
        if let Some(id) = str_at(value, "/session_id").or_else(|| str_at(value, "/conversation_id"))
        {
            self.reply.session_id = Some(id.to_string());
        }
        let usage = value.get("usage");
        let tokens = |key: &str| {
            usage
                .and_then(|u| u.get(key))
                .or_else(|| value.get(key))
                .and_then(Value::as_u64)
        };
        // Fresh + cache-creation + cache-read, comparable whatever the cache hit rate.
        self.reply.input_tokens = match (
            tokens("input_tokens"),
            tokens("cache_creation_input_tokens"),
            tokens("cache_read_input_tokens").or_else(|| tokens("cache_read_tokens")),
        ) {
            (None, None, None) => None,
            (fresh, created, read) => {
                Some(fresh.unwrap_or(0) + created.unwrap_or(0) + read.unwrap_or(0))
            }
        };
        self.reply.output_tokens = tokens("output_tokens");
        self.reply.cost_usd = value.get("total_cost_usd").and_then(Value::as_f64);
        self.reply.subtype = str_at(value, "/subtype").map(str::to_string);
        self.reply.is_error = value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.reply.num_turns = value.get("num_turns").and_then(Value::as_u64);
        self.reply.structured_output = value
            .get("structured_output")
            .filter(|s| !s.is_null())
            .cloned();
    }

    /// Codex `exec --json`: the thread id, the last completed agent
    /// message, tool items, and `turn.completed` usage.
    fn codex_line(&mut self, event: &Value) {
        // Only Codex's own `thread.*`/`turn.*`/`item.*` events, so a reply
        // that merely holds a `{"type":..}` object is not taken for one.
        let Some(kind) = str_at(event, "/type").filter(|k| {
            ["thread.", "turn.", "item."]
                .iter()
                .any(|p| k.starts_with(p))
        }) else {
            return;
        };
        self.answered = true;
        let item_type = str_at(event, "/item/type");
        match kind {
            "thread.started" => {
                self.reply.session_id = str_at(event, "/thread_id").map(str::to_string);
            }
            "item.completed" if item_type == Some("agent_message") => {
                if let Some(text) = str_at(event, "/item/text") {
                    self.reply.text = text.to_string();
                }
            }
            "item.started" | "item.completed"
                if matches!(item_type, Some("command_execution" | "mcp_tool_call")) =>
            {
                let id = str_at(event, "/item/id").unwrap_or_default();
                let name = str_at(event, "/item/tool")
                    .or(item_type)
                    .unwrap_or_default();
                self.tool_started(id, name);
                if kind == "item.completed" {
                    self.tool_finished(id);
                }
            }
            "turn.completed" => {
                self.terminal = true;
                self.reply.input_tokens =
                    event.pointer("/usage/input_tokens").and_then(Value::as_u64);
                self.reply.output_tokens = event
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64);
            }
            // A top-level `error` may be transient (a reconnect notice) and
            // is ignored; only `turn.failed` fails and ends the turn.
            "turn.failed" => {
                self.terminal = true;
                self.reply.is_error = true;
                self.reply.subtype = Some(kind.to_string());
                if let Some(message) =
                    str_at(event, "/error/message").or_else(|| str_at(event, "/message"))
                {
                    self.reply.text = message.to_string();
                }
            }
            _ => {}
        }
    }

    /// opencode `run --format json`: `sessionID` on every event, `text`
    /// parts (the last one is the reply), `tool_use` parts with their
    /// status, and per-step `step_finish` tokens/cost summed over the turn.
    fn opencode_line(&mut self, event: &Value) {
        // Every opencode event carries `sessionID`; a reply that merely
        // holds a `{"type":..}` object does not.
        let (Some(kind), Some(_)) = (str_at(event, "/type"), str_at(event, "/sessionID")) else {
            return;
        };
        self.answered = true;
        if self.reply.session_id.is_none() {
            self.reply.session_id = str_at(event, "/sessionID").map(str::to_string);
        }
        let part = event.get("part").unwrap_or(&Value::Null);
        match kind {
            "text" => {
                if let Some(text) = str_at(part, "/text") {
                    self.reply.text = text.to_string();
                }
            }
            "tool_use" => {
                let id = str_at(part, "/callID")
                    .or_else(|| str_at(part, "/id"))
                    .unwrap_or_default();
                self.tool_started(id, str_at(part, "/tool").unwrap_or_default());
                if matches!(str_at(part, "/state/status"), Some("completed" | "error")) {
                    self.tool_finished(id);
                }
            }
            "step_finish" => {
                // Input excludes cache, output excludes reasoning: add them
                // back for inclusive totals.
                let count = |path: &str| {
                    part.pointer(&format!("/tokens{path}"))
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                };
                *self.reply.input_tokens.get_or_insert(0) +=
                    count("/input") + count("/cache/read") + count("/cache/write");
                *self.reply.output_tokens.get_or_insert(0) +=
                    count("/output") + count("/reasoning");
                if let Some(cost) = part.get("cost").and_then(Value::as_f64) {
                    *self.reply.cost_usd.get_or_insert(0.0) += cost;
                }
            }
            "error" => {
                self.terminal = true;
                self.reply.is_error = true;
                self.reply.subtype = Some(kind.to_string());
                if let Some(message) = ["/error/data/message", "/error/message", "/error/name"]
                    .iter()
                    .find_map(|p| str_at(event, p))
                {
                    self.reply.text = message.to_string();
                }
            }
            _ => {}
        }
    }

    /// A streamed chunk of the agent's reply (ACP `agent_message_chunk`).
    pub(super) fn push_text(&mut self, chunk: &str) {
        self.answered = true;
        if std::mem::take(&mut self.text_closed) {
            self.reply.text.clear();
        }
        self.reply.text.push_str(chunk);
    }

    /// The turn ended, as `subtype`; `error` makes it a failed turn whose
    /// message replaces the reply text.
    pub(super) fn end_turn(&mut self, subtype: &str, error: Option<String>) {
        self.answered = true;
        self.terminal = true;
        self.reply.subtype = Some(subtype.to_string());
        if let Some(message) = error {
            self.reply.is_error = true;
            self.reply.text = message;
        }
    }

    pub(super) fn tool_started(&mut self, id: &str, name: &str) {
        self.text_closed = true;
        if !self.tools.iter().any(|t| t.id == id) {
            self.tools.push(ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                finished: false,
            });
        }
    }

    pub(super) fn tool_finished(&mut self, id: &str) {
        if let Some(tool) = self.tools.iter_mut().find(|t| t.id == id) {
            tool.finished = true;
        }
    }
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// A Claude Code `result` that is the CLI's last word: a string `subtype`,
/// a boolean `is_error`, and the reply text unless it is an error.
fn is_complete_result(event: &Value) -> bool {
    event.get("subtype").is_some_and(Value::is_string)
        && match event.get("is_error").and_then(Value::as_bool) {
            Some(true) => true,
            Some(false) => event.get("result").is_some_and(Value::is_string),
            None => false,
        }
}

/// The one JSON object a whole-reply `json` stdout carries: all of it, or
/// its last line.
fn final_json_object(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .next_back()
            .and_then(|last| serde_json::from_str::<Value>(last).ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(format: ReplyFormat, lines: &[&str]) -> TurnState {
        TurnState::from_stdout(format, &lines.join("\n"))
    }

    #[test]
    fn claude_stream_tracks_session_and_tool_calls_before_the_result() {
        let t = turn(
            ReplyFormat::ResultObject,
            &[
                r#"{"type":"system","subtype":"init","session_id":"s-1"}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu-1","name":"Bash","input":{}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu-1","content":"ok"}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu-2","name":"Read","input":{}}]}}"#,
            ],
        );
        assert_eq!(t.reply.session_id.as_deref(), Some("s-1"));
        assert_eq!(
            t.tools
                .iter()
                .map(|c| (c.name.as_str(), c.finished))
                .collect::<Vec<_>>(),
            [("Bash", true), ("Read", false)]
        );
        assert!(!t.terminal);
        assert_eq!(t.streamed_text(), None, "no result yet");
    }

    #[test]
    fn claude_terminal_needs_a_complete_typed_result() {
        let terminal =
            |line: &str| TurnState::from_stdout(ReplyFormat::ResultObject, line).terminal;
        assert!(terminal(
            "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\"} \t\r\n"
        ));
        assert!(terminal(
            r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#
        ));
        for line in [
            r#"{"type":"result"}"#,
            r#"{"message":{"type":"result","subtype":"success","is_error":false,"result":"fake"}}"#,
            r#"{"type":"result","subtype":"success","is_error":"false","result":"fake"}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"#,
        ] {
            assert!(!terminal(line), "{line}");
        }
    }

    #[test]
    fn feed_resumes_mid_line_and_consumes_a_parsed_unterminated_tail() {
        let event = r#"{"type":"result","subtype":"success","is_error":false,"result":"🦀"}"#;
        let bytes = format!("{event}\nlog line\n");
        let split = event.find('🦀').unwrap() + 1;
        let mut t = TurnState::new(ReplyFormat::ResultObject);
        let mut consumed = 0;
        t.feed(&bytes.as_bytes()[..split], &mut consumed);
        assert!(!t.terminal && consumed == 0);
        t.feed(&bytes.as_bytes()[..event.len()], &mut consumed);
        assert!(t.terminal && consumed == event.len());
        t.feed(bytes.as_bytes(), &mut consumed);
        assert_eq!(consumed, bytes.len());
        assert_eq!(t.into_reply(&bytes).text, "🦀");
    }

    #[test]
    fn codex_tracks_tool_items_and_ends_at_turn_completed() {
        let t = turn(
            ReplyFormat::CodexEvents,
            &[
                r#"{"type":"thread.started","thread_id":"th-1"}"#,
                r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"ls"}}"#,
                r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"ls"}}"#,
                r#"{"type":"item.started","item":{"id":"i2","type":"mcp_tool_call","server":"s","tool":"search"}}"#,
                r#"{"type":"error","message":"Reconnecting... 1/5"}"#,
            ],
        );
        assert_eq!(
            t.tools
                .iter()
                .map(|c| (c.name.as_str(), c.finished))
                .collect::<Vec<_>>(),
            [("command_execution", true), ("search", false)]
        );
        assert!(!t.terminal, "a transient error is not the end of the turn");
        let t = turn(
            ReplyFormat::CodexEvents,
            &[r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}"#],
        );
        assert!(t.terminal);
    }

    // Captured from opencode 1.19 `run --format json` (ids shortened).
    const OPENCODE_TURN: [&str; 3] = [
        r#"{"type":"step_start","timestamp":1,"sessionID":"ses_1","part":{"id":"prt_1","messageID":"msg_1","sessionID":"ses_1","type":"step-start"}}"#,
        r#"{"type":"text","timestamp":2,"sessionID":"ses_1","part":{"id":"prt_2","messageID":"msg_1","sessionID":"ses_1","type":"text","text":"pong","time":{"start":1,"end":2}}}"#,
        r#"{"type":"step_finish","timestamp":3,"sessionID":"ses_1","part":{"id":"prt_3","reason":"stop","messageID":"msg_1","sessionID":"ses_1","type":"step-finish","tokens":{"total":61701,"input":61698,"output":3,"reasoning":0,"cache":{"write":0,"read":0}},"cost":0.04072662}}"#,
    ];

    #[test]
    fn opencode_reads_session_text_tokens_and_cost() {
        let reply = turn(ReplyFormat::OpencodeEvents, &OPENCODE_TURN).into_reply("");
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("ses_1"));
        assert_eq!(reply.input_tokens, Some(61698));
        assert_eq!(reply.output_tokens, Some(3));
        assert_eq!(reply.cost_usd, Some(0.04072662));
        assert!(!reply.is_error);
    }

    #[test]
    fn opencode_sums_steps_tracks_tools_and_keeps_the_last_text() {
        let t = turn(
            ReplyFormat::OpencodeEvents,
            &[
                r#"{"type":"text","sessionID":"ses_2","part":{"text":"starting"}}"#,
                r#"{"type":"tool_use","sessionID":"ses_2","part":{"callID":"c1","tool":"bash","state":{"status":"completed"}}}"#,
                r#"{"type":"step_finish","sessionID":"ses_2","part":{"tokens":{"input":10,"output":2,"reasoning":1,"cache":{"read":5,"write":1}},"cost":0.5}}"#,
                r#"{"type":"tool_use","sessionID":"ses_2","part":{"callID":"c2","tool":"read","state":{"status":"running"}}}"#,
                r#"{"type":"text","sessionID":"ses_2","part":{"text":"done"}}"#,
                r#"{"type":"step_finish","sessionID":"ses_2","part":{"tokens":{"input":4,"output":1},"cost":0.25}}"#,
            ],
        );
        assert_eq!(
            t.tools
                .iter()
                .map(|c| (c.name.as_str(), c.finished))
                .collect::<Vec<_>>(),
            [("bash", true), ("read", false)]
        );
        let reply = t.into_reply("");
        assert_eq!(reply.text, "done");
        assert_eq!(reply.input_tokens, Some(10 + 5 + 1 + 4));
        assert_eq!(reply.output_tokens, Some(2 + 1 + 1));
        assert_eq!(reply.cost_usd, Some(0.75));
    }

    #[test]
    fn opencode_error_event_fails_the_turn() {
        let t = turn(
            ReplyFormat::OpencodeEvents,
            &[
                r#"{"type":"error","sessionID":"ses_3","error":{"name":"APIError","data":{"message":"rate limited"}}}"#,
            ],
        );
        assert!(t.terminal);
        let reply = t.into_reply("");
        assert!(reply.is_error);
        assert_eq!(reply.text, "rate limited");
        assert_eq!(reply.session_id.as_deref(), Some("ses_3"));
    }

    #[test]
    fn streamed_text_ignores_an_already_unwrapped_json_reply() {
        for reply in [
            r#"{"action":"advance_task","response":"done"}"#,
            r#"{"type":"module"}"#,
            r#"{"type":"error","message":"not a stream event"}"#,
        ] {
            for format in [
                ReplyFormat::ResultObject,
                ReplyFormat::CodexEvents,
                ReplyFormat::OpencodeEvents,
            ] {
                assert_eq!(
                    TurnState::from_stdout(format, reply).streamed_text(),
                    None,
                    "{format:?}: {reply}"
                );
            }
        }
    }

    #[test]
    fn codex_transient_error_does_not_fail_a_completed_turn() {
        let reply = turn(
            ReplyFormat::CodexEvents,
            &[
                r#"{"type":"thread.started","thread_id":"th-1"}"#,
                r#"{"type":"error","message":"Reconnecting... 1/5"}"#,
                r#"{"type":"item.completed","item":{"id":"m","type":"agent_message","text":"done"}}"#,
                r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}"#,
            ],
        )
        .into_reply("");
        assert!(!reply.is_error);
        assert_eq!(reply.subtype, None);
        assert_eq!(reply.text, "done");
    }
}

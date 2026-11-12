//! The Agent Client Protocol (JSON-RPC 2.0, one message per line on the
//! agent's stdin/stdout) as a pure state machine. [`AcpCore`] takes the
//! agent's lines and says what to write back ([`Out::Send`]) or to decide
//! ([`Out::Decide`]); what the agent reports lands in the same [`TurnState`]
//! every CLI stream feeds. No process here: `stream.rs` drives it.
//!
//! One headless turn: `initialize` → `session/load` (resuming, when the
//! agent can) or `session/new` → `session/set_config_option` (model, when
//! routed) → `session/prompt`. Hand-rolled rather than the
//! `agent-client-protocol` crate, which is async (tokio + futures) and
//! models the whole spec; a blocking capture loop needs these few messages.

use super::turn::TurnState;
use agent_registry::Agent;
use agentflare_approval::{AutonomyTier, CommandClass, TierAction, classify_command, tier_action};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The ACP version spoken.
const PROTOCOL_VERSION: u64 = 1;

/// What the core asks its driver to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Out {
    /// Write this line to the agent.
    Send(String),
    /// Decide a permission request (`tool`/`input` in the approval policy's
    /// shape, see [`policy_action`]), then [`AcpCore::answer`] `ask`.
    Decide {
        ask: u64,
        tool: String,
        input: Value,
    },
}

/// Decides a permission request (`tool`, `input` as in [`Out::Decide`]).
/// True allows it.
pub(crate) type Approver = Arc<dyn Fn(&str, &Value) -> bool + Send + Sync>;

/// The approver for an unattended run: the approval policy at
/// [`AutonomyTier::Full`] (reads and writes run; network, install and
/// destructive actions would prompt, and with nobody to ask they are refused).
pub(crate) fn unattended_approver() -> Approver {
    Arc::new(|tool, input| allowed(AutonomyTier::Full, tool, input))
}

/// Whether `tier` lets `tool` run unprompted.
pub(crate) fn allowed(tier: AutonomyTier, tool: &str, input: &Value) -> bool {
    let class = match tool {
        "Read" => CommandClass::Read,
        "Bash" => classify_command(input.get("command").and_then(Value::as_str).unwrap_or("")),
        "WebFetch" => CommandClass::Network,
        // `Write`, and anything unrecognized: the policy's fail-closed default.
        _ => CommandClass::Write,
    };
    tier_action(tier, class) == TierAction::Allow
}

/// An ACP tool call in the approval policy's vocabulary.
fn policy_action(call: &Value) -> (String, Value) {
    let s = |p: &str| call.pointer(p).and_then(Value::as_str).map(str::to_string);
    let path = ["/locations/0/path", "/rawInput/path", "/rawInput/file_path"]
        .iter()
        .find_map(|p| s(p))
        .unwrap_or_default();
    let command = match call.pointer("/rawInput/command") {
        Some(Value::String(c)) => c.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => s("/title").unwrap_or_default(),
    };
    let raw = || call.get("rawInput").cloned().unwrap_or(Value::Null);
    match call.get("kind").and_then(Value::as_str).unwrap_or("other") {
        "edit" | "delete" | "move" => ("Write".into(), json!({"file_path": path})),
        "execute" => ("Bash".into(), json!({"command": command})),
        "read" | "search" => ("Read".into(), json!({"file_path": path})),
        "fetch" => ("WebFetch".into(), raw()),
        _ => (s("/title").unwrap_or_else(|| "other".into()), raw()),
    }
}

/// The session to resume and the model to select, read from a print-mode
/// argv (`agent`'s resume flag, `--model`). Nothing else in it applies to an
/// ACP server.
pub(crate) fn session_args(agent: Agent, args: &[String]) -> (Option<String>, Option<String>) {
    let value = |flag: &str| {
        let eq = format!("{flag}=");
        args.iter()
            .enumerate()
            .find_map(|(i, a)| match a.strip_prefix(&eq) {
                Some(v) => Some(v.to_string()),
                None => (a == flag).then(|| args.get(i + 1).cloned()).flatten(),
            })
    };
    let resume = agent_registry::headless_cli(agent)
        .and_then(|c| c.resume)
        .and_then(value);
    (resume, value("--model"))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    Initialize,
    NewSession,
    LoadSession,
    SetModel,
    Prompt,
}

impl Pending {
    fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::NewSession => "session/new",
            Self::LoadSession => "session/load",
            Self::SetModel => "session/set_config_option",
            Self::Prompt => "session/prompt",
        }
    }
}

/// The ACP protocol for one headless turn.
pub(crate) struct AcpCore {
    cwd: PathBuf,
    prompt: Option<String>,
    resume: Option<String>,
    model: Option<String>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    session: Option<String>,
    /// Open permission requests: ask → (the agent's JSON-RPC id, options).
    asks: HashMap<u64, (Value, Vec<Value>)>,
    next_ask: u64,
}

impl AcpCore {
    /// `cwd` must be absolute (ACP requires it).
    pub(crate) fn new(
        cwd: PathBuf,
        prompt: String,
        resume: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            cwd,
            prompt: Some(prompt),
            resume,
            model,
            next_id: 0,
            pending: HashMap::new(),
            session: None,
            asks: HashMap::new(),
            next_ask: 0,
        }
    }

    fn request(&mut self, kind: Pending, params: Value) -> Vec<Out> {
        self.next_id += 1;
        self.pending.insert(self.next_id, kind);
        let msg = json!({"jsonrpc": "2.0", "id": self.next_id, "method": kind.method(), "params": params});
        vec![Out::Send(msg.to_string())]
    }

    /// The first line to send once the agent is running.
    pub(crate) fn start(&mut self) -> Vec<Out> {
        // No fs/terminal: the agent reads, writes and runs commands itself,
        // asking first.
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "agentflare", "version": env!("CARGO_PKG_VERSION")},
        });
        self.request(Pending::Initialize, params)
    }

    /// One line from the agent; what it reports goes into `turn`.
    pub(crate) fn line(&mut self, line: &[u8], turn: &mut TurnState) -> Vec<Out> {
        let Ok(msg) = serde_json::from_slice::<Value>(line) else {
            return Vec::new();
        };
        let params = msg.get("params").unwrap_or(&Value::Null);
        match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
            (Some(method), Some(id)) => self.agent_request(method, id.clone(), params),
            // `session/load` replays the old conversation before answering:
            // history, not this turn.
            (Some("session/update"), None)
                if !self.pending.values().any(|p| *p == Pending::LoadSession) =>
            {
                update(params.get("update").unwrap_or(&Value::Null), turn);
                Vec::new()
            }
            (None, Some(id)) => self.response(id.as_u64(), &msg, turn),
            _ => Vec::new(),
        }
    }

    fn new_session(&mut self) -> Vec<Out> {
        let params = json!({"cwd": self.cwd, "mcpServers": []});
        self.request(Pending::NewSession, params)
    }

    fn response(&mut self, id: Option<u64>, msg: &Value, turn: &mut TurnState) -> Vec<Out> {
        let Some(kind) = id.and_then(|id| self.pending.remove(&id)) else {
            return Vec::new();
        };
        if let Some(err) = msg.get("error") {
            // A saved session the agent can't take back: start afresh.
            if kind == Pending::LoadSession {
                self.session = None;
                return self.new_session();
            }
            let text = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("an error");
            turn.end_turn("error", Some(format!("{} failed: {text}", kind.method())));
            return Vec::new();
        }
        let result = msg.get("result").unwrap_or(&Value::Null);
        match kind {
            Pending::Initialize => {
                let can_load = result
                    .pointer("/agentCapabilities/loadSession")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                match self.resume.take().filter(|_| can_load) {
                    Some(sid) => {
                        self.session = Some(sid.clone());
                        let params = json!({"sessionId": sid, "cwd": self.cwd, "mcpServers": []});
                        self.request(Pending::LoadSession, params)
                    }
                    None => self.new_session(),
                }
            }
            Pending::NewSession | Pending::LoadSession => {
                if let Some(sid) = result.get("sessionId").and_then(Value::as_str) {
                    self.session = Some(sid.to_string());
                }
                let Some(sid) = self.session.clone() else {
                    turn.end_turn(
                        "error",
                        Some(format!("{} opened no session", kind.method())),
                    );
                    return Vec::new();
                };
                turn.reply.session_id = Some(sid.clone());
                match self
                    .model
                    .take()
                    .filter(|m| current_model(result) != Some(m))
                {
                    Some(model) => {
                        let params = json!({"sessionId": sid, "configId": "model", "value": model});
                        self.request(Pending::SetModel, params)
                    }
                    None => self.send_prompt(),
                }
            }
            Pending::SetModel => self.send_prompt(),
            Pending::Prompt => {
                match result.get("stopReason").and_then(Value::as_str) {
                    Some("max_tokens") => turn.end_turn("error_max_tokens", None),
                    Some("max_turn_requests") => turn.end_turn("error_max_turns", None),
                    Some(reason @ ("refusal" | "cancelled")) => {
                        turn.end_turn(reason, Some(format!("the agent's turn ended: {reason}")))
                    }
                    _ => turn.end_turn("success", None),
                }
                Vec::new()
            }
        }
    }

    fn send_prompt(&mut self) -> Vec<Out> {
        let (Some(sid), Some(text)) = (self.session.clone(), self.prompt.take()) else {
            return Vec::new();
        };
        let params = json!({"sessionId": sid, "prompt": [{"type": "text", "text": text}]});
        self.request(Pending::Prompt, params)
    }

    fn agent_request(&mut self, method: &str, id: Value, params: &Value) -> Vec<Out> {
        if method != "session/request_permission" {
            // No file system or terminal is offered; the agent does its own.
            let msg = json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("{method} is not offered by this client")}});
            return vec![Out::Send(msg.to_string())];
        }
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.next_ask += 1;
        self.asks.insert(self.next_ask, (id, options));
        let (tool, input) = policy_action(params.get("toolCall").unwrap_or(&Value::Null));
        vec![Out::Decide {
            ask: self.next_ask,
            tool,
            input,
        }]
    }

    /// Answers permission request `ask` once: the first option of the wanted
    /// kind (`allow_once`, else any `allow*`; likewise `reject*`), or
    /// `cancelled` when the agent offered none.
    pub(crate) fn answer(&mut self, ask: u64, allow: bool) -> Vec<Out> {
        let Some((id, options)) = self.asks.remove(&ask) else {
            return Vec::new();
        };
        let (once, any) = if allow {
            ("allow_once", "allow")
        } else {
            ("reject_once", "reject")
        };
        fn kind(option: &Value) -> &str {
            option
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
        }
        let pick = options
            .iter()
            .find(|o| kind(o) == once)
            .or_else(|| options.iter().find(|o| kind(o).starts_with(any)))
            .and_then(|o| o.get("optionId").cloned());
        let outcome = match pick {
            Some(option) => json!({"outcome": "selected", "optionId": option}),
            None => json!({"outcome": "cancelled"}),
        };
        let msg = json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}});
        vec![Out::Send(msg.to_string())]
    }
}

/// The session's selected model, from its `model` config option.
fn current_model(result: &Value) -> Option<&str> {
    result
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| o.get("id").and_then(Value::as_str) == Some("model"))?
        .get("currentValue")?
        .as_str()
}

/// A `session/update` notification's `update`, into `turn`.
fn update(u: &Value, turn: &mut TurnState) {
    let s = |k: &str| u.get(k).and_then(Value::as_str).unwrap_or_default();
    let id = s("toolCallId");
    match s("sessionUpdate") {
        "agent_message_chunk" => {
            if let Some(text) = u.pointer("/content/text").and_then(Value::as_str) {
                turn.push_text(text);
            }
        }
        "tool_call" => {
            let name = Some(s("title"))
                .filter(|t| !t.is_empty())
                .unwrap_or(s("kind"));
            turn.tool_started(id, name);
        }
        "tool_call_update" => {}
        _ => return,
    }
    if matches!(s("status"), "completed" | "failed") {
        turn.tool_finished(id);
    }
}

#[cfg(test)]
#[path = "acp_tests.rs"]
mod tests;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    ClaudeCode,
    Codex,
    OpenCode,
    Cursor,
    Gemini,
    Copilot,
    Amp,
    Pi,
    Qoder,
    OpenClaw,
    Kimi,
    Antigravity,
    Droid,
    Unknown(String),
}

impl SessionSource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::Copilot => "copilot",
            Self::Amp => "amp",
            Self::Pi => "pi",
            Self::Qoder => "qoder",
            Self::OpenClaw => "openclaw",
            Self::Kimi => "kimi",
            Self::Antigravity => "antigravity",
            Self::Droid => "droid",
            Self::Unknown(s) => s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Idle,
    Waiting,
    Completed,
    Error,
    Abandoned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingReason {
    NeedsInput,
    TurnDone,
    AtPrompt,
    Interrupted,
    PermissionPrompt,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

impl TokenUsage {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cost {
    pub total_usd: f64,
    pub input_usd: f64,
    pub output_usd: f64,
    pub cache_read_usd: f64,
    pub cache_write_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub source: SessionSource,
    pub project: String,
    pub project_path: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub status: SessionStatus,
    pub awaiting_reason: Option<AwaitingReason>,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<u64>,
    pub tokens: TokenUsage,
    pub cost: Option<Cost>,
    pub turn_count: u32,
    pub tool_call_count: u32,
    pub subagent_count: u32,
    pub tags: Vec<String>,
    pub starred: bool,
    pub pid: Option<u32>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    pub session_id: String,
    pub seq: u32,
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub tokens: Option<TokenUsage>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub session_id: String,
    pub turn_seq: u32,
    pub name: String,
    pub input: serde_json::Value,
    pub output: Option<String>,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subagent {
    pub id: String,
    pub session_id: String,
    pub parent_tool_call_id: Option<String>,
    pub kind: String,
    pub status: String,
    pub task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEvent {
    pub id: String,
    pub session_id: String,
    pub path: String,
    pub kind: String, // read/write/edit
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyCost {
    pub date: String, // YYYY-MM-DD
    pub tokens: TokenUsage,
    pub cost_usd: f64,
    pub session_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStats {
    pub tool: String,
    pub count: u64,
    pub avg_duration_ms: Option<f64>,
    pub failure_rate: Option<f64>,
}

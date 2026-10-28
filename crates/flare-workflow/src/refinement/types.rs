use crate::{WorkflowData, WorkflowRunId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub type HarnessTree = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RefinementMode {
    Parallel,
    Sequential,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub agent: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_calls: u32,
    pub call_timeout_secs: u64,
    pub total_timeout_secs: u64,
    pub max_cost_usd: Option<f64>,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_calls: 32,
            call_timeout_secs: 600,
            total_timeout_secs: 3600,
            max_cost_usd: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefinementRequest {
    pub task_id: String,
    pub domain: String,
    pub prompt: String,
    pub repository: PathBuf,
    pub solver: AgentConfig,
    pub improver: AgentConfig,
    pub mode: RefinementMode,
    pub k: u32,
    pub checks: Vec<Vec<String>>,
    #[serde(default)]
    pub limits: Limits,
}
impl RefinementRequest {
    pub fn validate(&self) -> Result<(), RefinementError> {
        if !(1..=16).contains(&self.k)
            || [
                &self.task_id,
                &self.domain,
                &self.prompt,
                &self.solver.agent,
                &self.solver.model,
                &self.improver.agent,
                &self.improver.model,
            ]
            .iter()
            .any(|s| s.trim().is_empty())
            || self.repository.as_os_str().is_empty()
        {
            return Err(RefinementError::Invalid(
                "k must be 1..16; task, domain, prompt, repository, agents and models are required"
                    .into(),
            ));
        }
        let l = &self.limits;
        if l.max_calls == 0
            || l.call_timeout_secs == 0
            || l.total_timeout_secs == 0
            || l.total_timeout_secs > 604800
            || l.call_timeout_secs > l.total_timeout_secs
            || l.max_cost_usd.is_some_and(|v| !v.is_finite() || v <= 0.0)
        {
            return Err(RefinementError::Invalid(
                "invalid invocation, time, or dollar limit (maximum total duration: 7 days)".into(),
            ));
        }
        if self.checks.is_empty()
            || self.checks.iter().any(|c| {
                c.first().is_none_or(|s| s.trim().is_empty()) || c.iter().any(|s| s.contains('\0'))
            })
        {
            return Err(RefinementError::Invalid(
                "at least one nonempty acceptance argv is required".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RefinementError {
    #[error("invalid refinement input: {0}")]
    Invalid(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("interrupted invocation: {0}")]
    Interrupted(String),
    #[error("refinement cancelled")]
    Cancelled,
    #[error("refinement limit exhausted: {0}")]
    Exhausted(String),
    #[error("refinement persistence: {0}")]
    Persistence(String),
    #[error("refinement identity changed: {0}")]
    Changed(String),
}
impl From<std::io::Error> for RefinementError {
    fn from(e: std::io::Error) -> Self {
        Self::Persistence(e.to_string())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Probe,
    General,
    Domain,
    Compose,
    Select,
    Reflect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invocation {
    pub id: String,
    pub run_id: WorkflowRunId,
    pub identity: String,
    pub role: Role,
    pub agent: AgentConfig,
    pub prompt: String,
    pub harness: HarnessTree,
    pub checks: Vec<Vec<String>>,
    pub limits: Limits,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
    pub reply: String,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub session_id: Option<String>,
    pub artifact_identity: Option<String>,
    pub artifact_path: Option<PathBuf>,
    pub accepted: Option<bool>,
    pub evidence: String,
    pub private_checks: String,
}
impl Receipt {
    pub fn successful(&self) -> bool {
        self.error.is_none() && self.accepted == Some(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub label: String,
    pub tree: HarnessTree,
    pub notes: String,
    pub fallback: bool,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trial {
    pub label: String,
    pub tree: HarnessTree,
    pub receipt: Receipt,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Adoption {
    pub trial_id: String,
    pub harness_identity: String,
    pub reason: String,
    pub is_baseline: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experience {
    pub project: String,
    pub domain: String,
    pub cue: String,
    pub lesson: String,
    pub evidence: String,
    pub trial_ids: Vec<String>,
    pub confidence: String,
}
impl Experience {
    pub fn valid_for(&self, data: &RefinementData) -> bool {
        self.project == data.project()
            && self.domain == data.request.domain
            && [&self.cue, &self.lesson, &self.evidence, &self.confidence]
                .iter()
                .all(|s| !s.trim().is_empty())
            && !self.trial_ids.is_empty()
            && self
                .trial_ids
                .iter()
                .all(|id| data.trials.iter().any(|t| &t.label == id))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefinementData {
    pub request: RefinementRequest,
    pub identity: String,
    pub base: HarnessTree,
    pub current: HarnessTree,
    pub experiences: Vec<Experience>,
    pub stages: Vec<Stage>,
    pub trials: Vec<Trial>,
    pub receipts: Vec<Receipt>,
    pub adoption: Option<Adoption>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub runtime: BTreeMap<String, String>,
    #[serde(default)]
    pub budget: serde_json::Value,
    #[serde(default)]
    pub completed_phases: std::collections::BTreeSet<usize>,
}
pub type RefinementReport = RefinementData;
impl WorkflowData for RefinementData {
    fn workflow_type() -> &'static str {
        "harness-refinement"
    }
}
impl RefinementData {
    pub fn new(
        request: RefinementRequest,
        identity: String,
        base: HarnessTree,
    ) -> Result<Self, RefinementError> {
        request.validate()?;
        super::validate_tree(&base)?;
        if identity.is_empty() {
            return Err(RefinementError::Invalid("empty run identity".into()));
        }
        Ok(Self {
            request,
            identity,
            current: base.clone(),
            base,
            experiences: vec![],
            stages: vec![],
            trials: vec![],
            receipts: vec![],
            adoption: None,
            warnings: vec![],
            runtime: BTreeMap::new(),
            budget: serde_json::Value::Null,
            completed_phases: Default::default(),
        })
    }
    pub fn project(&self) -> String {
        self.request.repository.to_string_lossy().into_owned()
    }
}

#[async_trait]
pub trait RefinementBackend: Send + Sync {
    async fn execute(&self, invocation: Invocation) -> Result<Receipt, RefinementError>;
    async fn load_experiences(
        &self,
        project: &str,
        domain: &str,
    ) -> Result<Vec<Experience>, RefinementError>;
    async fn save_experience(
        &self,
        run_id: WorkflowRunId,
        experience: &Experience,
    ) -> Result<(), RefinementError>;
    /// Check external cancellation/deadline again before durable side effects.
    async fn check_active(&self, _run_id: WorkflowRunId) -> Result<(), RefinementError> {
        Ok(())
    }
}

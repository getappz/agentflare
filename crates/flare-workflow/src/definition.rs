//! Workflow definition types and DAG validation.
//!
//! Ported from SMG `wfaas` definition.rs (Apache-2.0), extended with
//! OpenFang's `StepMode`/`ErrorMode` step semantics.

use std::{collections::{HashMap, HashSet}, fmt, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};

use crate::{
    executor::StepExecutor,
    types::{ErrorMode, FailureAction, RetryPolicy, StepId, StepMode, WorkflowContext, WorkflowData, WorkflowId},
};

/// A condition function that determines whether a step should run.
pub type StepCondition<D> = Arc<dyn Fn(&WorkflowContext<D>) -> bool + Send + Sync>;

/// Errors that can occur during workflow validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ValidationError {
    /// A step depends on another step that doesn't exist.
    #[error("step '{step}' depends on non-existent step '{dependency}'")]
    MissingDependency { step: StepId, dependency: StepId },

    /// A cycle was detected in the workflow DAG.
    #[error("cycle detected involving step '{0}'")]
    CycleDetected(StepId),
}

/// Definition of a single step within a workflow.
pub struct StepDefinition<D: WorkflowData> {
    pub id: StepId,
    pub name: String,
    pub executor: Arc<dyn StepExecutor<D>>,
    pub mode: StepMode,
    pub error_mode: ErrorMode,
    pub retry_policy: Option<RetryPolicy>,
    pub timeout: Option<Duration>,
    pub on_failure: FailureAction,
    /// Dependencies that must ALL complete before this step runs.
    pub depends_on: Vec<StepId>,
    /// Dependencies where ANY completing triggers this step.
    pub depends_on_any: Vec<StepId>,
    /// Delay before starting the step (after dependencies satisfied).
    pub delay: Option<Duration>,
    /// Run step at or after this time (after dependencies satisfied).
    pub scheduled_at: Option<DateTime<Utc>>,
    /// Condition to evaluate; if false, step is skipped.
    pub run_if: Option<StepCondition<D>>,
    /// Named variable to store this step's output in for later `{{var}}`
    /// references (OpenFang semantics).
    pub output_var: Option<String>,
}

impl<D: WorkflowData> Clone for StepDefinition<D> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            executor: Arc::clone(&self.executor),
            mode: self.mode.clone(),
            error_mode: self.error_mode.clone(),
            retry_policy: self.retry_policy.clone(),
            timeout: self.timeout,
            on_failure: self.on_failure,
            depends_on: self.depends_on.clone(),
            depends_on_any: self.depends_on_any.clone(),
            delay: self.delay,
            scheduled_at: self.scheduled_at,
            run_if: self.run_if.clone(),
            output_var: self.output_var.clone(),
        }
    }
}

impl<D: WorkflowData> fmt::Debug for StepDefinition<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StepDefinition")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("mode", &self.mode)
            .field("error_mode", &self.error_mode)
            .field("retry_policy", &self.retry_policy)
            .field("timeout", &self.timeout)
            .field("on_failure", &self.on_failure)
            .field("depends_on", &self.depends_on)
            .field("depends_on_any", &self.depends_on_any)
            .field("delay", &self.delay)
            .field("scheduled_at", &self.scheduled_at)
            .field("run_if", &self.run_if.as_ref().map(|_| "<condition>"))
            .finish_non_exhaustive()
    }
}

impl<D: WorkflowData> StepDefinition<D> {
    pub fn new(id: impl Into<String>, name: impl Into<String>, executor: Arc<dyn StepExecutor<D>>) -> Self {
        Self {
            id: StepId::new(id.into()),
            name: name.into(),
            executor,
            mode: StepMode::Sequential,
            error_mode: ErrorMode::Fail,
            retry_policy: None,
            timeout: None,
            on_failure: FailureAction::FailWorkflow,
            depends_on: Vec::new(),
            depends_on_any: Vec::new(),
            delay: None,
            scheduled_at: None,
            run_if: None,
            output_var: None,
        }
    }

    pub fn with_mode(mut self, mode: StepMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_error_mode(mut self, mode: ErrorMode) -> Self {
        self.error_mode = mode;
        self
    }

    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_failure_action(mut self, action: FailureAction) -> Self {
        self.on_failure = action;
        self
    }

    /// Set dependencies: the step runs only after ALL of these complete.
    pub fn depends_on(mut self, deps: &[&str]) -> Self {
        self.depends_on = deps.iter().map(|s| StepId::new(*s)).collect();
        self
    }

    /// Set "any of" dependencies: the step runs when ANY of these complete
    /// (in addition to all `depends_on`). For `depends_on` a failed dependency
    /// blocks the step; for `depends_on_any` only an ALL-failed set blocks.
    /// A skipped dependency counts as completed for satisfaction purposes.
    pub fn depends_on_any(mut self, deps: &[&str]) -> Self {
        self.depends_on_any = deps.iter().map(|s| StepId::new(*s)).collect();
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn scheduled_at(mut self, time: DateTime<Utc>) -> Self {
        self.scheduled_at = Some(time);
        self
    }

    pub fn run_if<F>(mut self, condition: F) -> Self
    where
        F: Fn(&WorkflowContext<D>) -> bool + Send + Sync + 'static,
    {
        self.run_if = Some(Arc::new(condition));
        self
    }

    /// Store this step's output in the named variable for later `{{var}}`
    /// references.
    pub fn with_output_var(mut self, name: impl Into<String>) -> Self {
        self.output_var = Some(name.into());
        self
    }

    /// Iterator over all dependencies (both `depends_on` and `depends_on_any`).
    pub fn all_dependencies(&self) -> impl Iterator<Item = &StepId> {
        self.depends_on.iter().chain(self.depends_on_any.iter())
    }
}

/// Complete workflow definition.
pub struct WorkflowDefinition<D: WorkflowData> {
    pub id: WorkflowId,
    pub name: String,
    pub steps: Vec<StepDefinition<D>>,
    pub default_retry_policy: RetryPolicy,
    pub default_timeout: Duration,
    /// Pre-computed reverse dependencies: step_id -> indices of steps that depend on it.
    reverse_deps: HashMap<StepId, Vec<usize>>,
    /// Pre-computed indices of steps with no dependencies (can start immediately).
    initial_step_indices: Vec<usize>,
}

impl<D: WorkflowData> Clone for WorkflowDefinition<D> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            steps: self.steps.clone(),
            default_retry_policy: self.default_retry_policy.clone(),
            default_timeout: self.default_timeout,
            reverse_deps: self.reverse_deps.clone(),
            initial_step_indices: self.initial_step_indices.clone(),
        }
    }
}

impl<D: WorkflowData> fmt::Debug for WorkflowDefinition<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkflowDefinition")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("steps", &self.steps)
            .field("default_retry_policy", &self.default_retry_policy)
            .field("default_timeout", &self.default_timeout)
            .finish_non_exhaustive()
    }
}

impl<D: WorkflowData> WorkflowDefinition<D> {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: WorkflowId::new(id.into()),
            name: name.into(),
            steps: Vec::new(),
            default_retry_policy: RetryPolicy::default(),
            default_timeout: Duration::from_secs(300),
            reverse_deps: HashMap::new(),
            initial_step_indices: Vec::new(),
        }
    }

    pub fn add_step(mut self, step: StepDefinition<D>) -> Self {
        self.steps.push(step);
        self
    }

    pub fn with_default_retry(mut self, policy: RetryPolicy) -> Self {
        self.default_retry_policy = policy;
        self
    }

    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Retry policy for a step (step-specific or default).
    pub fn get_retry_policy<'a>(&'a self, step: &'a StepDefinition<D>) -> &'a RetryPolicy {
        step.retry_policy.as_ref().unwrap_or(&self.default_retry_policy)
    }

    /// Timeout for a step (step-specific or default).
    pub fn get_timeout(&self, step: &StepDefinition<D>) -> Duration {
        step.timeout.unwrap_or(self.default_timeout)
    }

    /// Validate the workflow DAG and build the dependency graph.
    ///
    /// Fails on missing dependencies or cycles. On success pre-computes
    /// reverse dependencies and initial (dependency-free) step indices.
    #[must_use = "validation result should be checked"]
    pub fn validate(&mut self) -> Result<(), ValidationError> {
        let steps_map: HashMap<&StepId, &StepDefinition<D>> =
            self.steps.iter().map(|s| (&s.id, s)).collect();

        for step in &self.steps {
            for dep in step.all_dependencies() {
                if !steps_map.contains_key(dep) {
                    return Err(ValidationError::MissingDependency {
                        step: step.id.clone(),
                        dependency: dep.clone(),
                    });
                }
            }
        }

        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        for step in &self.steps {
            if !visited.contains(&step.id)
                && Self::has_cycle(&step.id, &steps_map, &mut visited, &mut rec_stack)
            {
                return Err(ValidationError::CycleDetected(step.id.clone()));
            }
        }

        self.reverse_deps.clear();
        for (idx, step) in self.steps.iter().enumerate() {
            for dep_id in step.all_dependencies() {
                self.reverse_deps.entry(dep_id.clone()).or_default().push(idx);
            }
        }

        self.initial_step_indices = self
            .steps
            .iter()
            .enumerate()
            .filter(|(_, s)| s.depends_on.is_empty() && s.depends_on_any.is_empty())
            .map(|(i, _)| i)
            .collect();

        Ok(())
    }

    /// DFS helper for cycle detection.
    fn has_cycle<'a>(
        step_id: &'a StepId,
        steps_map: &HashMap<&'a StepId, &'a StepDefinition<D>>,
        visited: &mut HashSet<&'a StepId>,
        rec_stack: &mut HashSet<&'a StepId>,
    ) -> bool {
        if rec_stack.contains(step_id) {
            return true;
        }
        if visited.contains(step_id) {
            return false;
        }

        visited.insert(step_id);
        rec_stack.insert(step_id);

        if let Some(step) = steps_map.get(step_id) {
            for dep in step.all_dependencies() {
                if Self::has_cycle(dep, steps_map, visited, rec_stack) {
                    return true;
                }
            }
        }

        rec_stack.remove(step_id);
        false
    }

    /// Indices of steps that depend on the given step.
    pub fn get_dependent_indices(&self, step_id: &StepId) -> &[usize] {
        self.reverse_deps
            .get(step_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Indices of steps with no dependencies.
    pub fn get_initial_step_indices(&self) -> &[usize] {
        &self.initial_step_indices
    }
}

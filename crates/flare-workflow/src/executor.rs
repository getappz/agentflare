//! Step executor trait and function-based implementation.
//!
//! Ported from SMG `wfaas` (Apache-2.0). A step is any `StepExecutor<D>`;
//! `FunctionStep` adapts a plain async closure.

use async_trait::async_trait;

use crate::types::{StepResult, WorkflowContext, WorkflowData, WorkflowError, WorkflowResult};

/// Trait for executing individual workflow steps.
#[async_trait]
pub trait StepExecutor<D: WorkflowData>: Send + Sync {
    /// Execute the step with the given context.
    async fn execute(&self, context: &mut WorkflowContext<D>) -> WorkflowResult<StepResult>;

    /// Whether an error should trigger a retry. Override to customize.
    /// By default, all errors are retry-able.
    fn is_retryable(&self, _error: &WorkflowError) -> bool {
        true
    }

    /// Called after the step succeeds (cleanup or side effects).
    async fn on_success(&self, _context: &WorkflowContext<D>) -> WorkflowResult<()> {
        Ok(())
    }

    /// Called when the step fails after all retries (compensation logic).
    async fn on_failure(
        &self,
        _context: &WorkflowContext<D>,
        _error: &WorkflowError,
    ) -> WorkflowResult<()> {
        Ok(())
    }
}

/// Simple function-based step executor.
pub struct FunctionStep<D, F>
where
    D: WorkflowData,
    F: Fn(
            &mut WorkflowContext<D>,
        )
            -> std::pin::Pin<
                Box<dyn std::future::Future<Output = WorkflowResult<StepResult>> + Send + '_>,
            > + Send
            + Sync,
{
    func: F,
    _phantom: std::marker::PhantomData<D>,
}

impl<D, F> FunctionStep<D, F>
where
    D: WorkflowData,
    F: Fn(
            &mut WorkflowContext<D>,
        )
            -> std::pin::Pin<
                Box<dyn std::future::Future<Output = WorkflowResult<StepResult>> + Send + '_>,
            > + Send
            + Sync,
{
    pub fn new(func: F) -> Self {
        Self {
            func,
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<D, F> StepExecutor<D> for FunctionStep<D, F>
where
    D: WorkflowData,
    F: Fn(
            &mut WorkflowContext<D>,
        )
            -> std::pin::Pin<
                Box<dyn std::future::Future<Output = WorkflowResult<StepResult>> + Send + '_>,
            > + Send
            + Sync,
{
    async fn execute(&self, context: &mut WorkflowContext<D>) -> WorkflowResult<StepResult> {
        (self.func)(context).await
    }
}

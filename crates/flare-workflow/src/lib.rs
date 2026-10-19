//! # flare-workflow
//!
//! Embedded durable workflow engine for agent orchestration: typed DAG steps
//! with journaled execution, durable waits, and human-in-the-loop events.
//!
//! Design lineage:
//! - **SMG `wfaas`** (Apache-2.0): typed DAG engine skeleton, retry/backoff
//!   machinery, `StateStore` trait, event bus.
//! - **OpenFang** (MIT/Apache-2.0): step semantics (Sequential/FanOut/Collect/
//!   Conditional/Loop), error modes, input/variable templating.
//! - **Restate** (BSL — design only, no code): journal/event-log durability
//!   (CompletableEntry invariant), step memoization, durable timers and
//!   promises.

pub mod budget;
pub mod budget_send;
pub mod definition;
pub mod engine;
pub mod events;
pub mod executor;
pub mod journal;
pub mod json;
pub mod loops;
pub mod retry;
pub mod rollback;
pub mod sqlite_store;
pub mod store;
pub mod types;
pub mod variables;
pub mod waits;

pub use definition::{StepCondition, StepDefinition, ValidationError, WorkflowDefinition};
pub use engine::{OrphanCancelDisposition, TerminalCallback, WorkflowEngine};
pub use events::{EventBus, EventSubscriber, LoggingSubscriber, WorkflowEvent};
pub use executor::{FunctionStep, StepExecutor, noop_executor};
pub use json::{
    JsonErrorMode, JsonMode, JsonStep, JsonWorkflow, PipelineData, SendMessage, StepInvocation,
    compile_workflow, compile_workflow_in,
};
pub use retry::{Backoff, Retryable, apply_jitter};
pub use sqlite_store::{SqliteStore, SqliteStoreError};
pub use store::{InMemoryStore, StateStore, smoke_test};
pub use types::*;
pub use variables::{capture_output, expand_variables};

#[cfg(test)]
#[path = "tests_schedule_start.rs"]
mod tests_schedule_start;

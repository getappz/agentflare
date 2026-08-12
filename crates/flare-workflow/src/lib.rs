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

pub mod definition;
pub mod executor;
pub mod journal;
pub mod sqlite_store;
pub mod store;
pub mod types;

pub use definition::{StepCondition, StepDefinition, ValidationError, WorkflowDefinition};
pub use executor::{FunctionStep, StepExecutor};
pub use sqlite_store::{SqliteStore, SqliteStoreError};
pub use store::{InMemoryStore, StateStore};
pub use types::*;

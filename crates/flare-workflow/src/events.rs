//! Workflow event system for observability and monitoring.
//!
//! Ported from SMG `wfaas` event.rs (Apache-2.0).

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::types::{StepId, WorkflowId, WorkflowRunId};

/// Default timeout for subscriber event handlers.
const DEFAULT_SUBSCRIBER_TIMEOUT: Duration = Duration::from_secs(30);

/// Events emitted by the workflow engine.
#[derive(Debug, Clone)]
pub enum WorkflowEvent {
    WorkflowStarted { run_id: WorkflowRunId, definition_id: WorkflowId },
    StepStarted { run_id: WorkflowRunId, step_id: StepId, attempt: u32 },
    StepSucceeded { run_id: WorkflowRunId, step_id: StepId, duration: Duration },
    StepFailed { run_id: WorkflowRunId, step_id: StepId, error: String, will_retry: bool },
    StepRetrying { run_id: WorkflowRunId, step_id: StepId, attempt: u32, delay: Duration },
    StepWaiting { run_id: WorkflowRunId, step_id: StepId, reason: String },
    WorkflowCompleted { run_id: WorkflowRunId, duration: Duration },
    WorkflowFailed { run_id: WorkflowRunId, failed_step: StepId, error: String },
    WorkflowCancelled { run_id: WorkflowRunId },
}

/// Trait for subscribing to workflow events.
#[async_trait]
pub trait EventSubscriber: Send + Sync {
    async fn on_event(&self, event: &WorkflowEvent);
}

/// Event bus for publishing and subscribing to workflow events.
///
/// Subscribers are notified in separate spawned tasks with a timeout, so a
/// slow or panicking subscriber never blocks others or the caller.
pub struct EventBus {
    subscribers: Arc<RwLock<Vec<Arc<dyn EventSubscriber>>>>,
    subscriber_timeout: Duration,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(Vec::new())),
            subscriber_timeout: DEFAULT_SUBSCRIBER_TIMEOUT,
        }
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(Vec::new())),
            subscriber_timeout: timeout,
        }
    }

    pub async fn subscribe(&self, subscriber: Arc<dyn EventSubscriber>) {
        self.subscribers.write().await.push(subscriber);
    }

    /// Unsubscribe by `Arc` pointer equality.
    pub async fn unsubscribe(&self, subscriber: &Arc<dyn EventSubscriber>) -> bool {
        let mut subs = self.subscribers.write().await;
        let len_before = subs.len();
        subs.retain(|s| !Arc::ptr_eq(s, subscriber));
        subs.len() < len_before
    }

    /// Fire-and-forget: notify every subscriber in a spawned task.
    pub async fn publish(&self, event: WorkflowEvent) {
        let subscribers: Vec<_> = self.subscribers.read().await.iter().cloned().collect();
        let timeout = self.subscriber_timeout;

        for (idx, subscriber) in subscribers.into_iter().enumerate() {
            let event = event.clone();
            tokio::spawn(async move {
                if tokio::time::timeout(timeout, subscriber.on_event(&event))
                    .await
                    .is_err()
                {
                    warn!(subscriber_index = idx, timeout_secs = timeout.as_secs(), "Event subscriber timed out");
                }
            });
        }
    }

    /// Notify all subscribers and wait for completion (or timeout).
    pub async fn publish_and_wait(&self, event: WorkflowEvent) {
        let subscribers: Vec<_> = self.subscribers.read().await.iter().cloned().collect();
        let timeout = self.subscriber_timeout;

        let handles: Vec<_> = subscribers
            .into_iter()
            .enumerate()
            .map(|(idx, subscriber)| {
                let event = event.clone();
                tokio::spawn(async move {
                    if tokio::time::timeout(timeout, subscriber.on_event(&event))
                        .await
                        .is_err()
                    {
                        warn!(subscriber_index = idx, timeout_secs = timeout.as_secs(), "Event subscriber timed out");
                    }
                })
            })
            .collect();

        for handle in handles {
            let _ = handle.await;
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus").finish_non_exhaustive()
    }
}

/// Subscriber that logs events via `tracing`.
pub struct LoggingSubscriber;

#[async_trait]
impl EventSubscriber for LoggingSubscriber {
    async fn on_event(&self, event: &WorkflowEvent) {
        match event {
            WorkflowEvent::WorkflowStarted { run_id, definition_id } => {
                info!(run_id = %run_id, definition_id = %definition_id, "Workflow started");
            }
            WorkflowEvent::StepStarted { run_id, step_id, attempt } => {
                info!(run_id = %run_id, step_id = %step_id, attempt = attempt, "Step started");
            }
            WorkflowEvent::StepSucceeded { run_id, step_id, duration } => {
                info!(run_id = %run_id, step_id = %step_id, duration_ms = duration.as_millis(), "Step succeeded");
            }
            WorkflowEvent::StepFailed { run_id, step_id, error, will_retry } => {
                warn!(run_id = %run_id, step_id = %step_id, error = error, will_retry = will_retry, "Step failed");
            }
            WorkflowEvent::StepRetrying { run_id, step_id, attempt, delay } => {
                info!(run_id = %run_id, step_id = %step_id, attempt = attempt, delay_ms = delay.as_millis(), "Step retrying");
            }
            WorkflowEvent::StepWaiting { run_id, step_id, reason } => {
                info!(run_id = %run_id, step_id = %step_id, reason = reason, "Step waiting");
            }
            WorkflowEvent::WorkflowCompleted { run_id, duration } => {
                info!(run_id = %run_id, duration_ms = duration.as_millis(), "Workflow completed");
            }
            WorkflowEvent::WorkflowFailed { run_id, failed_step, error } => {
                error!(run_id = %run_id, failed_step = %failed_step, error = error, "Workflow failed");
            }
            WorkflowEvent::WorkflowCancelled { run_id } => {
                info!(run_id = %run_id, "Workflow cancelled");
            }
        }
    }
}

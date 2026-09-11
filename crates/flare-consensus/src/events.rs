//! Consensus event system, for streaming a live run to a UI.
//!
//! Same shape as `flare-workflow`'s `EventBus`/`EventSubscriber` — fire-and-
//! forget publish to N subscribers, each notified in its own spawned task
//! with a timeout so one slow/panicking subscriber never blocks the others
//! or the engine loop. `flare-desktop` implements `EventSubscriber` to
//! forward events to a Tauri `app.emit()`.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::RwLock;
use tracing::warn;

use crate::types::{Disagreement, ParticipantResponse, Phase, SynthesisResult};

const DEFAULT_SUBSCRIBER_TIMEOUT: Duration = Duration::from_secs(30);

/// Events emitted over the lifetime of a `ConsensusEngine::run` call.
/// Mirrors `ai-consensus-core`'s `ConsensusEventMap`, minus the tool-calling
/// events (no tool-call loop in this port).
#[derive(Debug, Clone)]
pub enum ConsensusEvent {
    RoundStart {
        round: u32,
        phase: Phase,
        label: String,
        blind: bool,
        participant_ids: Vec<String>,
    },
    ParticipantStart {
        round: u32,
        phase: Phase,
        participant_id: String,
        model_id: String,
        persona_id: String,
    },
    ParticipantToken {
        round: u32,
        participant_id: String,
        token: String,
    },
    ParticipantComplete {
        round: u32,
        phase: Phase,
        response: ParticipantResponse,
    },
    ConfidenceUpdate {
        round: u32,
        participant_id: String,
        confidence: u8,
        /// Running mean of all confidences seen so far in this round (including this one).
        running_average: f64,
    },
    DisagreementDetected {
        round: u32,
        disagreement: Disagreement,
    },
    RoundComplete {
        round: u32,
        phase: Phase,
        average_confidence: f64,
        stddev: f64,
        score: i32,
        disagreements: Vec<Disagreement>,
        responses: Vec<ParticipantResponse>,
        duration_ms: i64,
    },
    EarlyStop {
        round: u32,
        delta: f64,
        reason: String,
    },
    SynthesisStart {
        model_id: String,
    },
    SynthesisToken {
        token: String,
    },
    SynthesisComplete {
        synthesis: SynthesisResult,
    },
    FinalResult {
        result: crate::types::ConsensusResult,
    },
    Error {
        message: String,
    },
}

#[async_trait]
pub trait EventSubscriber: Send + Sync {
    async fn on_event(&self, event: &ConsensusEvent);
}

/// Event bus for a single consensus run. Cheap to construct per-run —
/// unlike `flare-workflow`'s long-lived engine-wide bus, this one is scoped
/// to one `ConsensusEngine::run` call.
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

    pub async fn subscribe(&self, subscriber: Arc<dyn EventSubscriber>) {
        self.subscribers.write().await.push(subscriber);
    }

    /// Fire-and-forget: notify every subscriber in a spawned task.
    pub async fn publish(&self, event: ConsensusEvent) {
        let subscribers: Vec<_> = self.subscribers.read().await.iter().cloned().collect();
        let timeout = self.subscriber_timeout;

        for (idx, subscriber) in subscribers.into_iter().enumerate() {
            let event = event.clone();
            tokio::spawn(async move {
                if tokio::time::timeout(timeout, subscriber.on_event(&event))
                    .await
                    .is_err()
                {
                    warn!(
                        subscriber_index = idx,
                        timeout_secs = timeout.as_secs(),
                        "consensus event subscriber timed out"
                    );
                }
            });
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

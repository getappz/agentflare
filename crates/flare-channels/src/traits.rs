//! Transport-agnostic traits.
//!
//! The crate owns the contracts; the binary owns the secrets, sessions, and
//! agent runs. Transports depend only on these traits, so `flare-channels`
//! never imports `vault`, `agent_launch`, or MCP types.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{ChannelEvent, ChannelMessage, LifecycleReaction, SendMessage};

/// Errors surfaced by channel operations.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("transport error on {channel}: {message}")]
    Transport { channel: String, message: String },
    /// HTTP 429: retry after `retry_after_secs`. Callers back off instead
    /// of treating this as fatal (OpenFang honors `retry_after` the same way).
    #[error("rate limited on {channel}: retry after {retry_after_secs}s")]
    RateLimited {
        channel: String,
        retry_after_secs: u64,
    },
    #[error("unknown channel: {0}")]
    UnknownChannel(String),
    #[error("auth rejected sender {sender} on {channel}")]
    Unauthorized { channel: String, sender: String },
}

pub type ChannelResult<T> = Result<T, ChannelError>;

/// The contract every messaging integration implements (ZeroClaw `Channel`).
#[async_trait]
pub trait Channel: Send + Sync {
    /// Unique channel name (`telegram`, `slack`, ...). Used for routing/logs.
    fn name(&self) -> &'static str;

    /// Dispatch one outbound message.
    async fn send(&self, message: SendMessage) -> ChannelResult<()>;

    /// Dispatch a terminal response (bypasses streaming/draft handling).
    /// Default: plain [`Channel::send`].
    async fn send_final(&self, message: SendMessage) -> ChannelResult<()> {
        self.send(message).await
    }

    /// Update a previously sent draft in place (streaming preview).
    /// Default: no-op for transports without edit support.
    async fn finalize_draft(
        &self,
        _target: &str,
        _message_id: &str,
        _text: &str,
    ) -> ChannelResult<()> {
        Ok(())
    }

    /// Start receiving: forward normalized [`ChannelMessage`]s to `tx`.
    /// Runs until cancelled or fatally errored.
    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> ChannelResult<()> {
        let _ = tx;
        Ok(())
    }

    /// Show a typing indicator (or phase equivalent) on `target`.
    /// Default: no-op for transports without one.
    async fn send_typing(&self, _target: &str) -> ChannelResult<()> {
        Ok(())
    }

    /// Show a lifecycle reaction on an already-sent message.
    /// Default: no-op for transports without reactions.
    async fn send_reaction(
        &self,
        _target: &str,
        _message_id: &str,
        _reaction: LifecycleReaction,
    ) -> ChannelResult<()> {
        Ok(())
    }

    /// Current health snapshot. Default: disconnected (transports with
    /// runtime stats override this).
    fn status(&self) -> crate::ChannelStatus {
        crate::ChannelStatus::default()
    }

    /// Publish a lifecycle event (typing/draft/settled) for realtime fans.
    /// Default: no-op; the bus-owning host overrides this.
    async fn emit(&self, _event: ChannelEvent) -> ChannelResult<()> {
        Ok(())
    }
}

/// Supplies a bot token without the crate touching the secret store.
pub trait TokenProvider: Send + Sync {
    fn token_for(&self, channel: &str) -> Option<String>;
}

/// Persists per-target agent session ids (`{target: session_id}`).
pub trait SessionStore: Send + Sync {
    fn load(&self) -> std::collections::HashMap<String, String>;
    /// Returns true when the map actually changed.
    fn save(&self, target: &str, session_id: &str) -> bool;
    fn remove(&self, target: &str) -> bool;
}

/// Runs one headless agent turn; implemented by the host binary.
pub trait AgentRunner: Send + Sync {
    fn run_turn(&self, prompt: &str, resume_session: Option<&str>) -> AgentOutcome;
}

/// Outcome of one [`AgentRunner::run_turn`] call.
#[derive(Debug, Clone)]
pub enum AgentOutcome {
    Ok {
        text: String,
        session_id: Option<String>,
    },
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MapStore(Mutex<HashMap<String, String>>);

    impl SessionStore for MapStore {
        fn load(&self) -> HashMap<String, String> {
            self.0.lock().expect("lock").clone()
        }
        fn save(&self, target: &str, session_id: &str) -> bool {
            self.0
                .lock()
                .expect("lock")
                .insert(target.to_string(), session_id.to_string());
            true
        }
        fn remove(&self, target: &str) -> bool {
            self.0.lock().expect("lock").remove(target).is_some()
        }
    }

    #[test]
    fn session_store_contract() {
        let s = MapStore(Mutex::new(HashMap::new()));
        assert!(s.save("t", "s1"));
        assert_eq!(s.load().get("t").map(String::as_str), Some("s1"));
        assert!(s.remove("t"));
        assert!(!s.remove("t"));
    }
}

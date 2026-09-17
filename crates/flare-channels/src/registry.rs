//! Named registry of channel handles.
//!
//! Lets hosts configure multiple channels/aliases (`telegram.home`, ...)
//! and route by name without `match`ing platform strings at every call site.

use std::collections::HashMap;
use std::sync::Arc;

use crate::{Channel, ChannelError, ChannelResult, SendMessage};

/// Thread-safe handle to one channel implementation.
pub type ChannelHandle = Arc<dyn Channel>;

/// Named lookup over the configured channels.
#[derive(Default)]
pub struct ChannelRegistry {
    channels: HashMap<String, ChannelHandle>,
}

impl ChannelRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    /// Register under `alias` (e.g. `telegram.home` or plain `telegram`).
    pub fn register(&mut self, alias: impl Into<String>, channel: ChannelHandle) {
        self.channels.insert(alias.into(), channel);
    }

    /// Look up a channel by alias.
    #[must_use]
    pub fn get(&self, alias: &str) -> Option<ChannelHandle> {
        self.channels.get(alias).cloned()
    }

    /// Send via the named channel. Errors [`ChannelError::UnknownChannel`]
    /// instead of panicking on a typo'd alias.
    pub async fn send(&self, alias: &str, message: SendMessage) -> ChannelResult<()> {
        let channel = self
            .get(alias)
            .ok_or_else(|| ChannelError::UnknownChannel(alias.to_string()))?;
        channel
            .send(message)
            .await
            .map_err(|e| ChannelError::Transport {
                channel: alias.to_string(),
                message: e.to_string(),
            })
    }

    /// Registered aliases in sorted order (stable CLI/doctor output).
    #[must_use]
    pub fn aliases(&self) -> Vec<String> {
        let mut names: Vec<String> = self.channels.keys().cloned().collect();
        names.sort();
        names
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMessage;
    use tokio::sync::mpsc;

    struct Noop(&'static str);

    #[async_trait::async_trait]
    impl Channel for Noop {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn send(&self, _message: SendMessage) -> ChannelResult<()> {
            Ok(())
        }
        async fn listen(&self, _tx: mpsc::Sender<ChannelMessage>) -> ChannelResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn send_routes_by_alias_and_rejects_unknown() {
        let mut r = ChannelRegistry::new();
        assert!(r.is_empty());
        r.register("telegram.home", Arc::new(Noop("telegram")));
        assert_eq!(r.aliases(), vec!["telegram.home".to_string()]);
        r.send("telegram.home", SendMessage::new("t", "hi"))
            .await
            .expect("known alias sends");
        let err = r
            .send("slack", SendMessage::new("t", "hi"))
            .await
            .expect_err("unknown alias errors");
        assert!(matches!(err, ChannelError::UnknownChannel(_)));
    }
}

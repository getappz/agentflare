//! Realtime fan-out hub.
//!
//! Same shape as the dashboard's `snapshot_broadcaster`: one shared
//! `tokio::sync::broadcast` channel so N subscribers (dashboard SSE,
//! CLI, future gateway WS) cost one publish path rather than N polls.

use tokio::sync::broadcast;

use crate::ChannelEvent;

/// Default lag buffer per subscriber. Draft updates supersede each other,
// so a small buffer with lag-drop (like the dashboard `/events` handler)
// is the right trade-off over unbounded memory.
pub const DEFAULT_BUS_CAPACITY: usize = 64;

/// Shared realtime bus for [`ChannelEvent`]s.
#[derive(Debug, Clone)]
pub struct ChatBus {
    tx: broadcast::Sender<ChannelEvent>,
}

impl ChatBus {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// Publish an event. Only fails when nobody is listening; that is
    /// intentionally not an error for fire-and-forget lifecycle hints.
    pub fn publish(&self, event: ChannelEvent) {
        let _ = self.tx.send(event);
    }

    /// Subscribe to the event stream.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ChannelEvent> {
        self.tx.subscribe()
    }

    /// Current subscriber count (useful for idle-skip, like the dashboard).
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl Default for ChatBus {
    fn default() -> Self {
        Self::new(DEFAULT_BUS_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMessage;

    #[test]
    fn publish_reaches_subscriber() {
        let bus = ChatBus::new(8);
        let mut rx = bus.subscribe();
        let msg = ChannelMessage::text("1", "s", "t", "hi", "telegram");
        bus.publish(ChannelEvent::Inbound(msg.clone()));
        let got = rx.try_recv().expect("event");
        assert_eq!(got, ChannelEvent::Inbound(msg));
    }

    #[test]
    fn publish_without_listeners_is_not_an_error() {
        let bus = ChatBus::new(8);
        assert_eq!(bus.receiver_count(), 0);
        bus.publish(ChannelEvent::Settled {
            channel: "telegram".to_string(),
            id: "1".to_string(),
        });
    }
}

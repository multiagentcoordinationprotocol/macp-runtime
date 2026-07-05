use crate::pb::Envelope;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

const DEFAULT_SESSION_STREAM_CAPACITY: usize = 256;

pub struct SessionStreamBus {
    channels: Mutex<HashMap<String, broadcast::Sender<Envelope>>>,
    capacity: usize,
}

impl Default for SessionStreamBus {
    fn default() -> Self {
        Self::new(DEFAULT_SESSION_STREAM_CAPACITY)
    }
}

impl SessionStreamBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            capacity,
        }
    }

    pub fn subscribe(&self, session_id: &str) -> broadcast::Receiver<Envelope> {
        let mut guard = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .entry(session_id.to_string())
            .or_insert_with(|| {
                let (sender, _receiver) = broadcast::channel(self.capacity);
                sender
            })
            .subscribe()
    }

    pub fn publish(&self, session_id: &str, envelope: Envelope) {
        let sender = {
            let guard = self.channels.lock().unwrap_or_else(|e| e.into_inner());
            guard.get(session_id).cloned()
        };
        if let Some(sender) = sender {
            let _ = sender.send(envelope);
        }
    }

    /// Remove a session's broadcast channel if it has no live receivers.
    /// Channels are created lazily on subscribe and were previously never
    /// removed — every session ever streamed pinned a map entry (and a
    /// lagging receiver pins up to `capacity` buffered envelopes) for the
    /// process lifetime. Called from session eviction; a channel with active
    /// receivers is left in place (`false`) and retried on the next sweep.
    pub fn remove_if_unused(&self, session_id: &str) -> bool {
        let mut guard = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get(session_id) {
            Some(sender) if sender.receiver_count() == 0 => {
                guard.remove(session_id);
                true
            }
            Some(_) => false,
            None => true,
        }
    }

    /// Number of live channels (observability / tests).
    pub fn channel_count(&self) -> usize {
        self.channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(message_id: &str) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: "Proposal".into(),
            message_id: message_id.into(),
            session_id: "s1".into(),
            sender: "agent://sender".into(),
            timestamp_unix_ms: 1,
            payload: vec![],
        }
    }

    #[test]
    fn subscribe_then_publish_round_trip() {
        let bus = SessionStreamBus::default();
        let mut rx = bus.subscribe("s1");
        bus.publish("s1", env("m1"));
        let envelope = rx.try_recv().expect("stream event");
        assert_eq!(envelope.message_id, "m1");
    }

    #[test]
    fn remove_if_unused_respects_live_receivers() {
        let bus = SessionStreamBus::default();
        let rx = bus.subscribe("s1");
        assert_eq!(bus.channel_count(), 1);

        // Live receiver: channel must survive eviction attempts.
        assert!(!bus.remove_if_unused("s1"));
        assert_eq!(bus.channel_count(), 1);

        // Receiver dropped: channel is removable.
        drop(rx);
        assert!(bus.remove_if_unused("s1"));
        assert_eq!(bus.channel_count(), 0);

        // Unknown session is trivially "removed".
        assert!(bus.remove_if_unused("nope"));
    }
}

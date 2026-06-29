//! In-process publish/subscribe used to push live updates to browsers.
//!
//! Handlers that change the queue call `publish`; each SSE connection holds a
//! `subscribe` receiver. SSE (one-way HTTP) is used instead of WebSockets
//! because updates only flow server to client. To scale across multiple
//! instances, replace this with Redis Pub/Sub or NATS.

use tokio::sync::broadcast;

#[derive(Clone)]
pub struct Broker {
    tx: broadcast::Sender<String>,
}

impl Broker {
    /// `capacity` is how many messages a slow subscriber may fall behind before
    /// it starts dropping the oldest ones.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Push a message to every subscriber. The payload is a small, PII-free
    /// nudge; clients then refetch the data they are allowed to see. An error
    /// here just means nobody is listening, which is fine.
    pub fn publish(&self, message: impl Into<String>) {
        let _ = self.tx.send(message.into());
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new(256)
    }
}

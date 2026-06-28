//! The message broker.
//!
//! This is the piece that lets the customer app receive live updates
//! *without* polling and *without* a two-way WebSocket.
//!
//! How it works:
//!   * Internally it is a publish/subscribe channel (`tokio::sync::broadcast`).
//!     Any handler that changes the queue calls `broker.publish(...)`.
//!   * Browsers subscribe over **Server-Sent Events (SSE)** — a single,
//!     lightweight, one-way HTTP stream (`GET /api/events`). The browser's
//!     built-in `EventSource` reconnects automatically if the link drops.
//!
//! SSE was chosen over WebSockets on purpose: updates only ever flow
//! server -> client, so we don't need a full duplex socket. It is just HTTP,
//! so it sails through proxies and load balancers with no special handling.
//!
//! Want to scale to multiple backend instances? Swap the in-process
//! `broadcast` channel here for Redis Pub/Sub or NATS — nothing else in the
//! codebase needs to change. See CUSTOMIZE.md.

use tokio::sync::broadcast;

/// A clonable handle to the pub/sub channel. Cloning is cheap; every part of
/// the app shares the same underlying channel.
#[derive(Clone)]
pub struct Broker {
    tx: broadcast::Sender<String>,
}

impl Broker {
    /// Create a broker. `capacity` is how many messages a slow subscriber may
    /// fall behind before it starts dropping the oldest ones.
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Push a message to every connected subscriber.
    ///
    /// The payload is a small JSON string such as `{"type":"update"}`. We keep
    /// it deliberately tiny and PII-free — it is only a *nudge*. On receiving
    /// it, the browser re-fetches just the data it is allowed to see.
    pub fn publish(&self, message: impl Into<String>) {
        // An error here only means "nobody is currently listening", which is
        // perfectly fine — there is nothing to do.
        let _ = self.tx.send(message.into());
    }

    /// Subscribe a new SSE connection to the stream.
    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new(256)
    }
}

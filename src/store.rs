//! The data store: every guest in the queue, the running number per queue
//! type, and which number is being served right now.
//!
//! State is kept in memory. After every change it publishes a `Snapshot` to a
//! background task that writes it to the configured storage backend (JSON file
//! by default; SQLite/Postgres/Mongo optionally — see `storage.rs`). A restart
//! reloads the snapshot and resumes where it left off.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;

/// Where a guest is in their journey through the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Waiting,
    Serving,
    Done,
    Skipped,
    NoShow,
}

/// One guest in the queue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Opaque, unguessable id. Doubles as the customer's private access token —
    /// it is what goes in their link/QR. Knowing it lets you see only this
    /// one guest's status, never anyone else's details.
    pub id: String,
    pub type_code: String,
    pub number: u32,
    /// Display label, e.g. "A02".
    pub label: String,
    /// Whatever the operator typed (name, phone, …). Driven by config fields.
    pub fields: Map<String, Value>,
    pub status: Status,
    pub created_at: u64,
    pub called_at: Option<u64>,
}

/// The serializable snapshot of all queue data. This is exactly what every
/// storage backend reads/writes (see `storage.rs`) and what the admin backup
/// export/import uses. Its JSON shape is backward-compatible with the old
/// `data.json`.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub counters: HashMap<String, u32>,
    #[serde(default)]
    pub serving: HashMap<String, Option<u32>>,
}

/// The full in-memory state. Reads/writes happen here (fast); persistence is
/// delegated to a pluggable backend via a `watch` channel — every mutation
/// publishes a fresh `Snapshot` that a background task saves.
#[derive(Default)]
pub struct Store {
    pub entries: Vec<Entry>,
    /// type code -> last issued number
    pub counters: HashMap<String, u32>,
    /// type code -> number currently being served (None = nobody yet)
    pub serving: HashMap<String, Option<u32>>,
    /// Set at startup; each mutation sends a snapshot here for persistence.
    tx: Option<watch::Sender<Snapshot>>,
}

/// Customer-safe projection of an entry — deliberately contains NO personal
/// data, only what the guest needs to watch their place in line.
#[derive(Serialize)]
pub struct PublicEntry {
    pub id: String,
    pub label: String,
    pub type_code: String,
    pub type_name: String,
    pub status: Status,
    pub ahead: usize,
    pub current_serving: Option<String>,
    pub total_waiting: usize,
    pub created_at: u64,
    /// True when the ticket is older than the configured TTL.
    pub expired: bool,
}

/// Public, per-queue-type snapshot for the customer "now serving" board.
#[derive(Serialize)]
pub struct TypeState {
    pub code: String,
    pub name: String,
    pub current_serving: Option<String>,
    pub waiting: usize,
    /// Labels of up to the next 5 waiting guests, lowest number first. Labels
    /// carry no personal data, so this is safe on the unauthenticated board.
    pub next_waiting: Vec<String>,
}

/// How many upcoming labels `TypeState.next_waiting` shows.
const NEXT_WAITING_LIMIT: usize = 5;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Zero-pads to 2 digits (`A01`, `A42`); numbers past 99 simply widen the
/// label (`A100`) rather than truncating, so this never loses information.
fn make_label(code: &str, number: u32) -> String {
    format!("{code}{number:02}")
}

/// A ticket is expired once it is older than `ttl_secs`. `ttl_secs == 0` means
/// tickets never expire.
fn is_expired(created_at: u64, ttl_secs: u64, now: u64) -> bool {
    ttl_secs > 0 && now > created_at.saturating_add(ttl_secs.saturating_mul(1000))
}

impl Store {
    /// Build the in-memory store from a snapshot loaded by a storage backend.
    pub fn from_snapshot(snap: Snapshot) -> Self {
        Store { entries: snap.entries, counters: snap.counters, serving: snap.serving, tx: None }
    }

    /// Clone the current state into a `Snapshot` (for persistence / backup).
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self.entries.clone(),
            counters: self.counters.clone(),
            serving: self.serving.clone(),
        }
    }

    /// Wire up the persistence channel. After this, every mutation publishes a
    /// snapshot to `tx` for the background saver to write.
    pub fn set_sender(&mut self, tx: watch::Sender<Snapshot>) {
        self.tx = Some(tx);
    }

    /// Add a new guest to a queue type and return their entry.
    pub fn create(&mut self, code: &str, fields: Map<String, Value>) -> Entry {
        let counter = self.counters.entry(code.to_string()).or_insert(0);
        *counter += 1;
        let number = *counter;
        let entry = Entry {
            id: nanoid::nanoid!(),
            type_code: code.to_string(),
            number,
            label: make_label(code, number),
            fields,
            status: Status::Waiting,
            created_at: now_ms(),
            called_at: None,
        };
        self.entries.push(entry.clone());
        self.persist();
        entry
    }

    /// Complete whoever is being served in `code`, then call the lowest-
    /// numbered waiting guest. Returns the id of the newly-called guest, if any.
    pub fn call_next(&mut self, code: &str, ttl_secs: u64) -> Option<String> {
        let now = now_ms();
        // Finish the current one.
        if let Some(Some(cur)) = self.serving.get(code).copied() {
            if let Some(e) = self
                .entries
                .iter_mut()
                .find(|e| e.type_code == code && e.number == cur && e.status == Status::Serving)
            {
                e.status = Status::Done;
            }
        }

        // Find the next waiting guest (lowest number), skipping expired tickets.
        let next_number = self
            .entries
            .iter()
            .filter(|e| {
                e.type_code == code
                    && e.status == Status::Waiting
                    && !is_expired(e.created_at, ttl_secs, now)
            })
            .map(|e| e.number)
            .min();

        match next_number {
            Some(n) => {
                let now = now_ms();
                let id = {
                    let e = self
                        .entries
                        .iter_mut()
                        .find(|e| e.type_code == code && e.number == n)
                        .expect("number just found above");
                    e.status = Status::Serving;
                    e.called_at = Some(now);
                    e.id.clone()
                };
                self.serving.insert(code.to_string(), Some(n));
                self.persist();
                Some(id)
            }
            None => {
                self.serving.insert(code.to_string(), None);
                self.persist();
                None
            }
        }
    }

    /// Force a specific entry into a status (skip, recall, mark serving, …).
    pub fn set_status(&mut self, id: &str, status: Status) -> bool {
        let Some((code, number)) =
            self.entries.iter().find(|e| e.id == id).map(|e| (e.type_code.clone(), e.number))
        else {
            return false;
        };

        // Promoting this entry to "serving" demotes whoever was serving before.
        if status == Status::Serving {
            if let Some(Some(cur)) = self.serving.get(&code).copied() {
                if cur != number {
                    if let Some(e) = self.entries.iter_mut().find(|e| {
                        e.type_code == code && e.number == cur && e.status == Status::Serving
                    }) {
                        e.status = Status::Done;
                    }
                }
            }
        }

        let now = now_ms();
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.status = status;
            if status == Status::Serving {
                e.called_at = Some(now);
            }
        }

        if status == Status::Serving {
            self.serving.insert(code, Some(number));
        } else if let Some(Some(cur)) = self.serving.get(&code).copied() {
            // We just moved the currently-served guest elsewhere; clear the slot.
            if cur == number {
                self.serving.insert(code, None);
            }
        }

        self.persist();
        true
    }

    /// Start a queue type fresh (e.g. a new day): drop its entries and reset
    /// its counter. Pass `None` to reset everything.
    pub fn reset(&mut self, code: Option<&str>) {
        match code {
            Some(c) => {
                self.entries.retain(|e| e.type_code != c);
                self.counters.insert(c.to_string(), 0);
                self.serving.insert(c.to_string(), None);
            }
            None => {
                self.entries.clear();
                self.counters.clear();
                self.serving.clear();
            }
        }
        self.persist();
    }

    pub fn waiting_count(&self, code: &str) -> usize {
        self.entries.iter().filter(|e| e.type_code == code && e.status == Status::Waiting).count()
    }

    pub fn current_serving_label(&self, code: &str) -> Option<String> {
        self.serving.get(code).copied().flatten().map(|n| make_label(code, n))
    }

    /// Build the customer-safe view for one guest. `ttl_secs` (0 = never) marks
    /// tickets older than the TTL as expired; expired waiting guests are not
    /// counted as "ahead".
    pub fn public_view(&self, id: &str, cfg: &Config, ttl_secs: u64) -> Option<PublicEntry> {
        let e = self.entries.iter().find(|e| e.id == id)?;
        let now = now_ms();
        let ahead = self
            .entries
            .iter()
            .filter(|o| {
                o.type_code == e.type_code
                    && o.status == Status::Waiting
                    && o.number < e.number
                    && !is_expired(o.created_at, ttl_secs, now)
            })
            .count();
        Some(PublicEntry {
            id: e.id.clone(),
            label: e.label.clone(),
            type_code: e.type_code.clone(),
            type_name: cfg.type_name(&e.type_code),
            status: e.status,
            ahead,
            current_serving: self.current_serving_label(&e.type_code),
            total_waiting: self.waiting_count(&e.type_code),
            created_at: e.created_at,
            expired: is_expired(e.created_at, ttl_secs, now),
        })
    }

    /// Public per-type board (no personal data). `ttl_secs` (0 = never)
    /// excludes expired tickets from `next_waiting`.
    pub fn state(&self, cfg: &Config, ttl_secs: u64) -> Vec<TypeState> {
        let now = now_ms();
        cfg.queue_types
            .iter()
            .map(|t| {
                let mut next: Vec<&Entry> = self
                    .entries
                    .iter()
                    .filter(|e| {
                        e.type_code == t.code
                            && e.status == Status::Waiting
                            && !is_expired(e.created_at, ttl_secs, now)
                    })
                    .collect();
                next.sort_by_key(|e| e.number);
                let next_waiting =
                    next.into_iter().take(NEXT_WAITING_LIMIT).map(|e| e.label.clone()).collect();

                TypeState {
                    code: t.code.clone(),
                    name: t.name.clone(),
                    current_serving: self.current_serving_label(&t.code),
                    waiting: self.waiting_count(&t.code),
                    next_waiting,
                }
            })
            .collect()
    }

    /// Serialize the current state to pretty JSON — the admin "download backup".
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&self.snapshot()).unwrap_or_else(|_| "{}".to_string())
    }

    /// Replace all data from a backup produced by `export_json`.
    pub fn import_json(&mut self, json: &str) -> Result<(), String> {
        let incoming: Snapshot = serde_json::from_str(json).map_err(|e| e.to_string())?;
        self.entries = incoming.entries;
        self.counters = incoming.counters;
        self.serving = incoming.serving;
        self.persist();
        Ok(())
    }

    /// Publish the latest snapshot to the persistence task (if wired). The
    /// actual write to the chosen backend happens there, off the request path.
    fn persist(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send_replace(self.snapshot());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    fn fields(obj: Value) -> Map<String, Value> {
        obj.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn make_label_padding() {
        assert_eq!(make_label("A", 7), "A07");
        assert_eq!(make_label("A", 42), "A42");
        assert_eq!(make_label("A", 123), "A123");
    }

    #[test]
    fn create_assigns_sequential_numbers_per_type() {
        let mut store = Store::default();
        let a1 = store.create("A", Map::new());
        let a2 = store.create("A", Map::new());
        let b1 = store.create("B", Map::new());
        assert_eq!(a1.label, "A01");
        assert_eq!(a2.label, "A02");
        assert_eq!(b1.label, "B01");
    }

    #[test]
    fn call_next_orders_by_lowest_number() {
        let mut store = Store::default();
        let a1 = store.create("A", Map::new());
        let a2 = store.create("A", Map::new());

        let called = store.call_next("A", 0);
        assert_eq!(called, Some(a1.id.clone()));
        assert_eq!(store.entries.iter().find(|e| e.id == a1.id).unwrap().status, Status::Serving);

        let called2 = store.call_next("A", 0);
        assert_eq!(called2, Some(a2.id.clone()));
        assert_eq!(store.entries.iter().find(|e| e.id == a1.id).unwrap().status, Status::Done);
        assert_eq!(store.entries.iter().find(|e| e.id == a2.id).unwrap().status, Status::Serving);
    }

    #[test]
    fn call_next_empty_returns_none_and_clears_serving() {
        let mut store = Store::default();
        assert_eq!(store.call_next("A", 0), None);
        assert_eq!(store.serving.get("A").copied().flatten(), None);
    }

    #[test]
    fn call_next_skips_expired_tickets() {
        let mut store = Store::default();
        let a1 = store.create("A", Map::new());
        store.entries.iter_mut().find(|e| e.id == a1.id).unwrap().created_at = 0;
        assert_eq!(store.call_next("A", 1), None);
    }

    #[test]
    fn set_status_serving_demotes_previous() {
        let mut store = Store::default();
        let a1 = store.create("A", Map::new());
        let a2 = store.create("A", Map::new());
        store.set_status(&a1.id, Status::Serving);
        assert_eq!(store.serving.get("A").copied().flatten(), Some(1));
        store.set_status(&a2.id, Status::Serving);
        assert_eq!(store.entries.iter().find(|e| e.id == a1.id).unwrap().status, Status::Done);
        assert_eq!(store.serving.get("A").copied().flatten(), Some(2));
    }

    #[test]
    fn set_status_away_from_serving_clears_slot() {
        let mut store = Store::default();
        let a1 = store.create("A", Map::new());
        store.set_status(&a1.id, Status::Serving);
        assert_eq!(store.serving.get("A").copied().flatten(), Some(1));
        store.set_status(&a1.id, Status::Done);
        assert_eq!(store.serving.get("A").copied().flatten(), None);
    }

    #[test]
    fn set_status_unknown_id_returns_false() {
        let mut store = Store::default();
        assert!(!store.set_status("nope", Status::Done));
    }

    #[test]
    fn recall_flow() {
        let mut store = Store::default();
        let a1 = store.create("A", Map::new());
        assert!(store.set_status(&a1.id, Status::Skipped));
        assert_eq!(store.entries[0].status, Status::Skipped);
        assert!(store.set_status(&a1.id, Status::Waiting));
        assert_eq!(store.entries[0].status, Status::Waiting);
        assert_eq!(store.call_next("A", 0), Some(a1.id));
    }

    #[test]
    fn reset_single_type_keeps_others() {
        let mut store = Store::default();
        store.create("A", Map::new());
        store.create("B", Map::new());
        store.reset(Some("A"));
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].type_code, "B");
        assert_eq!(store.counters.get("A").copied(), Some(0));
    }

    #[test]
    fn reset_all_clears_everything() {
        let mut store = Store::default();
        store.create("A", Map::new());
        store.create("B", Map::new());
        store.reset(None);
        assert!(store.entries.is_empty());
        assert!(store.counters.is_empty());
        assert!(store.serving.is_empty());
    }

    #[test]
    fn public_view_ahead_excludes_expired_and_higher_numbers() {
        let mut store = Store::default();
        let cfg = Config::default();
        let a1 = store.create("A", Map::new());
        store.create("A", Map::new());
        let a3 = store.create("A", Map::new());
        store.entries.iter_mut().find(|e| e.id == a1.id).unwrap().created_at = 0;

        let view = store.public_view(&a3.id, &cfg, 1).unwrap();
        assert_eq!(view.ahead, 1); // a1 expired (excluded), a2 still counts
    }

    #[test]
    fn public_view_expired_flag() {
        let mut store = Store::default();
        let cfg = Config::default();
        let a1 = store.create("A", Map::new());
        store.entries.iter_mut().find(|e| e.id == a1.id).unwrap().created_at = 0;

        let view = store.public_view(&a1.id, &cfg, 1).unwrap();
        assert!(view.expired);
    }

    #[test]
    fn export_import_roundtrip() {
        let mut store = Store::default();
        store.create("A", fields(json!({ "name": "Alice" })));

        let snapshot_json = store.export_json();
        let mut restored = Store::default();
        restored.import_json(&snapshot_json).unwrap();

        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].fields.get("name").unwrap(), "Alice");
    }

    #[test]
    fn state_next_waiting_ordered_and_excludes_expired() {
        let mut store = Store::default();
        let cfg = Config::default();
        let a1 = store.create("A", Map::new());
        let a2 = store.create("A", Map::new());
        let a3 = store.create("A", Map::new());
        store.entries.iter_mut().find(|e| e.id == a2.id).unwrap().created_at = 0;

        let state = store.state(&cfg, 1);
        let a_state = state.iter().find(|s| s.code == "A").unwrap();
        assert_eq!(a_state.next_waiting, vec![a1.label.clone(), a3.label.clone()]);
    }
}

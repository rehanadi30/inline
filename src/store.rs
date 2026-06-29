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
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

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
        Store {
            entries: snap.entries,
            counters: snap.counters,
            serving: snap.serving,
            tx: None,
        }
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
        let Some((code, number)) = self
            .entries
            .iter()
            .find(|e| e.id == id)
            .map(|e| (e.type_code.clone(), e.number))
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
        self.entries
            .iter()
            .filter(|e| e.type_code == code && e.status == Status::Waiting)
            .count()
    }

    pub fn current_serving_label(&self, code: &str) -> Option<String> {
        self.serving
            .get(code)
            .copied()
            .flatten()
            .map(|n| make_label(code, n))
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

    /// Public per-type board (no personal data).
    pub fn state(&self, cfg: &Config) -> Vec<TypeState> {
        cfg.queue_types
            .iter()
            .map(|t| TypeState {
                code: t.code.clone(),
                name: t.name.clone(),
                current_serving: self.current_serving_label(&t.code),
                waiting: self.waiting_count(&t.code),
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

//! The data store: every guest in the queue, the running number per queue
//! type, and which number is being served right now.
//!
//! It is kept entirely in memory (fast and dead simple) and snapshotted to a
//! JSON file after every change, so a restart picks up exactly where it left
//! off. For a host-stand queue the write volume is tiny, so this is plenty —
//! no database to run. If you outgrow it, swap `persist()` for SQLite/Postgres.

use crate::config::Config;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// The full in-memory state.
#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    pub entries: Vec<Entry>,
    /// type code -> last issued number
    pub counters: HashMap<String, u32>,
    /// type code -> number currently being served (None = nobody yet)
    pub serving: HashMap<String, Option<u32>>,
    #[serde(skip)]
    data_file: String,
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

impl Store {
    /// Load the snapshot from disk, or start empty if there is none.
    pub fn load(data_file: &str) -> Self {
        let mut store = std::fs::read_to_string(data_file)
            .ok()
            .and_then(|t| serde_json::from_str::<Store>(&t).ok())
            .unwrap_or_default();
        store.data_file = data_file.to_string();
        if !data_file.is_empty() {
            println!("[store] using data file {data_file} ({} entries)", store.entries.len());
        }
        store
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
    pub fn call_next(&mut self, code: &str) -> Option<String> {
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

        // Find the next waiting guest (lowest number).
        let next_number = self
            .entries
            .iter()
            .filter(|e| e.type_code == code && e.status == Status::Waiting)
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

    /// How many waiting guests sit ahead of this one in the same type.
    pub fn ahead_of(&self, entry: &Entry) -> usize {
        self.entries
            .iter()
            .filter(|o| {
                o.type_code == entry.type_code
                    && o.status == Status::Waiting
                    && o.number < entry.number
            })
            .count()
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

    /// Build the customer-safe view for one guest.
    pub fn public_view(&self, id: &str, cfg: &Config) -> Option<PublicEntry> {
        let e = self.entries.iter().find(|e| e.id == id)?;
        Some(PublicEntry {
            id: e.id.clone(),
            label: e.label.clone(),
            type_code: e.type_code.clone(),
            type_name: cfg.type_name(&e.type_code),
            status: e.status,
            ahead: self.ahead_of(e),
            current_serving: self.current_serving_label(&e.type_code),
            total_waiting: self.waiting_count(&e.type_code),
            created_at: e.created_at,
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

    /// Atomically write the snapshot to disk. Best-effort: a failure logs but
    /// never crashes the server.
    fn persist(&self) {
        if self.data_file.is_empty() {
            return;
        }
        let json = match serde_json::to_string_pretty(self) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[store] serialize error: {e}");
                return;
            }
        };
        let tmp = format!("{}.tmp", self.data_file);
        let ok = std::fs::write(&tmp, json)
            .and_then(|_| std::fs::rename(&tmp, &self.data_file))
            .is_ok();
        if !ok {
            eprintln!("[store] failed to persist to {}", self.data_file);
        }
    }
}

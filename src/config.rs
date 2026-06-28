//! Loads `config.json` — the file a business edits to describe *their* queue:
//! the brand name, the queue types (Small/Medium/Large table, etc.) and the
//! fields shown on the operator's "add guest" form.
//!
//! Both apps fetch this at runtime via `GET /api/config`, so changing the
//! queue requires only editing this JSON file and restarting — no recompile.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// One queue type. Each has its own independent running number; `code` is the
/// label prefix, so code "A" produces labels A01, A02, A03 …
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueueType {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// One field on the operator's "add guest" form. This is what makes the form
/// flexible — add or remove entries and the form rebuilds itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldDef {
    pub key: String,
    pub label: String,
    /// text | tel | number | email | textarea | select
    #[serde(default = "default_field_type")]
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    /// Only used when `type` is "select".
    #[serde(default)]
    pub options: Vec<String>,
}

fn default_field_type() -> String {
    "text".to_string()
}

/// The whole queue definition.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_brand")]
    pub brand: String,
    #[serde(default)]
    pub tagline: String,
    pub queue_types: Vec<QueueType>,
    pub fields: Vec<FieldDef>,
}

fn default_brand() -> String {
    "inline".to_string()
}

impl Config {
    /// Load from `path`. If the file is missing we fall back to a sensible
    /// built-in default so the app always starts.
    pub fn load(path: &str) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Config>(&text) {
                Ok(cfg) => {
                    println!("[config] loaded {} ({} queue types)", path, cfg.queue_types.len());
                    cfg
                }
                Err(e) => {
                    eprintln!("[config] {path} is invalid JSON ({e}); using defaults");
                    Self::default()
                }
            },
            Err(_) => {
                eprintln!("[config] {path} not found; using built-in defaults");
                Self::default()
            }
        }
    }

    /// Is this a known queue type code?
    pub fn has_type(&self, code: &str) -> bool {
        self.queue_types.iter().any(|t| t.code == code)
    }

    /// Human name for a code, falling back to the code itself.
    pub fn type_name(&self, code: &str) -> String {
        self.queue_types
            .iter()
            .find(|t| t.code == code)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| code.to_string())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            brand: default_brand(),
            tagline: "Please wait for your number".to_string(),
            queue_types: vec![
                QueueType { code: "A".into(), name: "Small table".into(), description: "1–2 guests".into() },
                QueueType { code: "B".into(), name: "Medium table".into(), description: "3–5 guests".into() },
                QueueType { code: "C".into(), name: "Large table".into(), description: "6+ guests".into() },
            ],
            fields: vec![
                FieldDef { key: "name".into(), label: "Name".into(), r#type: "text".into(), required: true, options: vec![] },
                FieldDef { key: "phone".into(), label: "Phone number".into(), r#type: "tel".into(), required: false, options: vec![] },
            ],
        }
    }
}

/// Tiny helper used by `Config::load`'s caller to confirm the file exists.
#[allow(dead_code)]
pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

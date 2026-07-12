//! Loads `config.json` — the file a business edits to describe *their* queue:
//! the brand name, the queue types (Small/Medium/Large table, etc.) and the
//! fields shown on the operator's "add guest" form.
//!
//! Both apps fetch this at runtime via `GET /api/config`, so changing the
//! queue requires only editing this JSON file and restarting — no recompile.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

/// Max characters accepted per submitted field value.
pub const MAX_FIELD_LEN: usize = 500;
/// Max number of keys accepted in a submitted fields map.
pub const MAX_FIELDS: usize = 32;

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

    /// Validate a guest's submitted field values against this queue's form
    /// definition: required fields must be present and non-empty, select
    /// fields must use one of their configured options, and every value must
    /// be a reasonably-sized scalar. Unknown keys are allowed (so API callers
    /// aren't broken by extra fields) but still count toward `MAX_FIELDS`.
    pub fn validate_fields(&self, fields: &Map<String, Value>) -> Result<(), String> {
        if fields.len() > MAX_FIELDS {
            return Err(format!("too many fields (max {MAX_FIELDS})"));
        }

        for (key, value) in fields {
            let as_str = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                Value::Array(_) | Value::Object(_) => {
                    return Err(format!("field '{key}' must be a text, number, or boolean value"));
                }
            };
            if as_str.chars().count() > MAX_FIELD_LEN {
                return Err(format!("field '{key}' is too long (max {MAX_FIELD_LEN} characters)"));
            }
        }

        for field in &self.fields {
            let value = fields.get(&field.key);
            let text = value.and_then(|v| v.as_str()).unwrap_or("").trim();

            if field.required && text.is_empty() {
                return Err(format!("'{}' is required", field.label));
            }

            if field.r#type == "select"
                && !field.options.is_empty()
                && !text.is_empty()
                && !field.options.iter().any(|o| o == text)
            {
                return Err(format!("'{}' must be one of the provided options", field.label));
            }
        }

        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            brand: default_brand(),
            tagline: "Please wait for your number".to_string(),
            queue_types: vec![
                QueueType {
                    code: "A".into(),
                    name: "Small table".into(),
                    description: "1–2 guests".into(),
                },
                QueueType {
                    code: "B".into(),
                    name: "Medium table".into(),
                    description: "3–5 guests".into(),
                },
                QueueType {
                    code: "C".into(),
                    name: "Large table".into(),
                    description: "6+ guests".into(),
                },
            ],
            fields: vec![
                FieldDef {
                    key: "name".into(),
                    label: "Name".into(),
                    r#type: "text".into(),
                    required: true,
                    options: vec![],
                },
                FieldDef {
                    key: "phone".into(),
                    label: "Phone number".into(),
                    r#type: "tel".into(),
                    required: false,
                    options: vec![],
                },
            ],
        }
    }
}

/// Tiny helper used by `Config::load`'s caller to confirm the file exists.
#[allow(dead_code)]
pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(obj: serde_json::Value) -> Map<String, Value> {
        obj.as_object().cloned().unwrap_or_default()
    }

    fn select_config() -> Config {
        Config {
            brand: default_brand(),
            tagline: String::new(),
            queue_types: vec![QueueType {
                code: "A".into(),
                name: "A".into(),
                description: String::new(),
            }],
            fields: vec![FieldDef {
                key: "size".into(),
                label: "Size".into(),
                r#type: "select".into(),
                required: true,
                options: vec!["Small".into(), "Large".into()],
            }],
        }
    }

    #[test]
    fn validate_fields_ok_when_required_present() {
        let cfg = Config::default();
        assert!(cfg.validate_fields(&fields(json!({ "name": "Alice" }))).is_ok());
    }

    #[test]
    fn validate_fields_rejects_missing_required() {
        let cfg = Config::default();
        assert!(cfg.validate_fields(&fields(json!({ "phone": "123" }))).is_err());
    }

    #[test]
    fn validate_fields_rejects_blank_required() {
        let cfg = Config::default();
        assert!(cfg.validate_fields(&fields(json!({ "name": "   " }))).is_err());
    }

    #[test]
    fn validate_fields_rejects_too_long_value() {
        let cfg = Config::default();
        let long = "x".repeat(MAX_FIELD_LEN + 1);
        assert!(cfg.validate_fields(&fields(json!({ "name": long }))).is_err());
    }

    #[test]
    fn validate_fields_rejects_non_scalar_value() {
        let cfg = Config::default();
        assert!(cfg.validate_fields(&fields(json!({ "name": ["Alice"] }))).is_err());
    }

    #[test]
    fn validate_fields_rejects_too_many_keys() {
        let cfg = Config::default();
        let mut map = Map::new();
        map.insert("name".into(), json!("Alice"));
        for i in 0..MAX_FIELDS {
            map.insert(format!("extra{i}"), json!("x"));
        }
        assert!(cfg.validate_fields(&map).is_err());
    }

    #[test]
    fn validate_fields_allows_unknown_keys() {
        let cfg = Config::default();
        assert!(cfg.validate_fields(&fields(json!({ "name": "Alice", "unlisted": "ok" }))).is_ok());
    }

    #[test]
    fn validate_fields_select_must_match_options() {
        let cfg = select_config();
        assert!(cfg.validate_fields(&fields(json!({ "size": "Medium" }))).is_err());
        assert!(cfg.validate_fields(&fields(json!({ "size": "Small" }))).is_ok());
    }
}

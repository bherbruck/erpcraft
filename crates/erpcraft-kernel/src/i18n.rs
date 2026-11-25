//! Translations of what the registry describes: labels, plurals, help,
//! the titles of command inputs and outputs, policy refusals.
//!
//! Each module carries a message catalogue per locale
//! ([`Module::catalogues`](crate::registry::Module::catalogues)): a JSON
//! object from the English text to the translation, such as
//! `{"Customer": "Debitor"}`. The kernel's catalogue also holds the terms
//! several modules share. Text is looked up in the catalogue of the module
//! that describes it, then in the kernel's, and stays English when neither
//! has it. English is what the code says; there is no English catalogue.
//!
//! The catalogues are keyed by the English text, not by an identifier,
//! because the registry's labels are written once, in the code, beside
//! what they label: a key would be a second name to keep in step. A module
//! that needs one text two ways says it in two English words.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{Error, Result};

/// The locales there are catalogues for, beside English.
pub type Locale = String;

/// Every module's catalogue of every locale.
#[derive(Debug, Default, Clone)]
pub struct Catalogues {
    /// Locale, then module, then English text to translation.
    by_locale: BTreeMap<Locale, BTreeMap<String, BTreeMap<String, String>>>,
}

/// The module whose catalogue comes after a module's own.
pub const SHARED: &str = "kernel";

/// The keys whose text is translated wherever they are. `title` is too,
/// except the title a JSON schema gives a type (`DecideInput`).
const KEYS: &[&str] = &["label", "plural", "help", "refusal"];

impl Catalogues {
    /// Add a module's catalogue of a locale: a JSON object of English text
    /// to translation.
    pub fn add(&mut self, module: &str, locale: &str, json: &str) -> Result<()> {
        let entries: BTreeMap<String, String> = serde_json::from_str(json).map_err(|e| {
            Error::Internal(format!(
                "the {locale} catalogue of module {module} is not a JSON object of texts: {e}"
            ))
        })?;
        self.by_locale
            .entry(locale.to_owned())
            .or_default()
            .entry(module.to_owned())
            .or_default()
            .extend(entries);
        Ok(())
    }

    /// The locales there are catalogues for, beside English.
    pub fn locales(&self) -> Vec<&str> {
        self.by_locale.keys().map(String::as_str).collect()
    }

    /// The locale to answer in for an `Accept-Language` header or a
    /// `locale` parameter: the first one asked for that there is a
    /// catalogue for (`de-DE` takes `de`), else English (`None`).
    pub fn negotiate(&self, asked: &str) -> Option<String> {
        let mut ranked: Vec<(f32, String)> = asked
            .split(',')
            .filter_map(|part| {
                let mut parts = part.trim().split(';');
                let tag = parts.next()?.trim().to_ascii_lowercase();
                let quality = parts
                    .find_map(|p| p.trim().strip_prefix("q="))
                    .and_then(|q| q.parse::<f32>().ok())
                    .unwrap_or(1.0);
                (!tag.is_empty()).then_some((quality, tag))
            })
            .collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, tag) in ranked {
            if tag == "en" || tag.starts_with("en-") {
                return None;
            }
            let primary = tag.split('-').next().unwrap_or(&tag);
            if self.by_locale.contains_key(primary) {
                return Some(primary.to_owned());
            }
        }
        None
    }

    /// The translation of `text` described by `module`, if there is one.
    pub fn lookup(&self, locale: &str, module: &str, text: &str) -> Option<&str> {
        let catalogues = self.by_locale.get(locale)?;
        catalogues
            .get(module)
            .and_then(|c| c.get(text))
            .or_else(|| catalogues.get(SHARED).and_then(|c| c.get(text)))
            .map(String::as_str)
    }

    /// Translate a description (as `meta.describe` gives it) in place.
    pub fn translate(&self, description: &mut Value, locale: &str) {
        name_modules(description, true);
        walk(description, SHARED, false, &mut |module, text| {
            self.lookup(locale, module, text).map(str::to_owned)
        });
        name_modules(description, false);
    }

    /// The texts of a description that have no translation in `locale`,
    /// with the module that describes each.
    pub fn missing(&self, description: &Value, locale: &str) -> Vec<(String, String)> {
        let mut out = vec![];
        let mut copy = description.clone();
        name_modules(&mut copy, true);
        walk(&mut copy, SHARED, false, &mut |module, text| {
            if self.lookup(locale, module, text).is_none() {
                out.push((module.to_owned(), text.to_owned()));
            }
            None
        });
        out.sort();
        out.dedup();
        out
    }
}

/// Visit every translatable text of a description, replacing it with what
/// `f` gives. `module` is the module of the record the text is in (the
/// nearest `module` field above it); `schema_root` says that this object
/// is a JSON schema whose `title` names a type.
fn walk(
    value: &mut Value,
    module: &str,
    schema_root: bool,
    f: &mut dyn FnMut(&str, &str) -> Option<String>,
) {
    match value {
        Value::Object(map) => {
            let module = match map.get("module").and_then(Value::as_str) {
                Some(m) if !m.is_empty() => m.to_owned(),
                _ => module.to_owned(),
            };
            for (key, v) in map.iter_mut() {
                match v {
                    Value::String(text) => {
                        let translatable = KEYS.contains(&key.as_str())
                            || (key == "title" && !schema_root && !type_name(text));
                        if translatable && let Some(t) = f(&module, text) {
                            *text = t;
                        }
                    }
                    Value::Object(children) if key == "properties" => {
                        for child in children.values_mut() {
                            walk(child, &module, false, f);
                        }
                    }
                    Value::Object(children) if key == "$defs" || key == "definitions" => {
                        for child in children.values_mut() {
                            walk(child, &module, true, f);
                        }
                    }
                    // A JSON schema: `input_schema`, `output_schema`,
                    // `entry_schema`.
                    _ if key.ends_with("_schema") => walk(v, &module, true, f),
                    _ => walk(v, &module, false, f),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, module, false, f);
            }
        }
        _ => {}
    }
}

/// The list of modules names each by `name`, not `module`: for the walk,
/// each says it is its own (`on`), and then no longer.
fn name_modules(description: &mut Value, on: bool) {
    if let Some(Value::Array(modules)) = description.get_mut("modules") {
        for module in modules.iter_mut().filter_map(Value::as_object_mut) {
            if on {
                if let Some(name) = module.get("name").cloned() {
                    module.insert("module".into(), name);
                }
            } else {
                module.remove("module");
            }
        }
    }
}

/// A title that names a Rust type a JSON schema was made from, such as
/// `GlEntryRow` inlined in an output: one word, upper case after lower.
fn type_name(text: &str) -> bool {
    !text.contains(' ')
        && text.chars().next().is_some_and(char::is_uppercase)
        && text
            .chars()
            .zip(text.chars().skip(1))
            .any(|(a, b)| a.is_lowercase() && b.is_uppercase())
}

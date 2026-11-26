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
    /// Per locale, every module's messages together, for error messages,
    /// which come without the module that raised them. Made when first
    /// asked for.
    messages: std::sync::OnceLock<BTreeMap<Locale, Messages>>,
}

/// The messages of a locale: those written out in full, and those with
/// placeholders as templates.
#[derive(Debug, Default, Clone)]
struct Messages {
    exact: BTreeMap<String, String>,
    templates: Vec<Template>,
}

/// A message with placeholders: `{name}`, `{}` or `{:?}` as Rust writes
/// them, `%` as a trigger does. It matches a message made from it and
/// gives the translation with the same values in the translation's
/// placeholders.
#[derive(Debug, Clone)]
struct Template {
    pattern: regex::Regex,
    slots: Vec<String>,
    translation: Vec<Piece>,
    /// How much of the message is fixed text: the more, the more
    /// specific, and the earlier it is tried.
    weight: usize,
    /// The fixed text the message begins with, if it begins with any.
    lead: String,
}

#[derive(Debug, Clone)]
enum Piece {
    Text(String),
    Slot(String),
}

/// The pieces of a message with placeholders, each placeholder named: by
/// its name, `#n` for the n-th unnamed one, `%n` for the n-th `%`.
fn pieces(text: &str) -> Vec<Piece> {
    let mut out = vec![];
    let mut literal = String::new();
    let (mut unnamed, mut percent) = (0, 0);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut inside = String::new();
                for d in chars.by_ref() {
                    if d == '}' {
                        break;
                    }
                    inside.push(d);
                }
                let name = inside
                    .split(':')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                let name = if name.is_empty() || name.chars().all(|c| c.is_ascii_digit()) {
                    unnamed += 1;
                    format!("#{unnamed}")
                } else {
                    name
                };
                out.push(Piece::Text(std::mem::take(&mut literal)));
                out.push(Piece::Slot(name));
            }
            // `%%` in a trigger's message is one `%`, no value.
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                literal.push('%');
            }
            '%' => {
                percent += 1;
                out.push(Piece::Text(std::mem::take(&mut literal)));
                out.push(Piece::Slot(format!("%{percent}")));
            }
            c => literal.push(c),
        }
    }
    out.push(Piece::Text(literal));
    out
}

impl Template {
    fn new(english: &str, translation: &str) -> Option<Template> {
        let parts = pieces(english);
        let slots: Vec<String> = parts
            .iter()
            .filter_map(|p| match p {
                Piece::Slot(s) => Some(s.clone()),
                Piece::Text(_) => None,
            })
            .collect();
        if slots.is_empty() {
            return None;
        }
        let mut pattern = String::from("(?s)^");
        let mut weight = 0;
        for part in &parts {
            match part {
                Piece::Text(t) => {
                    weight += t.len();
                    pattern.push_str(&regex::escape(t));
                }
                Piece::Slot(_) => pattern.push_str("(.+?)"),
            }
        }
        pattern.push('$');
        let lead = match parts.first() {
            Some(Piece::Text(t)) => t.clone(),
            _ => String::new(),
        };
        Some(Template {
            pattern: regex::Regex::new(&pattern).ok()?,
            slots,
            translation: pieces(translation),
            weight,
            lead,
        })
    }

    /// Whether the template only wraps another message, as `line {}: {}`
    /// and `field '{}' {}` do: fixed text at most around a line or field
    /// and the message itself last.
    fn wraps(&self) -> bool {
        self.pattern.as_str().ends_with("(.+?)$") && self.weight <= self.lead.len() + 2
    }

    /// The translation of `message` if it is made from this template, with
    /// each value as `inner` translates it: a value that is a sentence of
    /// its own (`invalid input: line 2: unknown field 'x'`) is translated
    /// too, a name or a number stays as it is.
    fn apply_with(
        &self,
        message: &str,
        inner: &dyn Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let found = self.pattern.captures(message)?;
        let value = |name: &str| {
            self.slots
                .iter()
                .position(|s| s == name)
                .and_then(|i| found.get(i + 1))
                .map(|m| {
                    let text = m.as_str();
                    (text.contains(' ') && text.len() < message.len())
                        .then(|| inner(text))
                        .flatten()
                        .unwrap_or_else(|| text.to_owned())
                })
        };
        let mut out = String::new();
        for piece in &self.translation {
            match piece {
                Piece::Text(t) => out.push_str(t),
                Piece::Slot(name) => out.push_str(&value(name).unwrap_or_default()),
            }
        }
        Some(out)
    }
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
                // q=0 is "not acceptable" (RFC 9110, 12.4.2).
                (!tag.is_empty() && quality > 0.0).then_some((quality, tag))
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

    /// Every module's messages of a locale, made once.
    fn messages(&self, locale: &str) -> Option<&Messages> {
        self.messages
            .get_or_init(|| {
                self.by_locale
                    .iter()
                    .map(|(locale, modules)| {
                        let mut messages = Messages::default();
                        // The kernel's first, so that a module's own wins.
                        let order = modules
                            .iter()
                            .filter(|(m, _)| m.as_str() == SHARED)
                            .chain(modules.iter().filter(|(m, _)| m.as_str() != SHARED));
                        for (_, catalogue) in order {
                            for (english, translation) in catalogue {
                                messages.exact.insert(english.clone(), translation.clone());
                            }
                        }
                        messages.templates = messages
                            .exact
                            .iter()
                            .filter_map(|(e, t)| Template::new(e, t))
                            .collect();
                        messages
                            .templates
                            .sort_by_key(|t| std::cmp::Reverse(t.weight));
                        (locale.clone(), messages)
                    })
                    .collect()
            })
            .get(locale)
    }

    /// The translation of an error message, whichever module made it: the
    /// message as it is in a catalogue, else one made from a message with
    /// placeholders, which keep their values. A message of a line
    /// (`line 3: ...`) or a field (`field 'qty' ...`) keeps its line or
    /// field. `None` when there is none.
    pub fn translate_message(&self, locale: &str, message: &str) -> Option<String> {
        let messages = self.messages(locale)?;
        if let Some(t) = messages.exact.get(message) {
            return Some(t.clone());
        }
        let inner = |text: &str| self.translate_message(locale, text);
        // A message of a line or a field that has a translation of its own,
        // as a whole (`line {}: these goods have been invoiced; ...`),
        // takes it. A template that begins with a placeholder would take
        // the prefix as its value and leave it English, so only those that
        // begin with the same words are tried here; and not the wrappers
        // (`line {line}: {msg}`), which are what the prefix is for.
        for prefix in ["line ", "field '"] {
            if message.starts_with(prefix)
                && let Some(t) = messages.templates.iter().find_map(|t| {
                    (t.lead.starts_with(prefix) && !t.wraps())
                        .then(|| t.apply_with(message, &inner))
                        .flatten()
                })
            {
                return Some(t);
            }
        }
        if let Some(rest) = message.strip_prefix("line ")
            && let Some((n, rest)) = rest.split_once(": ")
            && n.chars().all(|c| c.is_ascii_digit())
        {
            let line = self
                .lookup(locale, SHARED, "line {}: ")
                .map(|t| t.replace("{}", n))
                .unwrap_or_else(|| format!("line {n}: "));
            return Some(format!(
                "{line}{}",
                self.translate_message(locale, rest)
                    .unwrap_or(rest.to_owned())
            ));
        }
        if let Some(rest) = message.strip_prefix("field '")
            && let Some((name, rest)) = rest.split_once("' ")
        {
            let field = self
                .lookup(locale, SHARED, "field '{}' ")
                .map(|t| t.replace("{}", name))
                .unwrap_or_else(|| format!("field '{name}' "));
            return Some(format!(
                "{field}{}",
                self.translate_message(locale, rest)
                    .unwrap_or(rest.to_owned())
            ));
        }
        // A record that is not found names its kind as the code does
        // (`sales_order not found`): the kind as its label says it.
        if let Some(what) = message.strip_suffix(" not found")
            && !what.contains(' ')
        {
            let mut label = what.replace('_', " ");
            if let Some(first) = label.get(..1) {
                label = first.to_uppercase() + &label[1..];
            }
            let noun = messages.exact.get(&label).cloned().unwrap_or(label);
            return messages
                .exact
                .get("{} not found")
                .map(|t| t.replace("{}", &noun));
        }
        messages
            .templates
            .iter()
            .find_map(|t| t.apply_with(message, &inner))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogues() -> Catalogues {
        let mut c = Catalogues::default();
        c.add(
            SHARED,
            "de",
            r#"{
                "line {}: ": "Zeile {}: ",
                "field '{}' ": "Feld '{}' ",
                "{} not found": "{} nicht gefunden",
                "Sales order": "Kundenauftrag",
                "the quantity must be more than zero": "die Menge muss größer als null sein"
            }"#,
        )
        .unwrap();
        c.add(
            "stock",
            "de",
            r#"{
                "only {} {uom} of item {item_code} are on hand in warehouse {warehouse_code}": "nur {} {uom} von Artikel {item_code} sind in Lager {warehouse_code} vorhanden",
                "account {code} is inactive": "Konto {code} ist inaktiv",
                "only % % of item % are on hand in warehouse %": "nur % % von Artikel % sind in Lager % vorhanden"
            }"#,
        )
        .unwrap();
        c
    }

    #[test]
    fn messages_with_values_are_translated_keeping_them() {
        let c = catalogues();
        let t = |m: &str| c.translate_message("de", m);
        assert_eq!(
            t("only 3 kg of item WIDGET are on hand in warehouse MAIN").as_deref(),
            Some("nur 3 kg von Artikel WIDGET sind in Lager MAIN vorhanden")
        );
        assert_eq!(
            t("account 4000 is inactive").as_deref(),
            Some("Konto 4000 ist inaktiv")
        );
        // A trigger's placeholders.
        assert_eq!(
            t("only 2 each of item W are on hand in warehouse M").as_deref(),
            Some("nur 2 each von Artikel W sind in Lager M vorhanden")
        );
        // A line's or field's message keeps its line or field.
        assert_eq!(
            t("line 3: the quantity must be more than zero").as_deref(),
            Some("Zeile 3: die Menge muss größer als null sein")
        );
        assert_eq!(
            t("field 'qty' line 2: the quantity must be more than zero").as_deref(),
            Some("Feld 'qty' Zeile 2: die Menge muss größer als null sein")
        );
        // A record not found, by the label of its kind.
        assert_eq!(
            t("sales_order not found").as_deref(),
            Some("Kundenauftrag nicht gefunden")
        );
        // Nothing to say: no translation.
        assert_eq!(t("something nobody wrote"), None);
        assert_eq!(c.translate_message("fr", "account 4000 is inactive"), None);
    }

    #[test]
    fn the_language_asked_for_is_negotiated() {
        let c = catalogues();
        assert_eq!(
            c.negotiate("de-DE,de;q=0.9,en;q=0.8").as_deref(),
            Some("de")
        );
        assert_eq!(c.negotiate("en-US,de;q=0.5"), None);
        assert_eq!(c.negotiate("fr;q=1, de;q=0.4").as_deref(), Some("de"));
        assert_eq!(c.negotiate("fr"), None);
    }
}

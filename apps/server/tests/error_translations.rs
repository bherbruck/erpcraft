//! Error messages in German. The messages a command can answer with are
//! read from the sources: every string written into an `Error::` call in a
//! crate or module, and every message a migration's trigger raises. Each
//! must have a German translation in its module's catalogue (or the
//! kernel's), so a new message fails here until it is translated. And a
//! refused command answers in the language asked for, its code and field
//! unchanged.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

/// The module whose catalogue holds the messages of a crate's sources.
fn module_of(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let at = |name: &str| parts.iter().position(|p| p == name);
    if let Some(i) = at("modules") {
        return parts[i + 1].clone();
    }
    let krate = at("crates")
        .map(|i| parts[i + 1].clone())
        .unwrap_or_default();
    match krate.trim_start_matches("erpcraft-") {
        "gl" => "gl",
        "openitems" => "open_items",
        "stock" => "stock",
        "tax" => "tax",
        "party" => "party",
        "pricing" => "pricing",
        "auth" => "auth",
        _ => "kernel",
    }
    .to_owned()
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

/// The string literals in `text` from `start`, within the parentheses that
/// open there, with line continuations joined.
fn literals_in_call(text: &str, start: usize) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut i = start;
    let mut out = vec![];
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b'"' => {
                let mut s = String::new();
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        match bytes[i + 1] {
                            b'\n' => {
                                i += 2;
                                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                                    i += 1;
                                }
                                continue;
                            }
                            b'"' => s.push('"'),
                            b'\\' => s.push('\\'),
                            b'n' => s.push('\n'),
                            b'\'' => s.push('\''),
                            other => {
                                s.push('\\');
                                s.push(other as char);
                            }
                        }
                        i += 2;
                        continue;
                    }
                    let ch = text[i..].chars().next().unwrap();
                    s.push(ch);
                    i += ch.len_utf8();
                }
                out.push(s);
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// Every message the sources can answer with, by module.
fn messages() -> BTreeSet<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut out = BTreeSet::new();
    let mut rust = vec![];
    for dir in ["crates", "modules"] {
        files(&root.join(dir), "rs", &mut rust);
    }
    const CALLS: &[&str] = &[
        "Error::validation(",
        "Error::state(",
        "Error::conflict(",
        "Error::stale(",
        "Error::duplicate(",
        "Error::in_use(",
        "Error::forbidden(",
        "Error::line(",
        "Error::field(",
    ];
    for path in rust {
        if path.components().any(|c| c.as_os_str() == "tests") {
            continue;
        }
        let module = module_of(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        for call in CALLS {
            for (at, _) in text.match_indices(call) {
                for literal in literals_in_call(&text, at + call.len() - 1) {
                    if literal.contains(' ') {
                        out.insert((module.clone(), literal));
                    }
                }
            }
        }
        for (at, _) in text.match_indices("Error::not_found(") {
            if let Some(what) = literals_in_call(&text, at + "Error::not_found(".len() - 1)
                .into_iter()
                .next()
            {
                out.insert((module.clone(), format!("{what} not found")));
            }
        }
    }
    let mut sql = vec![];
    for dir in ["crates", "modules"] {
        files(&root.join(dir), "sql", &mut sql);
    }
    for path in sql {
        let module = module_of(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        for (at, _) in text.match_indices("RAISE EXCEPTION '") {
            let rest = &text[at + "RAISE EXCEPTION '".len()..];
            let mut message = String::new();
            let mut chars = rest.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\'' {
                    if chars.peek() == Some(&'\'') {
                        chars.next();
                        message.push('\'');
                        continue;
                    }
                    break;
                }
                message.push(c);
            }
            out.insert((module.clone(), message));
        }
    }
    out
}

#[tokio::test]
async fn every_error_message_has_a_german_translation() {
    let t = TestApp::start().await;
    let catalogues = t.kernel.registry().catalogues();
    let all = messages();
    assert!(all.len() > 500, "the sources were not found: {}", all.len());
    let missing: Vec<&(String, String)> = all
        .iter()
        .filter(|(module, text)| catalogues.lookup("de", module, text).is_none())
        .collect();
    if let Ok(path) = std::env::var("ERPCRAFT_MISSING_MESSAGES") {
        let rows: Vec<_> = missing.iter().map(|(m, t)| json!([m, t])).collect();
        std::fs::write(path, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
    }
    let shown: Vec<String> = missing
        .iter()
        .take(40)
        .map(|(module, text)| format!("{module}: {text:?}"))
        .collect();
    assert!(
        missing.is_empty(),
        "{} error messages have no German translation; add them to the module's i18n/de.json \
         (placeholders such as {{code}}, {{}} or a trigger's % stay as they are):\n{}",
        missing.len(),
        shown.join("\n")
    );
    t.stop().await;
}

#[tokio::test]
async fn a_refusal_answers_in_the_language_asked_for() {
    let t = TestApp::start().await;
    let c = t.company("ACME", "EUR").await;
    let draft = json!({ "company_id": c.id, "doc_date": "2026-03-01",
                        "body": { "lines": [ { "account_id": c.cash, "debit": "10" } ] } });
    let (status, created) = t
        .post(&t.admin, "/api/documents/journal_entry", draft)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let post = json!({ "id": created["id"] });
    // Posting one line is refused: in German by header; the code stays.
    let (status, body) = t
        .send(
            "POST",
            "/api/commands/journal_entry.post",
            Some(&t.admin),
            Some(post.clone()),
            &[("accept-language", "de-DE,de;q=0.9")],
        )
        .await;
    let (status_en, body_en) = t
        .command(&t.admin, "journal_entry.post", post.clone())
        .await;
    assert_eq!(status, status_en);
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], body_en["error"]["code"]);
    assert_eq!(
        body["error"]["message"], "ein Buchungssatz braucht mindestens zwei Zeilen",
        "{body} {body_en}"
    );
    // A missing record, by parameter.
    let (status, body) = t
        .get(
            &t.admin,
            "/api/entities/customer/00000000-0000-0000-0000-000000000000?locale=de",
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nicht gefunden"),
        "{body}"
    );
    t.stop().await;
}

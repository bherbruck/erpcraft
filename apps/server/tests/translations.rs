//! Labels, help and refusals in German: every text the registry describes
//! has a translation in a module's catalogue (or the kernel's), so a new
//! label fails here until it is translated; and /api/meta answers in the
//! language asked for, English otherwise.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::Value;

#[tokio::test]
async fn every_text_has_a_german_translation() {
    let t = TestApp::start().await;
    let (status, meta) = t.get(&t.admin, "/api/meta").await;
    assert_eq!(status, StatusCode::OK);
    let missing = t.kernel.registry().catalogues().missing(&meta, "de");
    let shown: Vec<String> = missing
        .iter()
        .take(60)
        .map(|(module, text)| format!("{module}: {text:?}"))
        .collect();
    assert!(
        missing.is_empty(),
        "{} texts have no German translation; add them to the module's i18n/de.json \
         (or the kernel's, for a term several modules share):\n{}",
        missing.len(),
        shown.join("\n")
    );
    t.stop().await;
}

fn entity<'a>(meta: &'a Value, name: &str) -> &'a Value {
    meta["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap()
}

fn doc_type<'a>(meta: &'a Value, name: &str) -> &'a Value {
    meta["doc_types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap()
}

#[tokio::test]
async fn meta_answers_in_the_language_asked_for() {
    let t = TestApp::start().await;
    // By parameter.
    let (_, de) = t.get(&t.admin, "/api/meta?locale=de").await;
    assert_eq!(de["locale"], "de");
    assert_eq!(entity(&de, "customer")["label"], "Debitor");
    assert_eq!(entity(&de, "vendor")["label"], "Kreditor");
    assert_eq!(doc_type(&de, "sales_delivery")["label"], "Lieferschein");
    assert_eq!(doc_type(&de, "purchase_receipt")["label"], "Wareneingang");
    assert_eq!(doc_type(&de, "dunning_letter")["label"], "Mahnung");
    let states: Vec<&str> = doc_type(&de, "sales_invoice")["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["label"].as_str().unwrap())
        .collect();
    assert!(states.contains(&"Gebucht"), "{states:?}");
    // By header, with a region and a weight.
    let (status, header) = t
        .send(
            "GET",
            "/api/meta",
            Some(&t.admin),
            None,
            &[("accept-language", "de-DE,de;q=0.9,en;q=0.8")],
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(entity(&header, "customer")["label"], "Debitor");
    // English, and a language there is no catalogue for, are English.
    for path in ["/api/meta", "/api/meta?locale=fr", "/api/meta?locale=en-GB"] {
        let (_, en) = t.get(&t.admin, path).await;
        assert!(en["locale"].is_null(), "{path}");
        assert_eq!(entity(&en, "customer")["label"], "Customer", "{path}");
    }
    t.stop().await;
}

//! Review 9: error messages in German, through the real catalogues of
//! every module. Each test fails on the code as it is.

mod common;

use axum::http::StatusCode;
use common::TestApp;
use serde_json::json;

/// A message that starts `line N: ` and has its own German translation
/// (as the sales and purchasing modules write them) is translated whole.
/// Today the `line N: ` shortcut answers first, translates only the rest
/// on its own, finds nothing, and leaves the sentence in English behind a
/// German "Zeile N:".
#[tokio::test]
async fn a_line_message_with_its_own_translation_is_translated_whole() {
    let t = TestApp::start().await;
    let c = t.kernel.registry().catalogues();
    let cases = [
        (
            "line 2: these goods have been invoiced; reverse the invoice first",
            "Zeile 2: diese Ware wurde berechnet; zuerst die Rechnung stornieren",
        ),
        (
            "line 1: the vendor has credited these goods; reverse the credit note first",
            "Zeile 1: der Kreditor hat diese Ware gutgeschrieben; zuerst die Gutschrift stornieren",
        ),
        (
            "field 'iban' was refused: DE00123 is not a valid IBAN (its check digits do not match)",
            "Feld 'iban' wurde abgelehnt: DE00123 ist keine gültige IBAN (die Prüfziffern stimmen nicht)",
        ),
    ];
    let mut wrong = vec![];
    for (english, german) in cases {
        let got = c.translate_message("de", english);
        if got.as_deref() != Some(german) {
            wrong.push(format!("{english:?}\n  got  {got:?}\n  want {german:?}"));
        }
    }
    t.stop().await;
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The sentence inside a wrapper such as `invalid {what}: {sentence}` is
/// translated too, and the line it is about kept.
#[tokio::test]
async fn the_sentence_inside_a_message_is_translated_too() {
    let t = TestApp::start().await;
    let c = t.kernel.registry().catalogues();
    let got = c.translate_message("de", "invalid input: line 2: unknown field 'colour'");
    t.stop().await;
    let got = got.unwrap_or_default();
    assert!(
        got.contains("Zeile 2") && got.contains("'colour'") && !got.contains("unknown field"),
        "{got}"
    );
}

/// A trigger's `%%` is one literal `%` in the message PostgreSQL raises,
/// not two placeholders: `the parts come to 100 %; ...`. The template
/// reads `% %%` as three placeholders and never matches the real message.
#[tokio::test]
async fn a_trigger_message_with_a_literal_percent_is_translated() {
    let t = TestApp::start().await;
    let c = t.kernel.registry().catalogues();
    let got = c.translate_message(
        "de",
        "the parts come to 100 %; they must leave something for the last, due at the net days",
    );
    t.stop().await;
    assert_eq!(
        got.as_deref(),
        Some(
            "die Raten ergeben 100 %; sie müssen etwas für die letzte lassen, fällig zu den Nettotagen"
        )
    );
}

/// `q=0` means "not acceptable" (RFC 9110 §12.4.2): a client that says
/// `de;q=0` asks not to be answered in German.
#[tokio::test]
async fn a_language_refused_with_q_zero_is_not_chosen() {
    let t = TestApp::start().await;
    let c = t.kernel.registry().catalogues();
    let negotiated = c.negotiate("de;q=0");
    let (status, body) = t
        .send(
            "GET",
            "/api/entities/customer/00000000-0000-0000-0000-000000000000",
            Some(&t.admin),
            None,
            &[("accept-language", "fr, de;q=0")],
        )
        .await;
    t.stop().await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(negotiated, None, "de;q=0 was taken as German");
    assert!(
        !body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("nicht gefunden"),
        "answered in German though German was refused: {}",
        json!(body)
    );
}

//! The templates built into the server: one for each of the documents a
//! company most often hands out, and one that prints any document from
//! its type's description, so that every type prints something. Each in
//! English, and in German (`de`), which also carries what § 14 UStG asks
//! of an invoice.

pub(crate) const STYLE: &str = include_str!("builtin/style.css");

fn part(locale: &str, name: &str) -> &'static str {
    match (locale, name) {
        ("de", "head") => include_str!("builtin/de/head.html"),
        ("de", "foot") => include_str!("builtin/de/foot.html"),
        ("de", "generic") => include_str!("builtin/de/generic.html"),
        (_, "head") => include_str!("builtin/head.html"),
        (_, "foot") => include_str!("builtin/foot.html"),
        _ => include_str!("builtin/generic.html"),
    }
}

fn own(doc_type: &str, locale: &str) -> Option<&'static str> {
    Some(match (locale, doc_type) {
        ("de", "purchase_order") => include_str!("builtin/de/purchase_order.html"),
        ("de", "sales_order") => include_str!("builtin/de/sales_order.html"),
        ("de", "sales_delivery") => include_str!("builtin/de/sales_delivery.html"),
        ("de", "sales_invoice") => include_str!("builtin/de/sales_invoice.html"),
        ("de", "customer_statement") => include_str!("builtin/de/customer_statement.html"),
        ("de", "dunning_letter") => include_str!("builtin/de/dunning_letter.html"),
        (_, "purchase_order") => include_str!("builtin/purchase_order.html"),
        (_, "sales_order") => include_str!("builtin/sales_order.html"),
        (_, "sales_delivery") => include_str!("builtin/sales_delivery.html"),
        (_, "sales_invoice") => include_str!("builtin/sales_invoice.html"),
        (_, "customer_statement") => include_str!("builtin/customer_statement.html"),
        (_, "dunning_letter") => include_str!("builtin/dunning_letter.html"),
        _ => return None,
    })
}

/// The language a built-in is chosen in: German for `de`, else English.
fn language(locale: &str) -> &'static str {
    if locale == "de" { "de" } else { "en" }
}

/// Whether the type has a template of its own.
pub(crate) fn has_own(doc_type: &str, locale: &str) -> bool {
    own(doc_type, language(locale)).is_some()
}

/// The built-in template for a document type in a language, whole.
pub(crate) fn template(doc_type: &str, locale: &str) -> String {
    let locale = language(locale);
    own(doc_type, locale)
        .unwrap_or(part(locale, "generic"))
        .replace("<!--HEAD-->", part(locale, "head"))
        .replace("<!--FOOT-->", part(locale, "foot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_compiles_and_holds_nothing_active() {
        for locale in ["en", "de"] {
            for doc_type in [
                "purchase_order",
                "sales_order",
                "sales_delivery",
                "sales_invoice",
                "customer_statement",
                "dunning_letter",
                "anything_else",
            ] {
                let source = template(doc_type, locale);
                crate::engine::check_source(&source)
                    .unwrap_or_else(|e| panic!("{locale} {doc_type}: {e}"));
            }
        }
    }
}

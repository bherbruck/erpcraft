//! The templates built into the server: one for each of the documents a
//! company most often hands out, and one that prints any document from
//! its type's description, so that every type prints something.

pub(crate) const STYLE: &str = include_str!("builtin/style.css");
const HEAD: &str = include_str!("builtin/head.html");
const FOOT: &str = include_str!("builtin/foot.html");
const GENERIC: &str = include_str!("builtin/generic.html");

fn own(doc_type: &str) -> Option<&'static str> {
    Some(match doc_type {
        "purchase_order" => include_str!("builtin/purchase_order.html"),
        "sales_order" => include_str!("builtin/sales_order.html"),
        "sales_delivery" => include_str!("builtin/sales_delivery.html"),
        "sales_invoice" => include_str!("builtin/sales_invoice.html"),
        _ => return None,
    })
}

/// Whether the type has a template of its own.
pub(crate) fn has_own(doc_type: &str) -> bool {
    own(doc_type).is_some()
}

/// The built-in template for a document type, whole.
pub(crate) fn template(doc_type: &str) -> String {
    own(doc_type)
        .unwrap_or(GENERIC)
        .replace("<!--HEAD-->", HEAD)
        .replace("<!--FOOT-->", FOOT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_compiles_and_holds_nothing_active() {
        for doc_type in [
            "purchase_order",
            "sales_order",
            "sales_delivery",
            "sales_invoice",
            "anything_else",
        ] {
            let source = template(doc_type);
            crate::engine::check_source(&source).unwrap_or_else(|e| panic!("{doc_type}: {e}"));
        }
    }
}

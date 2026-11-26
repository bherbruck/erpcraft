//! The module's records as generic entities. Partners and cross-references
//! are written with the generic commands; the log of interchanges and
//! messages is read-only, written only by the module's commands.

use erpcraft_kernel::registry::Registry;
use erpcraft_kernel::{EntityMeta, Error, FieldMeta, FieldType, Result};
use serde_json::{Map, Value};

fn created() -> FieldMeta {
    FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly()
}

fn partner_field() -> FieldMeta {
    FieldMeta::new("partner_id", "Partner", FieldType::link("edi_partner"))
}

fn separator(name: &str, label: &str, help: &str) -> FieldMeta {
    FieldMeta::new(name, label, FieldType::Text).help(help)
}

pub fn all() -> Vec<EntityMeta> {
    vec![
        partner(),
        item_xref(),
        uom_xref(),
        location_xref(),
        interchange(),
        message(),
        settings(),
        guide(),
        guide_message(),
    ]
}

fn partner() -> EntityMeta {
    EntityMeta::new("edi_partner", "EDI partner")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(FieldMeta::new("code", "Code", FieldType::Text).required())
        .field(FieldMeta::new("name", "Name", FieldType::Text).required())
        .field(
            FieldMeta::new("customer_id", "Customer", FieldType::link("customer"))
                .required()
                .help("The customer the partner's orders are drafted for."),
        )
        .field(
            FieldMeta::new("warehouse_id", "Ships from", FieldType::link("warehouse")).help(
                "The warehouse the partner's orders ship from, unless a draft names another.",
            ),
        )
        .field(FieldMeta::new(
            "syntax",
            "Syntax",
            FieldType::labelled(&[("x12", "ANSI X12"), ("edifact", "UN/EDIFACT")]),
        ))
        .field(
            FieldMeta::new(
                "usage",
                "Usage",
                FieldType::options(&["test", "production"]),
            )
            .help(
                "While test, nothing the partner sends becomes a document, and what is sent \
                 to it is marked as a test.",
            ),
        )
        .field(
            FieldMeta::new("our_qualifier", "Our id qualifier", FieldType::Text)
                .required()
                .help("The kind of identifier ours is, as the partner's guide says (ISA07)."),
        )
        .field(
            FieldMeta::new("our_id", "Our id", FieldType::Text)
                .required()
                .help("Our interchange identifier, as the partner knows us (ISA08)."),
        )
        .field(
            FieldMeta::new("their_qualifier", "Their id qualifier", FieldType::Text)
                .required()
                .help("The kind of identifier theirs is (ISA05)."),
        )
        .field(
            FieldMeta::new("their_id", "Their id", FieldType::Text)
                .required()
                .help("The partner's interchange identifier (ISA06)."),
        )
        .field(
            FieldMeta::new("our_group_id", "Our group id", FieldType::Text)
                .help("Our identifier in the group header (GS02); our id when empty."),
        )
        .field(
            FieldMeta::new("their_group_id", "Their group id", FieldType::Text)
                .help("The partner's identifier in the group header (GS03); theirs when empty."),
        )
        .field(FieldMeta::new(
            "version",
            "Version",
            FieldType::options(&["004010", "005010", "D96A"]),
        ))
        .field(
            FieldMeta::new(
                "ack_form",
                "Acknowledgement",
                FieldType::options(&["997", "999", "contrl", "none"]),
            )
            .help("What the partner expects back for each group it sends."),
        )
        .field(
            FieldMeta::new("ack_hours", "Acknowledged within (hours)", FieldType::Int)
                .help("How long the partner has to acknowledge what is sent to it."),
        )
        .field(separator(
            "element_separator",
            "Element separator",
            "One character, written between elements.",
        ))
        .field(separator(
            "component_separator",
            "Component separator",
            "One character, written between the components of an element.",
        ))
        .field(separator(
            "repetition_separator",
            "Repetition separator",
            "One character; written only from version 005010.",
        ))
        .field(separator(
            "segment_terminator",
            "Segment terminator",
            "One character, written after each segment.",
        ))
        .field(
            FieldMeta::new("line_breaks", "Line breaks", FieldType::Bool).help(
                "A line break after each segment written, for partners that want to read files.",
            ),
        )
        .field(
            FieldMeta::new(
                "decimal_mark",
                "Decimal mark",
                FieldType::labelled(&[(".", "Point"), (",", "Comma")]),
            )
            .help("EDIFACT: the decimal mark numbers are written with, stated in the UNA."),
        )
        .field(
            FieldMeta::new(
                "charset",
                "Character set",
                FieldType::labelled(&[
                    ("UNOA", "UNOA: capitals and digits"),
                    ("UNOB", "UNOB: letters and digits"),
                    ("UNOC", "UNOC: ISO 8859-1"),
                    ("UNOY", "UNOY: UTF-8"),
                ]),
            )
            .help(
                "EDIFACT: the character set interchanges are written in. A text it cannot carry \
                 makes the message invalid, never a changed text.",
            ),
        )
        .field(
            FieldMeta::new("guide_id", "Guide", FieldType::link("edi_guide")).help(
                "The partner's implementation guide: what it reads and writes differently. \
                 Empty: the module's own way for the syntax and version.",
            ),
        )
        .field(
            FieldMeta::new("run_as_user_id", "Runs as", FieldType::link("user")).help(
                "The user automation acts as for this partner, with that user's permissions.",
            ),
        )
        .field(FieldMeta::new("active", "Active", FieldType::Bool))
        .field(created())
        .order_by(&["code"])
        .search(&["code", "name", "their_id"])
        .display(&["code", "name"])
        .plural("EDI partners")
        .company_field("company_id")
}

fn item_xref() -> EntityMeta {
    EntityMeta::new("edi_item_xref", "EDI item number")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(partner_field().help("Empty: every partner of the company. Only a GTIN may be for every partner."))
        .field(
            FieldMeta::new("item_id", "Item", FieldType::link("item"))
                .required(),
        )
        .field(
            FieldMeta::new(
                "kind",
                "Kind",
                FieldType::labelled(&[
                    ("gtin", "GTIN (UPC, EAN)"),
                    ("buyer_item", "Buyer's item number"),
                    ("seller_item", "Our item number as the partner sends it"),
                ]),
            )
            .required(),
        )
        .field(FieldMeta::new("value", "Number", FieldType::Text).required())
        .field(
            FieldMeta::new("partner_uom", "Partner's unit", FieldType::Text)
                .help("The unit the partner orders this item in, such as CA for a case."),
        )
        .field(
            FieldMeta::new("units_per", "Units per", FieldType::Decimal)
                .quantity()
                .help("How many of the item's own unit one of the partner's unit is: 6 for a case of 6."),
        )
        .field(created())
        .order_by(&["value"])
        .search(&["value"])
        .display(&["kind", "value"])
        .plural("EDI item numbers")
        .company_field("company_id")
}

fn uom_xref() -> EntityMeta {
    EntityMeta::new("edi_uom_xref", "EDI unit")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(partner_field().help("Empty: every partner of the company."))
        .field(
            FieldMeta::new("partner_uom", "Partner's unit", FieldType::Text)
                .required()
                .help("The code the partner sends, such as EA."),
        )
        .field(
            FieldMeta::new("uom", "Our unit", FieldType::Text)
                .required()
                .help("The unit as items keep it."),
        )
        .field(created())
        .order_by(&["partner_uom"])
        .search(&["partner_uom", "uom"])
        .display(&["partner_uom", "uom"])
        .plural("EDI units")
        .company_field("company_id")
}

fn location_xref() -> EntityMeta {
    EntityMeta::new("edi_location_xref", "EDI location")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(partner_field().required())
        .field(
            FieldMeta::new("qualifier", "Qualifier", FieldType::Text)
                .required()
                .help("What kind of number the partner sends for the place (N103)."),
        )
        .field(
            FieldMeta::new("code", "Code", FieldType::Text)
                .required()
                .help("The partner's number for the store or warehouse (N104)."),
        )
        .field(
            FieldMeta::new("address_id", "Address", FieldType::Uuid)
                .required()
                .help(
                    "One of the shipping addresses of the partner's customer (party.get). \
                     Empty when that address was removed or the partner names another \
                     customer now: an order to this place then asks for one.",
                ),
        )
        .field(
            FieldMeta::new("warehouse_id", "Ships from", FieldType::link("warehouse"))
                .help("The warehouse orders to this place ship from, over the partner's own."),
        )
        .field(created())
        .order_by(&["code"])
        .search(&["code"])
        .display(&["qualifier", "code"])
        .plural("EDI locations")
        .company_field("company_id")
}

fn interchange() -> EntityMeta {
    EntityMeta::new("edi_interchange", "EDI interchange")
        .field(FieldMeta::new("company_id", "Company", FieldType::link("company")).readonly())
        .field(partner_field().readonly())
        .field(
            FieldMeta::new("direction", "Direction", FieldType::options(&["in", "out"])).readonly(),
        )
        .field(
            FieldMeta::new("syntax", "Syntax", FieldType::options(&["x12", "edifact"])).readonly(),
        )
        .field(FieldMeta::new("control", "Control number", FieldType::Text).readonly())
        .field(FieldMeta::new("usage", "Usage", FieldType::Text).readonly())
        .field(FieldMeta::new("sha256", "SHA-256", FieldType::Text).readonly())
        .field(FieldMeta::new("size", "Bytes", FieldType::Int).readonly())
        .field(FieldMeta::new("transport", "Transport", FieldType::Text).readonly())
        .field(FieldMeta::new("transport_ref", "Transport reference", FieldType::Text).readonly())
        .field(
            FieldMeta::new(
                "state",
                "State",
                FieldType::options(&[
                    "stored",
                    "unreadable",
                    "unroutable",
                    "read",
                    "ready",
                    "sent",
                    "failed",
                ]),
            )
            .readonly(),
        )
        .field(FieldMeta::new("error", "Error", FieldType::Text).readonly())
        .field(created())
        .field(FieldMeta::new("sent_at", "Sent", FieldType::Timestamp).readonly())
        .order_by(&["created_at"])
        .search(&["control", "sha256"])
        .display(&["direction", "control"])
        .plural("EDI interchanges")
        .company_field("company_id")
        .read_only()
}

fn message() -> EntityMeta {
    EntityMeta::new("edi_message", "EDI message")
        .field(
            FieldMeta::new(
                "interchange_id",
                "Interchange",
                FieldType::link("edi_interchange"),
            )
            .readonly(),
        )
        .field(FieldMeta::new("company_id", "Company", FieldType::link("company")).readonly())
        .field(partner_field().readonly())
        .field(
            FieldMeta::new("direction", "Direction", FieldType::options(&["in", "out"])).readonly(),
        )
        .field(FieldMeta::new("message", "Message", FieldType::Text).readonly())
        .field(FieldMeta::new("group_control", "Group control number", FieldType::Text).readonly())
        .field(FieldMeta::new("control", "Control number", FieldType::Text).readonly())
        .field(FieldMeta::new("position", "Position", FieldType::Int).readonly())
        .field(
            FieldMeta::new(
                "state",
                "State",
                FieldType::options(&[
                    "received",
                    "rejected",
                    "test",
                    "proposed",
                    "drafted",
                    "dismissed",
                    "invalid",
                    "ready",
                    "sent",
                    "acknowledged",
                    "acknowledged_with_errors",
                    "failed",
                ]),
            )
            .readonly(),
        )
        .field(FieldMeta::new("business_key", "Order number", FieldType::Text).readonly())
        .field(FieldMeta::new("purpose", "Purpose", FieldType::Text).readonly())
        .field(FieldMeta::new("doc_type", "Document type", FieldType::Text).readonly())
        .field(
            FieldMeta::new("document_id", "Document", FieldType::Uuid)
                .readonly()
                .help("The document of the type beside it."),
        )
        .field(FieldMeta::new("answers_id", "Answers", FieldType::link("edi_message")).readonly())
        .field(
            FieldMeta::new(
                "answers_interchange_id",
                "Answers interchange",
                FieldType::link("edi_interchange"),
            )
            .readonly(),
        )
        .field(
            FieldMeta::new(
                "superseded_by",
                "Superseded by",
                FieldType::link("edi_message"),
            )
            .readonly(),
        )
        .field(FieldMeta::new("findings", "Findings", FieldType::Json).readonly())
        .field(FieldMeta::new("reason", "Reason dismissed", FieldType::Text).readonly())
        .field(FieldMeta::new("ack_status", "Acknowledged", FieldType::Text).readonly())
        .field(
            FieldMeta::new(
                "ack_message_id",
                "Acknowledgement",
                FieldType::link("edi_message"),
            )
            .readonly(),
        )
        .field(FieldMeta::new("ack_due_at", "Acknowledgement due", FieldType::Timestamp).readonly())
        .field(created())
        .order_by(&["created_at"])
        .search(&["business_key", "control"])
        .display(&["message", "business_key"])
        .plural("EDI messages")
        .company_field("company_id")
        .read_only()
}

pub fn settings() -> EntityMeta {
    EntityMeta::new("edi_settings", "EDI settings")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new("gs1_company_prefix", "GS1 company prefix", FieldType::Text).help(
                "The 7 to 10 digits GS1 gave the company: what its cartons' SSCCs begin with.",
            ),
        )
        .field(
            FieldMeta::new("sscc_extension", "SSCC extension digit", FieldType::Int)
                .help("The digit in front of the prefix, 0 to 9; 0 unless you have a reason."),
        )
        .field(
            FieldMeta::new("next_sscc_serial", "Next SSCC serial", FieldType::Int)
                .help("The serial the next carton gets. It only goes up: a serial is used once."),
        )
        .field(created())
        .order_by(&["company_id"])
        .display(&["gs1_company_prefix"])
        .plural("EDI settings")
        .company_field("company_id")
}

fn guide() -> EntityMeta {
    EntityMeta::new("edi_guide", "EDI guide")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(FieldMeta::new("code", "Code", FieldType::Text).required())
        .field(FieldMeta::new("name", "Name", FieldType::Text).required())
        .field(
            FieldMeta::new(
                "syntax",
                "Syntax",
                FieldType::labelled(&[("x12", "ANSI X12"), ("edifact", "UN/EDIFACT")]),
            )
            .required(),
        )
        .field(
            FieldMeta::new(
                "version",
                "Version",
                FieldType::options(&["004010", "005010", "D96A"]),
            )
            .required(),
        )
        .field(
            FieldMeta::new("base_guide_id", "Builds on", FieldType::link("edi_guide")).help(
                "A guide of the company this one builds on, such as a retailer's general guide \
                 under the guide of one of its divisions. One level only.",
            ),
        )
        .field(FieldMeta::new("notes", "Notes", FieldType::Text))
        .field(created())
        .order_by(&["code"])
        .search(&["code", "name"])
        .display(&["code", "name"])
        .plural("EDI guides")
        .company_field("company_id")
}

fn guide_message() -> EntityMeta {
    EntityMeta::new("edi_guide_message", "EDI guide message")
        .field(
            FieldMeta::new("company_id", "Company", FieldType::link("company"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new("guide_id", "Guide", FieldType::link("edi_guide"))
                .required()
                .immutable(),
        )
        .field(
            FieldMeta::new("message", "Message", FieldType::Text)
                .required()
                .help("Such as 850 or ORDERS."),
        )
        .field(
            FieldMeta::new(
                "direction",
                "Direction",
                FieldType::labelled(&[("in", "Received"), ("out", "Sent")]),
            )
            .required(),
        )
        .field(
            FieldMeta::new("spec", "Spec", FieldType::Json)
                .required()
                .help(
                    "What the guide says of the message: options, constraints and extra segments. \
                 Checked when it is saved.",
                ),
        )
        .order_by(&["message", "direction"])
        .display(&["message", "direction"])
        .plural("EDI guide messages")
        .child_of("edi_guide", "guide_id")
        .written_together(&["message", "direction", "spec"])
        .validator(check_guide_message)
        .company_field("company_id")
}

/// A guide's spec is checked when it is saved: what it names exists, and
/// its expressions parse and read only declared facts.
fn check_guide_message(_: &Registry, data: &Map<String, Value>) -> Result<()> {
    let (Some(message), Some(direction), Some(spec)) =
        (data.get("message"), data.get("direction"), data.get("spec"))
    else {
        return Ok(());
    };
    let message = message.as_str().unwrap_or_default();
    let syntax = if message.bytes().all(|b| b.is_ascii_digit()) {
        "x12"
    } else {
        "edifact"
    };
    let spec = crate::guide::parse(spec).map_err(|why| Error::field("spec", why))?;
    crate::guide::check(
        syntax,
        message,
        direction.as_str().unwrap_or_default(),
        &spec,
    )
    .map_err(|why| Error::field("spec", why))
}

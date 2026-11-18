//! One sentence per segment, in our own words, for a person or an agent
//! reading a message: `PO1*1*48*EA*12.5` is "Line 1: 48 EA at 12.5".
//!
//! Only the segments the readers and writers of this crate act on are
//! explained, and only by what this crate reads of them. Anything else is
//! shown by its tag alone: this is not a dictionary of the standard.

use crate::segment::Segment;

/// The sentence for one segment. `set` is the transaction set or message
/// it belongs to (`850`, `997`), which a few tags need to be read right;
/// empty for an envelope segment.
pub fn explain(segment: &Segment, set: &str) -> String {
    let g = |n: usize| segment.get(n);
    let join = |parts: &[&str]| -> String {
        parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let date = |text: &str| -> String {
        if text.len() == 8 && text.bytes().all(|b| b.is_ascii_digit()) {
            format!("{}-{}-{}", &text[..4], &text[4..6], &text[6..])
        } else {
            text.to_owned()
        }
    };
    let pairs = |from: usize, to: usize| -> String {
        (from..=to)
            .step_by(2)
            .filter(|n| !g(*n).is_empty() && !g(n + 1).is_empty())
            .map(|n| format!("{} {}", g(n), g(n + 1)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let with_ids = |sentence: String, ids: String| {
        if ids.is_empty() {
            sentence
        } else {
            format!("{sentence}; {ids}")
        }
    };
    match segment.tag.as_str() {
        "ISA" => format!(
            "Interchange {} from {}/{} to {}/{}, {}",
            g(13).trim(),
            g(5).trim(),
            g(6).trim(),
            g(7).trim(),
            g(8).trim(),
            match g(15) {
                "T" => "a test",
                "P" => "production",
                _ => "of unknown usage",
            }
        ),
        "IEA" => format!("End of interchange {}: {} groups", g(2), g(1)),
        "GS" => format!(
            "Group {} of kind {} from {} to {}, version {}",
            g(6),
            g(1),
            g(2),
            g(3),
            g(8)
        ),
        "GE" => format!("End of group {}: {} sets", g(2), g(1)),
        "ST" => format!("Set {} number {}", g(1), g(2)),
        "SE" => format!("End of set {}: {} segments", g(2), g(1)),
        "TA1" => format!("Interchange {} {}, note {}", g(1), status(g(4)), g(5)),
        "BEG" => format!(
            "Order {}{} dated {}, purpose {}, type {}",
            g(3),
            if g(4).is_empty() {
                String::new()
            } else {
                format!(" release {}", g(4))
            },
            date(g(5)),
            g(1),
            g(2)
        ),
        "BAK" => format!(
            "Acknowledgement of order {} dated {}: {}",
            g(3),
            date(g(4)),
            match g(2) {
                "AD" => "accepted as sent",
                "AC" => "accepted with changes",
                "RD" => "rejected",
                other => other,
            }
        ),
        "CUR" => format!("Amounts in {}", g(2)),
        "REF" => format!("Reference {}: {}", g(1), join(&[g(2), g(3)])),
        "DTM" => format!(
            "{}: {}",
            match g(1) {
                "002" => "Delivery requested",
                "010" => "Ship requested",
                "037" => "Ship not before",
                "038" => "Ship no later than",
                "001" => "Cancel after",
                "011" => "Shipped",
                "017" => "Expected delivery",
                other => return format!("Date {other}: {}", date(g(2))),
            },
            date(g(2))
        ),
        "ITD" => {
            let mut parts = vec![];
            if !g(3).is_empty() {
                parts.push(format!("{}% off within {} days", g(3), g(5)));
            }
            if !g(7).is_empty() {
                parts.push(format!("net {} days", g(7)));
            }
            if !g(12).is_empty() {
                parts.push(g(12).to_owned());
            }
            format!("Terms: {}", parts.join(", "))
        }
        "FOB" => format!("Freight paid by: {}", g(1)),
        "TD5" => format!("Carrier: {}", join(&[g(2), g(3), g(5)])),
        "TD1" => format!("{} {} packages, {} {}", g(2), g(1), g(7), g(8)),
        "TD3" => format!("Equipment {} {}", g(2), g(3)),
        "N1" => format!(
            "Party {}: {}{}",
            g(1),
            g(2),
            if g(4).is_empty() {
                String::new()
            } else {
                format!(" ({} {})", g(3), g(4))
            }
        ),
        "N2" | "N3" => format!("Address: {}", join(&[g(1), g(2)])),
        "N4" => format!("Place: {}", join(&[g(1), g(2), g(3), g(4)])),
        "PO1" => with_ids(
            format!("Line {}: {} {} at {}", g(1), g(2), g(3), g(4)),
            pairs(6, 24),
        ),
        "ACK" => format!(
            "Line answer: {} {} {}",
            match g(1) {
                "IA" => "accepted,",
                "IQ" => "quantity changed to",
                "IP" => "price changed,",
                "IR" => "rejected,",
                "IB" => "backordered,",
                other => other,
            },
            g(2),
            g(3)
        ),
        "PID" => format!("Description: {}", g(5)),
        "PO4" => format!("Pack: {} per case", g(1)),
        "SDQ" => format!("Split over locations, in {}: {}", g(1), pairs(3, 21)),
        "CTT" => format!("{} lines, quantities add up to {}", g(1), g(2)),
        "AMT" => format!("Amount {}: {}", g(1), g(2)),
        "BSN" => format!("Ship notice {} dated {} {}", g(2), date(g(3)), g(4)),
        "HL" => format!(
            "Level {} ({}){}",
            g(1),
            match g(3) {
                "S" => "shipment",
                "O" => "order",
                "T" => "pallet",
                "P" => "carton",
                "I" => "item",
                other => other,
            },
            if g(2).is_empty() {
                String::new()
            } else {
                format!(" under level {}", g(2))
            }
        ),
        "PRF" => format!("For order {}", g(1)),
        "MAN" => format!("Carton label {} {}", g(1), g(2)),
        "LIN" => with_ids(format!("Item of line {}", g(1)), pairs(2, 20)),
        "SN1" => format!(
            "Shipped {} {}{}",
            g(2),
            g(3),
            if g(1).is_empty() {
                String::new()
            } else {
                format!(" of line {}", g(1))
            }
        ),
        "BIG" => format!("Invoice {} dated {} for order {}", g(2), date(g(1)), g(4)),
        "IT1" => with_ids(
            format!("Invoice line {}: {} {} at {}", g(1), g(2), g(3), g(4)),
            pairs(6, 24),
        ),
        "TDS" => format!("Invoice total, in cents: {}", g(1)),
        "TXI" => format!("Tax {}: {}", g(1), g(2)),
        "SAC" => format!("Charge or allowance {}: {}", g(2), g(5)),
        "CAD" => format!("Carrier: {}", g(5)),
        "ISS" => format!("{} {} shipped", g(1), g(2)),
        "AK1" => format!("Answers group {} of kind {}", g(2), g(1)),
        "AK2" => format!("Answers set {} number {}", g(1), g(2)),
        "AK3" | "IK3" => format!(
            "Segment {} at position {} is in error (code {})",
            g(1),
            g(2),
            g(4)
        ),
        "AK4" | "IK4" => format!(
            "Element {} is in error (code {}){}",
            segment.component(1, 1),
            g(3),
            if g(4).is_empty() {
                String::new()
            } else {
                format!(": {}", g(4))
            }
        ),
        "AK5" | "IK5" => format!("The set is {}", status(g(1))),
        "AK9" => format!(
            "The group is {}: {} sets stated, {} received, {} accepted",
            status(g(1)),
            g(2),
            g(3),
            g(4)
        ),
        "CTX" if set == "999" => "Context of the error".to_owned(),
        // EDIFACT.
        "UNA" => "Separators of the interchange".to_owned(),
        "UNB" => format!(
            "Interchange {} from {} to {}",
            g(5),
            segment.component(2, 1),
            segment.component(3, 1)
        ),
        "UNZ" => format!("End of interchange {}: {} messages", g(2), g(1)),
        "UNH" => format!("Message {} number {}", segment.component(2, 1), g(1)),
        "UNT" => format!("End of message {}: {} segments", g(2), g(1)),
        "BGM" => format!("Document {} number {}", segment.component(1, 1), g(2)),
        "NAD" => format!("Party {}: {}", g(1), segment.component(2, 1)),
        "QTY" => format!(
            "Quantity {}: {}",
            segment.component(1, 1),
            segment.component(1, 2)
        ),
        "PRI" => format!(
            "Price {}: {}",
            segment.component(1, 1),
            segment.component(1, 2)
        ),
        tag => tag.to_owned(),
    }
}

fn status(code: &str) -> &'static str {
    match code {
        "A" => "accepted",
        "E" => "accepted with errors",
        "P" => "partly accepted",
        "R" | "M" | "W" | "X" => "rejected",
        _ => "of unknown status",
    }
}

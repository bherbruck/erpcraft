//! The spike's proof: one tokenizer reads X12 and EDIFACT with the
//! separators each file states, an 850 becomes a typed order, a 997 is
//! written for it and read back, and no input makes any of it panic.
//!
//! The parties in the fixtures are made up. The GTINs begin with 04, a
//! prefix GS1 keeps for use inside a company, so they are nobody's.

use chrono::NaiveDate;
use erpcraft_edi::segment::{Delimiters, MAX_BYTES, MAX_COMPONENTS_PER_SEGMENT};
use erpcraft_edi::x12::ack::{FunctionalAck, SetVerdict, Status};
use erpcraft_edi::x12::{self, Envelope, GroupError, InterchangeError, SetError, po850};
use erpcraft_edi::{Syntax, edifact, gs1, syntax_of};
use rust_decimal::Decimal;

const PO850: &str = include_str!("fixtures/po850.x12");
const ORDERS: &str = include_str!("fixtures/orders.edi");

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// The fixture with other separators, as a partner might send it.
fn reseparated(element: char, component: char, terminator: &str) -> String {
    PO850
        .replace('>', "\u{1}")
        .replace('*', &element.to_string())
        .replace("~\n", terminator)
        .replace('\u{1}', &component.to_string())
}

#[test]
fn the_envelope_is_read_with_the_separators_of_the_isa() {
    let interchanges = x12::read(PO850.as_bytes()).unwrap();
    assert_eq!(interchanges.len(), 1);
    let interchange = &interchanges[0];
    assert_eq!(interchange.isa.sender_id, "NORTHFIELDMERC");
    assert_eq!(interchange.isa.receiver_id, "CEDARHOLLOW");
    assert_eq!(interchange.isa.version, "00401");
    assert_eq!(interchange.isa.control, "000000417");
    assert_eq!(interchange.isa.usage, 'P');
    assert_eq!(interchange.isa.delimiters, Delimiters::X12);
    assert!(interchange.errors.is_empty());
    let group = &interchange.groups[0];
    assert_eq!(
        (group.functional_id.as_str(), group.version.as_str()),
        ("PO", "004010")
    );
    assert!(group.errors.is_empty());
    let set = &group.sets[0];
    assert_eq!((set.id.as_str(), set.control.as_str()), ("850", "0001"));
    assert_eq!(set.segments.len(), 28);
    assert!(set.errors.is_empty());

    // The same order with a pipe, a caret and a line end for separators,
    // and with control characters, reads to the same segments.
    for other in [
        reseparated('|', '^', "\n"),
        reseparated('\u{1d}', '\u{1f}', "\u{1e}"),
        reseparated('*', ':', "~"),
        reseparated('*', '>', "~\r\n"),
    ] {
        let read = x12::read(other.as_bytes()).unwrap();
        let theirs = &read[0].groups[0].sets[0];
        assert_eq!(theirs.segments.len(), set.segments.len());
        for (a, b) in theirs.segments.iter().zip(&set.segments) {
            assert_eq!((&a.tag, &a.elements), (&b.tag, &b.elements));
        }
    }
}

#[test]
fn what_was_read_is_written_back_byte_for_byte() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let set = &interchange.groups[0].sets[0];
    let written = x12::write_segments(&set.segments, &interchange.isa.delimiters).unwrap();
    let from = PO850.find("ST*").unwrap();
    let to = PO850.find("GE*").unwrap();
    assert_eq!(written, PO850[from..to].replace('\n', ""));
}

#[test]
fn an_850_becomes_a_typed_order() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let (order, verdict) = po850::read(&interchange.groups[0].sets[0]);
    assert_eq!(verdict, SetVerdict::default());

    assert_eq!(order.purpose, "00");
    assert_eq!(order.order_type, "SA");
    assert_eq!(order.number, "4500018842");
    assert_eq!(order.date, Some(date(2026, 11, 18)));
    assert_eq!(order.currency.as_deref(), Some("USD"));
    assert_eq!(order.freight_terms.as_deref(), Some("CC"));
    assert_eq!(order.references.len(), 2);
    assert_eq!(order.references[1].qualifier, "IA");
    assert_eq!(order.references[1].value, "0071934");
    let dates: Vec<_> = order
        .dates
        .iter()
        .map(|d| (d.qualifier.as_str(), d.date))
        .collect();
    assert_eq!(
        dates,
        [
            ("037", date(2026, 12, 1)),
            ("038", date(2026, 12, 5)),
            ("001", date(2026, 12, 12))
        ]
    );
    let terms = &order.terms[0];
    assert_eq!(terms.discount_percent, Some(dec("2")));
    assert_eq!((terms.discount_days, terms.net_days), (Some(10), Some(30)));

    let roles: Vec<_> = order.parties.iter().map(|p| p.role.as_str()).collect();
    assert_eq!(roles, ["BT", "ST", "VN"]);
    let ship_to = &order.parties[1];
    assert_eq!(ship_to.name, "NORTHFIELD DC 07");
    assert_eq!(
        (ship_to.id_qualifier.as_str(), ship_to.id.as_str()),
        ("92", "0007")
    );
    assert_eq!(ship_to.address_lines, ["2200 LOGISTICS PKWY", "DOCK 14"]);
    assert_eq!(
        (
            ship_to.city.as_str(),
            ship_to.region.as_str(),
            ship_to.postal_code.as_str()
        ),
        ("COLUMBUS", "OH", "43217")
    );

    assert_eq!(order.lines.len(), 3);
    let first = &order.lines[0];
    assert_eq!(first.line_id, "1");
    assert_eq!(first.quantity, Some(dec("48")));
    assert_eq!(first.uom, "EA");
    assert_eq!(first.unit_price, Some(dec("12.5")));
    let ids: Vec<_> = first
        .product_ids
        .iter()
        .map(|p| (p.qualifier.as_str(), p.value.as_str()))
        .collect();
    assert_eq!(
        ids,
        [
            ("UP", "040000100010"),
            ("VN", "CH-1001"),
            ("IN", "88213001")
        ]
    );
    assert_eq!(first.descriptions, ["CLAW HAMMER 16OZ FIBERGLASS"]);
    assert_eq!(first.pack.as_ref().unwrap().per_case, Some(dec("12")));
    // Every GTIN in the fixture carries its right check digit.
    for line in &order.lines {
        assert!(gs1::is_valid(&line.product_ids[0].value));
    }
    let second = &order.lines[1];
    let split: Vec<_> = second
        .allocations
        .iter()
        .map(|a| (a.location.as_str(), a.quantity))
        .collect();
    assert_eq!(split, [("0007", dec("80")), ("0011", dec("40"))]);

    assert_eq!(order.lines_stated, Some(3));
    assert_eq!(order.hash_total, Some(dec("192")));
    assert!(order.notes.is_empty(), "{:?}", order.notes);
    // The routing segment has no field here, and the order says so.
    assert_eq!(order.unhandled, [(11, "TD5".to_owned())]);
}

fn reply(interchange: &x12::Interchange, ack: &FunctionalAck) -> String {
    let group = &interchange.groups[0];
    let at = date(2026, 11, 18).and_hms_opt(9, 31, 0).unwrap();
    let envelope = Envelope::reply_to(interchange, group, "FA", at, 52, 52);
    x12::write(&envelope, "997", &[ack.body(&envelope.delimiters)]).unwrap()
}

#[test]
fn a_997_accepts_a_good_order_and_reads_back_as_it_was_built() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let ack = FunctionalAck::of(&interchange.groups[0], |set| po850::read(set).1);
    assert_eq!(ack.status, Status::Accepted);
    assert_eq!((ack.received, ack.accepted), (1, 1));

    let written = reply(interchange, &ack);
    assert_eq!(
        written,
        "ISA*00*          *00*          *ZZ*CEDARHOLLOW    *ZZ*NORTHFIELDMERC *261118*0931\
         *U*00401*000000052*0*P*>~\
         GS*FA*CEDARHOLLOW*NORTHFIELDMERC*20261118*0931*52*X*004010~\
         ST*997*0001~AK1*PO*417~AK2*850*0001~AK5*A~AK9*A*1*1*1~SE*6*0001~\
         GE*1*52~IEA*1*000000052~"
    );

    // The partner's side: the reply is a sound interchange addressed
    // back, and it says the same as what was built.
    let back = &x12::read(written.as_bytes()).unwrap()[0];
    assert!(back.errors.is_empty());
    assert_eq!(back.isa.sender_id, interchange.isa.receiver_id);
    assert_eq!(back.isa.receiver_id, interchange.isa.sender_id);
    let group = &back.groups[0];
    assert!(group.errors.is_empty() && group.sets[0].errors.is_empty());
    assert_eq!(FunctionalAck::read(&group.sets[0]).unwrap(), ack);
}

#[test]
fn a_997_rejects_a_broken_order_and_says_where() {
    // A quantity that is a composite where a number belongs, a date that is no date, and one
    // segment fewer than the SE counts.
    let broken = PO850
        .replace("PO1*1*48*EA", "PO1*1*4>8*EA")
        .replace("DTM*038*20261205", "DTM*038*20261305")
        .replace("FOB*CC~\n", "");
    let interchange = &x12::read(broken.as_bytes()).unwrap()[0];
    let set = &interchange.groups[0].sets[0];
    assert_eq!(set.errors, [SetError::SegmentCountMismatch]);

    let (order, verdict) = po850::read(set);
    assert_eq!(order.lines[0].quantity, None);
    let ack = FunctionalAck::of(&interchange.groups[0], |_| verdict.clone());
    assert_eq!(ack.status, Status::Rejected);
    assert_eq!((ack.received, ack.accepted), (1, 0));
    let set_ack = &ack.sets[0];
    assert_eq!(set_ack.status, Status::Rejected);
    assert_eq!(set_ack.codes, [4, 5]);
    let places: Vec<_> = set_ack
        .segments
        .iter()
        .map(|s| {
            (
                s.tag.as_str(),
                s.position,
                s.elements[0].position,
                s.elements[0].code,
            )
        })
        .collect();
    assert_eq!(places, [("DTM", 8, 2, 8), ("PO1", 18, 2, 6)]);

    let written = reply(interchange, &ack);
    // The bad quantity is copied without the separator it held: X12
    // cannot write one inside data.
    assert!(
        written.contains("AK3*DTM*8**8~AK4*2**8*20261305~AK3*PO1*18**8~AK4*2**6*4~AK5*R*4*5~"),
        "{written}"
    );
    let back = &x12::read(written.as_bytes()).unwrap()[0];
    let read = FunctionalAck::read(&back.groups[0].sets[0]).unwrap();
    assert_eq!(read.sets[0].codes, [4, 5]);
    assert_eq!(read.status, Status::Rejected);
}

#[test]
fn envelopes_that_do_not_agree_are_read_and_reported() {
    let wrong = PO850
        .replace("SE*28*0001", "SE*28*0002")
        .replace("GE*1*417", "GE*2*418")
        .replace("IEA*1*000000417", "IEA*3*000000400");
    let interchange = &x12::read(wrong.as_bytes()).unwrap()[0];
    assert_eq!(
        interchange.errors,
        [
            InterchangeError::GroupCountMismatch,
            InterchangeError::ControlNumberMismatch
        ]
    );
    let group = &interchange.groups[0];
    assert_eq!(
        group.errors,
        [
            GroupError::SetCountMismatch,
            GroupError::ControlNumberMismatch
        ]
    );
    assert_eq!(group.sets[0].errors, [SetError::ControlNumberMismatch]);
    let ack = FunctionalAck::of(group, |set| po850::read(set).1);
    assert_eq!(ack.status, Status::Rejected);
    assert_eq!(ack.codes, [5, 4]);
    assert_eq!(ack.sets_stated, "2");

    // A set that is not an 850 is acknowledged as not supported.
    let other = PO850.replace("ST*850*", "ST*875*");
    let interchange = &x12::read(other.as_bytes()).unwrap()[0];
    let ack = FunctionalAck::of(&interchange.groups[0], |set| po850::read(set).1);
    assert_eq!(ack.sets[0].codes, [1]);
}

#[test]
fn version_00501_states_a_repetition_separator() {
    let newer = PO850
        .replace("*U*00401*", "*^*00501*")
        .replace("*004010~", "*005010~")
        .replace("REF*DP*042", "REF*DP*042^043");
    let interchange = &x12::read(newer.as_bytes()).unwrap()[0];
    assert_eq!(interchange.isa.delimiters.repetition, Some(b'^'));
    let reference = &interchange.groups[0].sets[0].segments[3];
    let element = reference.element(2).unwrap();
    assert_eq!(element.repetitions(), 2);
    assert_eq!(element.components(1), ["043"]);
    // In 00401 the same character is data.
    let older = PO850.replace("REF*DP*042", "REF*DP*042^043");
    let interchange = &x12::read(older.as_bytes()).unwrap()[0];
    assert_eq!(interchange.groups[0].sets[0].segments[3].get(2), "042^043");
}

#[test]
fn a_file_that_cannot_be_followed_is_refused_with_its_place() {
    let refused = |bytes: &[u8]| x12::read(bytes).unwrap_err().message;
    assert!(refused(b"").contains("empty"));
    assert!(refused(b"hello").contains("ISA"));
    // Cut off anywhere before its end, a file is never read as whole.
    let whole = PO850.trim_end().as_bytes();
    for cut in 1..whole.len() {
        assert!(x12::read(&whole[..cut]).is_err(), "cut at {cut}");
    }
    assert!(refused(PO850.replace("IEA*1*000000417~", "").as_bytes()).contains("cut off"));
    assert!(refused(PO850.replace("ST*850*0001~\n", "").as_bytes()).contains("outside"));
    assert!(refused(PO850.replace("CUR*BY*USD", "CUR*BY*U\u{0}D").as_bytes()).contains("control"));
    assert!(refused(PO850.replace("*>~", "**~").as_bytes()).contains("same character"));
    assert!(refused(PO850.replace("*>~", "*A~").as_bytes()).contains("separator"));
    let wide = PO850.replace("CUR*BY*USD", &format!("CUR{}", "*".repeat(600)));
    assert!(refused(wide.as_bytes()).contains(&MAX_COMPONENTS_PER_SEGMENT.to_string()));
    assert!(refused(&vec![b'A'; MAX_BYTES + 1]).contains("larger"));
    // Many segments are refused by count, not by running out of memory.
    let mut many = PO850[..PO850.find("SE*").unwrap()].to_owned();
    many.push_str(&"REF~".repeat(250_000));
    assert!(refused(many.as_bytes()).contains("segments"));
}

#[test]
fn no_change_to_a_file_makes_reading_it_panic() {
    // Every byte of both fixtures replaced in turn by bytes that mean
    // something to a parser. Reading may refuse; it must return.
    for fixture in [PO850, ORDERS] {
        let bytes = fixture.as_bytes();
        for at in 0..bytes.len() {
            for with in [
                b'*', b'~', b'>', b'+', b':', b'\'', b'?', b'9', b'-', b'.', 0, 0xFF,
            ] {
                let mut changed = bytes.to_vec();
                changed[at] = with;
                if let Ok(interchanges) = x12::read(&changed) {
                    for group in interchanges.iter().flat_map(|i| &i.groups) {
                        let ack = FunctionalAck::of(group, |set| po850::read(set).1);
                        let _ = ack.body(&interchanges[0].isa.delimiters);
                    }
                }
                let _ = edifact::read(&changed);
            }
        }
    }
}

#[test]
fn the_writer_never_puts_a_separator_inside_x12_data() {
    let interchange = &x12::read(PO850.as_bytes()).unwrap()[0];
    let at = date(2026, 11, 18).and_hms_opt(9, 31, 0).unwrap();
    let envelope = Envelope::reply_to(interchange, &interchange.groups[0], "FA", at, 1, 1);
    let body = vec![erpcraft_edi::Segment::new("AK1", &["PO", "4~17"])];
    let refused = x12::write(&envelope, "997", &[body]).unwrap_err();
    assert!(refused.message.contains("separator"), "{refused}");
    let mut long = envelope.clone();
    long.sender_id = "MORE THAN FIFTEEN CHARACTERS".to_owned();
    assert!(x12::write(&long, "997", &[]).is_err());
}

#[test]
fn the_same_tokenizer_reads_edifact_with_the_separators_of_the_una() {
    assert_eq!(syntax_of(PO850.as_bytes()), Some(Syntax::X12));
    assert_eq!(syntax_of(ORDERS.as_bytes()), Some(Syntax::Edifact));

    let interchanges = edifact::read(ORDERS.as_bytes()).unwrap();
    let interchange = &interchanges[0];
    assert!(interchange.una);
    assert_eq!(interchange.delimiters.decimal, b',');
    assert_eq!(interchange.delimiters.release, Some(b'?'));
    assert_eq!(interchange.syntax_id, "UNOC");
    assert_eq!(interchange.syntax_version, "3");
    assert_eq!(interchange.sender, "4012345000009");
    assert_eq!(interchange.sender_qualifier, "14");
    assert_eq!(interchange.control, "417");
    assert!(interchange.errors.is_empty());
    assert!(gs1::is_valid(&interchange.sender) && gs1::is_valid(&interchange.receiver));

    let message = &interchange.messages[0];
    assert_eq!(
        [
            message.kind.as_str(),
            message.version.as_str(),
            message.release.as_str(),
            message.agency.as_str(),
            message.association.as_str()
        ],
        ["ORDERS", "D", "96A", "UN", "EAN008"]
    );
    assert!(message.errors.is_empty());
    assert_eq!(message.segments.len(), 21);

    let find = |tag: &str, nth: usize| {
        message
            .segments
            .iter()
            .filter(|s| s.tag == tag)
            .nth(nth)
            .unwrap()
    };
    assert_eq!(find("BGM", 0).get(2), "4500018842");
    assert_eq!(find("DTM", 1).component(1, 2), "20261205");
    let deliver_to = find("NAD", 2);
    assert_eq!(deliver_to.get(1), "DP");
    assert_eq!(deliver_to.component(2, 3), "9");
    assert_eq!(deliver_to.get(4), "NORDFELD HANDEL GMBH");
    assert_eq!(deliver_to.get(8), "34117");
    assert_eq!(find("LIN", 1).component(3, 1), "4098765100024");
    assert!(gs1::is_valid(find("LIN", 1).component(3, 1)));
    // Released separators are data, and a released release character is
    // one question mark.
    assert_eq!(
        find("IMD", 1).component(3, 4),
        "BITSATZ 6+1, WAS IST DAS? 'PROFI'"
    );
    // The UNA says a comma is the decimal mark.
    let price = find("PRI", 0).component(1, 2);
    assert_eq!(price, "12,5");
    assert_eq!(
        edifact::number(price, &interchange.delimiters),
        Some(dec("12.5"))
    );
    assert_eq!(edifact::number("12.5", &interchange.delimiters), None);
    assert_eq!(edifact::number("1e5", &interchange.delimiters), None);

    // Without a UNA the defaults apply; counts that are wrong are
    // reported with CONTRL's codes, and a file cut off is refused.
    let plain = ORDERS
        .replace("UNA:+,? '\n", "")
        .replace("UNT+21+1", "UNT+20+2")
        .replace("UNZ+1+417", "UNZ+2+417");
    let interchange = &edifact::read(plain.as_bytes()).unwrap()[0];
    assert!(!interchange.una);
    assert_eq!(interchange.delimiters, Delimiters::EDIFACT);
    assert_eq!(interchange.messages[0].errors, [29, 28]);
    assert_eq!(interchange.errors, [29]);
    let whole = ORDERS.trim_end().as_bytes();
    for cut in 1..whole.len() {
        assert!(edifact::read(&whole[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn text_outside_ascii_is_read_as_utf8_or_else_as_latin_1() {
    let utf8 = ORDERS.replace("NORDFELD HANDEL", "NORDFELD HÄNDEL");
    let read = &edifact::read(utf8.as_bytes()).unwrap()[0];
    let name = |i: &edifact::Interchange| i.messages[0].segments[6].get(4).to_owned();
    assert_eq!(name(read), "NORDFELD HÄNDEL GMBH");
    let latin1: Vec<u8> = utf8
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap())
        .collect();
    assert_eq!(
        name(&edifact::read(&latin1).unwrap()[0]),
        "NORDFELD HÄNDEL GMBH"
    );
}

#[test]
fn a_carton_gets_an_sscc_with_its_check_digit() {
    assert_eq!(gs1::check_digit("04000010001"), Some(0));
    assert_eq!(
        gs1::sscc(0, "0400001", 1).as_deref(),
        Some("004000010000000014")
    );
    assert!(gs1::is_valid("004000010000000014"));
    assert!(!gs1::is_valid("004000010000000015"));
    // A serial that does not fit is refused, not cut.
    assert_eq!(gs1::sscc(0, "0400001000", 1_000_000), None);
    assert_eq!(
        gs1::sscc(0, "0400001000", 999_999).map(|s| s.len()),
        Some(18)
    );
    assert_eq!(gs1::sscc(10, "0400001", 1), None);
    assert_eq!(gs1::sscc(0, "04000", 1), None);
}

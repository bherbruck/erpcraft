//! EDIFACT messages on the typed messages X12 uses: an ORDERS read, and
//! ORDERS, ORDRSP, DESADV, INVOIC and CONTRL written and read back as
//! they were meant. The files are made up, with fictional parties.

use chrono::{NaiveDate, NaiveDateTime};
use erpcraft_edi::edifact::{
    self, Envelope, Outgoing, SyntaxId, contrl::Contrl, desadv, invoic, orders, ordrsp,
};
use erpcraft_edi::segment::{Delimiters, Segment};
use erpcraft_edi::x12::asn856::{Item, ItemLot, Node, Pack, PackKind, ShipNotice, ShipOrder};
use erpcraft_edi::x12::invoice810::{Charge, Invoice, InvoiceLine, Tax};
use erpcraft_edi::x12::po850::{DateOf, Line, Party, ProductId, PurchaseOrder, Reference};
use erpcraft_edi::x12::poack855::{AckLine, LineStatus, OrderAck};
use rust_decimal::Decimal;

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn at() -> NaiveDateTime {
    day(2026, 11, 20).and_hms_opt(9, 30, 0).unwrap()
}

/// A made-up ORDERS from a fictional German retailer: a UNA with a decimal
/// comma, a released '+' and apostrophe in a name, ISO 8859-1 letters.
fn orders_file() -> Vec<u8> {
    let text = "UNA:+,? '\
UNB+UNOC:3+4012345000016:14+4098765000013:14+261120:0930+REF4711'\
UNH+1+ORDERS:D:96A:UN:EAN008'\
BGM+220+PO-88001+9'\
DTM+137:20261120:102'\
DTM+2:20261127:102'\
RFF+CT:K-2026-17'\
NAD+BY+4012345000016::9++Lindenhof Märkte GmbH'\
NAD+SU+4098765000013::9++Brücke ?+ Söhne?'s Werkzeug'\
NAD+DP+4012345000092::9++Zentrallager Ost+Lagerweg 3+Erfurt++99084+DE'\
CUX+2:EUR:9'\
LIN+1++4098765100014:EN'\
PIA+1+HM-100:SA+77120:IN'\
IMD+F++:::Hammer, 500 g'\
QTY+21:48:PCE'\
PRI+AAA:12,5'\
LIN+2++4098765100021:EN'\
QTY+21:12,5:KGM'\
PRI+AAB:3,25'\
UNS+S'\
MOA+86:640,63'\
CNT+2:2'\
UNT+21+1'\
UNZ+1+REF4711'";
    // ISO 8859-1, as UNOC is.
    text.chars().map(|c| c as u32 as u8).collect()
}

fn the_message(bytes: &[u8]) -> (edifact::Message, Delimiters) {
    let mut interchanges = edifact::read(bytes).expect("reads");
    let interchange = interchanges.remove(0);
    assert!(interchange.errors.is_empty(), "{:?}", interchange.errors);
    let d = interchange.delimiters;
    (interchange.messages.into_iter().next().unwrap(), d)
}

#[test]
fn an_orders_reads_into_the_typed_order() {
    let bytes = orders_file();
    let interchanges = edifact::read(&bytes).unwrap();
    let interchange = &interchanges[0];
    assert_eq!(interchange.sender, "4012345000016");
    assert_eq!(interchange.sender_qualifier, "14");
    assert_eq!(interchange.syntax_id, "UNOC");
    let (message, d) = the_message(&bytes);
    assert!(message.errors.is_empty(), "{:?}", message.errors);
    let (order, verdict) = orders::read(&message, &d);
    assert!(verdict.is_clean(), "{verdict:?}");
    assert_eq!(order.number, "PO-88001");
    assert_eq!(order.purpose, "00");
    assert_eq!(order.date, Some(day(2026, 11, 20)));
    assert_eq!(
        order.dates,
        vec![DateOf {
            qualifier: "002".into(),
            date: day(2026, 11, 27)
        }]
    );
    assert_eq!(order.currency.as_deref(), Some("EUR"));
    let supplier = order.parties.iter().find(|p| p.role == "VN").unwrap();
    // Released characters and ISO 8859-1 letters, as they were meant.
    assert_eq!(supplier.name, "Brücke + Söhne's Werkzeug");
    let ship_to = order.parties.iter().find(|p| p.role == "ST").unwrap();
    assert_eq!(ship_to.id, "4012345000092");
    assert_eq!(ship_to.id_qualifier, "UL");
    assert_eq!(ship_to.city, "Erfurt");
    assert_eq!(order.lines.len(), 2);
    let hammer = &order.lines[0];
    assert_eq!(hammer.line_id, "1");
    assert_eq!(hammer.quantity, Some(dec("48")));
    assert_eq!(hammer.uom, "PCE");
    // A decimal comma.
    assert_eq!(hammer.unit_price, Some(dec("12.5")));
    assert_eq!(hammer.descriptions, vec!["Hammer, 500 g"]);
    assert!(hammer.product_ids.contains(&ProductId {
        qualifier: "EN".into(),
        value: "4098765100014".into()
    }));
    assert!(hammer.product_ids.contains(&ProductId {
        qualifier: "VN".into(),
        value: "HM-100".into()
    }));
    assert!(hammer.product_ids.contains(&ProductId {
        qualifier: "IN".into(),
        value: "77120".into()
    }));
    // A gross price where no net one is given.
    assert_eq!(order.lines[1].unit_price, Some(dec("3.25")));
    assert_eq!(order.lines[1].quantity, Some(dec("12.5")));
    assert_eq!(order.total, Some(dec("640.63")));
    assert_eq!(order.lines_stated, Some(2));
    assert!(order.unhandled.is_empty(), "{:?}", order.unhandled);
}

fn envelope(decimal: u8, syntax: SyntaxId) -> Envelope {
    Envelope {
        delimiters: Delimiters {
            decimal,
            ..Delimiters::EDIFACT
        },
        syntax,
        syntax_version: "3".into(),
        sender: "4098765000013".into(),
        sender_qualifier: "14".into(),
        receiver: "4012345000016".into(),
        receiver_qualifier: "14".into(),
        at: at(),
        control: "00000042".into(),
        test: false,
    }
}

fn outgoing(kind: &str, association: &str, body: Vec<Segment>) -> Outgoing {
    Outgoing {
        kind: kind.into(),
        version: "D".into(),
        release: "96A".into(),
        agency: "UN".into(),
        association: association.into(),
        reference: "1".into(),
        body,
    }
}

/// Written in an interchange, read back: the message and its delimiters.
fn through(envelope: &Envelope, message: Outgoing) -> (edifact::Message, Delimiters) {
    let bytes = edifact::write(envelope, &[message]).expect("writes");
    let back = edifact::read(&bytes).expect("reads back");
    assert!(back[0].una);
    assert_eq!(back[0].control, envelope.control);
    the_message(&bytes)
}

fn parties() -> Vec<Party> {
    vec![
        Party {
            role: "BY".into(),
            id_qualifier: "UL".into(),
            id: "4012345000016".into(),
            name: "Lindenhof Märkte GmbH".into(),
            ..Party::default()
        },
        Party {
            role: "ST".into(),
            id_qualifier: "UL".into(),
            id: "4012345000092".into(),
            name: "Zentrallager Ost".into(),
            address_lines: vec!["Lagerweg 3".into()],
            city: "Erfurt".into(),
            postal_code: "99084".into(),
            country: "DE".into(),
            ..Party::default()
        },
    ]
}

fn gtin(value: &str) -> ProductId {
    ProductId {
        qualifier: "EN".into(),
        value: value.into(),
    }
}

#[test]
fn an_orders_written_reads_back_as_meant() {
    let order = PurchaseOrder {
        purpose: "00".into(),
        number: "PO-1".into(),
        date: Some(day(2026, 11, 20)),
        currency: Some("EUR".into()),
        references: vec![Reference {
            qualifier: "CT".into(),
            value: "K-1".into(),
            description: String::new(),
        }],
        dates: vec![DateOf {
            qualifier: "002".into(),
            date: day(2026, 11, 27),
        }],
        parties: parties(),
        lines: vec![Line {
            line_id: "1".into(),
            quantity: Some(dec("48")),
            uom: "PCE".into(),
            unit_price: Some(dec("12.5")),
            product_ids: vec![gtin("4098765100014")],
            descriptions: vec!["Hammer: 500 g + Stiel".into()],
            ..Line::default()
        }],
        total: Some(dec("600")),
        ..PurchaseOrder::default()
    };
    for mark in [b'.', b','] {
        let e = envelope(mark, SyntaxId::Unoc);
        let (message, d) = through(
            &e,
            outgoing("ORDERS", "EAN008", orders::body(&order, &e.delimiters)),
        );
        let (read, verdict) = orders::read(&message, &d);
        assert!(verdict.is_clean());
        assert_eq!(read, orders::carried(&order));
    }
}

#[test]
fn an_ordrsp_reads_back_as_meant() {
    let ack = OrderAck {
        purpose: "00".into(),
        ack_type: "AC".into(),
        order_number: "PO-1".into(),
        order_date: Some(day(2026, 11, 20)),
        seller_order: "SO-2026-00017".into(),
        date: Some(day(2026, 11, 21)),
        currency: Some("EUR".into()),
        parties: parties(),
        lines: vec![
            AckLine {
                line_id: "1".into(),
                quantity: Some(dec("48")),
                uom: "PCE".into(),
                unit_price: Some(dec("12.5")),
                product_ids: vec![gtin("4098765100014")],
                statuses: vec![LineStatus {
                    code: "IA".into(),
                    quantity: Some(dec("48")),
                    uom: "PCE".into(),
                    ..LineStatus::default()
                }],
            },
            AckLine {
                line_id: "2".into(),
                quantity: Some(dec("12.5")),
                uom: "KGM".into(),
                unit_price: Some(dec("3.25")),
                product_ids: vec![],
                statuses: vec![LineStatus {
                    code: "IQ".into(),
                    quantity: Some(dec("10")),
                    uom: "KGM".into(),
                    date_qualifier: "011".into(),
                    date: Some(day(2026, 11, 25)),
                }],
            },
        ],
        ..OrderAck::default()
    };
    let e = envelope(b',', SyntaxId::Unoc);
    let (message, d) = through(
        &e,
        outgoing("ORDRSP", "", ordrsp::body(&ack, &e.delimiters)),
    );
    let (read, verdict) = ordrsp::read(&message, &d);
    assert!(verdict.is_clean());
    assert_eq!(read, ordrsp::carried(&ack));
}

fn notice(standard_carton: bool) -> ShipNotice {
    let item = |line: &str, packs: Vec<Pack>, lots: Vec<ItemLot>| Item {
        line_id: line.into(),
        product_ids: vec![gtin("4098765100014")],
        shipped: Some(dec("24")),
        uom: "PCE".into(),
        ordered: Some(dec("48")),
        ordered_uom: "PCE".into(),
        description: "Hammer".into(),
        packs,
        lots,
    };
    let carton = |sscc: &str, contents: Vec<Node>| Pack {
        kind: PackKind::Carton,
        sscc: Some(sscc.into()),
        contents,
    };
    let lot = |code: &str, expiry: Option<NaiveDate>, serial: bool| ItemLot {
        lot: code.into(),
        expiry,
        serial,
    };
    let contents = if standard_carton {
        vec![Node::Item(item(
            "1",
            vec![
                carton("340987650000000017", vec![]),
                carton("340987650000000024", vec![]),
            ],
            vec![lot("L-1", Some(day(2027, 6, 30)), false)],
        ))]
    } else {
        vec![Node::Pack(Pack {
            kind: PackKind::Pallet,
            sscc: Some("340987650000000031".into()),
            contents: vec![
                Node::Pack(carton(
                    "340987650000000017",
                    vec![Node::Item(item(
                        "1",
                        vec![],
                        vec![lot("SN-1", None, true), lot("SN-2", None, true)],
                    ))],
                )),
                Node::Pack(carton(
                    "340987650000000024",
                    vec![Node::Item(item("2", vec![], vec![]))],
                )),
            ],
        })]
    };
    ShipNotice {
        purpose: "00".into(),
        shipment_id: "DN-2026-00031".into(),
        date: Some(day(2026, 11, 24)),
        time: "1415".into(),
        references: vec![Reference {
            qualifier: "BM".into(),
            value: "BOL-778".into(),
            description: String::new(),
        }],
        dates: vec![DateOf {
            qualifier: "011".into(),
            date: day(2026, 11, 24),
        }],
        parties: parties(),
        orders: vec![ShipOrder {
            order_number: "PO-1".into(),
            order_date: Some(day(2026, 11, 20)),
            contents,
            ..ShipOrder::default()
        }],
        ..ShipNotice::default()
    }
}

#[test]
fn a_desadv_reads_back_as_meant_both_ways_of_packing() {
    for standard_carton in [false, true] {
        let meant = notice(standard_carton);
        let e = envelope(b'.', SyntaxId::Unoc);
        let body = desadv::body(&meant, &e.delimiters).expect("one order");
        assert!(body.iter().any(|s| s.tag == "GIN"));
        let (message, d) = through(&e, outgoing("DESADV", "EAN007", body));
        let (read, verdict) = desadv::read(&message, &d);
        assert!(verdict.is_clean());
        assert!(read.unhandled.is_empty(), "{:?}", read.unhandled);
        assert_eq!(
            read,
            desadv::carried(&meant),
            "standard carton: {standard_carton}"
        );
    }
    // Two orders in one notice: not a DESADV of this module.
    let mut two = notice(false);
    two.orders.push(two.orders[0].clone());
    assert!(desadv::body(&two, &Delimiters::EDIFACT).is_none());
}

#[test]
fn an_invoic_reads_back_as_meant_with_its_totals() {
    let invoice = Invoice {
        date: Some(day(2026, 11, 25)),
        number: "INV-2026-00044".into(),
        order_date: Some(day(2026, 11, 20)),
        order_number: "PO-1".into(),
        kind: "DI".into(),
        currency: Some("EUR".into()),
        parties: parties(),
        lines: vec![InvoiceLine {
            line_id: "1".into(),
            quantity: Some(dec("48")),
            uom: "PCE".into(),
            unit_price: Some(dec("12.5")),
            product_ids: vec![gtin("4098765100014")],
            description: "Hammer".into(),
            charges: vec![Charge {
                indicator: "A".into(),
                code: "DI".into(),
                amount: dec("30"),
                description: "Rabatt".into(),
            }],
        }],
        total: Some(dec("713.40")),
        taxes: vec![Tax {
            kind: "VA".into(),
            amount: dec("113.90"),
            percent: Some(dec("19")),
        }],
        charges: vec![Charge {
            indicator: "C".into(),
            code: "FC".into(),
            amount: dec("29.50"),
            description: "Fracht".into(),
        }],
        ..Invoice::default()
    };
    let e = envelope(b',', SyntaxId::Unoc);
    let (message, d) = through(
        &e,
        outgoing(
            "INVOIC",
            "EAN008",
            invoic::body(&invoice, &e.delimiters).unwrap(),
        ),
    );
    let (read, verdict) = invoic::read(&message, &d);
    assert!(verdict.is_clean());
    assert!(read.unhandled.is_empty(), "{:?}", read.unhandled);
    assert_eq!(read, invoic::carried(&invoice));
    assert_eq!(read.total, Some(dec("713.40")));
    // Three places: refused, never rounded.
    let mut odd = invoice.clone();
    odd.total = Some(dec("713.405"));
    assert!(invoic::body(&odd, &Delimiters::EDIFACT).is_none());
}

#[test]
fn a_contrl_answers_each_message_and_reads_back() {
    // An ORDERS whose UNT counts wrong: rejected with code 29.
    let text: String = orders_file().iter().map(|b| *b as char).collect();
    let text = text.replace("UNT+21+1", "UNT+20+1");
    let bytes: Vec<u8> = text.chars().map(|c| c as u32 as u8).collect();
    let interchange = edifact::read(&bytes).unwrap().remove(0);
    let contrl = Contrl::of(&interchange, &[Default::default()]);
    assert_eq!(contrl.action, "7");
    assert_eq!(contrl.messages[0].action, "4");
    assert_eq!(contrl.messages[0].code, Some(29));
    assert_eq!(contrl.interchange, "REF4711");
    let mut e = envelope(b'.', SyntaxId::Unoc);
    e.control = "00000043".into();
    let (message, _) = through(&e, outgoing("CONTRL", "", contrl.body()));
    assert_eq!(message.kind, "CONTRL");
    assert_eq!(Contrl::read(&message), Some(contrl));
}

#[test]
fn a_character_the_set_cannot_carry_is_refused() {
    let order = PurchaseOrder {
        number: "PO-€".into(),
        ..PurchaseOrder::default()
    };
    let unoc = envelope(b'.', SyntaxId::Unoc);
    assert!(
        edifact::write(
            &unoc,
            &[outgoing(
                "ORDERS",
                "",
                orders::body(&order, &unoc.delimiters)
            )]
        )
        .unwrap_err()
        .message
        .contains("UNOC")
    );
    // UTF-8 carries it.
    let unoy = envelope(b'.', SyntaxId::Unoy);
    let (message, d) = through(
        &unoy,
        outgoing("ORDERS", "", orders::body(&order, &unoy.delimiters)),
    );
    assert_eq!(orders::read(&message, &d).0.number, "PO-€");
    // UNOA has no small letters.
    let mut small = order.clone();
    small.number = "po-1".into();
    let unoa = envelope(b'.', SyntaxId::Unoa);
    assert!(
        edifact::write(
            &unoa,
            &[outgoing(
                "ORDERS",
                "",
                orders::body(&small, &unoa.delimiters)
            )]
        )
        .is_err()
    );
}

#[test]
fn any_byte_changed_is_read_or_refused_never_a_panic() {
    let bytes = orders_file();
    for at in 0..bytes.len() {
        for with in [b'+', b':', b'\'', b'?', b',', b'9', b'A', 0, 0xFF] {
            let mut changed = bytes.clone();
            changed[at] = with;
            if let Ok(interchanges) = edifact::read(&changed) {
                for interchange in interchanges {
                    let d = interchange.delimiters;
                    for message in &interchange.messages {
                        let _ = orders::read(message, &d);
                        let _ = desadv::read(message, &d);
                        let _ = invoic::read(message, &d);
                        let _ = ordrsp::read(message, &d);
                        let _ = Contrl::read(message);
                    }
                    let _ = Contrl::of(&interchange, &[]);
                }
            }
        }
    }
}

#[test]
fn a_remadv_an_ordchg_and_an_invrpt_read_back_as_written() {
    use erpcraft_edi::edifact::{invrpt, ordchg, remadv};
    use erpcraft_edi::x12::change860::{ChangeLine, OrderChange};
    use erpcraft_edi::x12::inventory846::{InventoryAdvice, InventoryItem};
    use erpcraft_edi::x12::remit820::{Adjustment, RemitItem, Remittance};
    let env = envelope(b',', SyntaxId::Unoc);
    let dec = |s: &str| s.parse::<Decimal>().unwrap();

    let remit = Remittance {
        handling: "I".into(),
        credit_debit: "C".into(),
        amount: Some(dec("347")),
        date: Some(day(2026, 11, 20)),
        trace: "PAY-5501".into(),
        items: vec![
            RemitItem {
                qualifier: "IV".into(),
                reference: "SI-1".into(),
                paid: Some(dec("119")),
                invoiced: Some(dec("119")),
                ..RemitItem::default()
            },
            RemitItem {
                qualifier: "IV".into(),
                reference: "SI-2".into(),
                paid: Some(dec("228")),
                invoiced: Some(dec("238")),
                discount: Some(dec("0.5")),
                adjustments: vec![Adjustment {
                    amount: Some(dec("-10")),
                    reason: "CB".into(),
                    reference: "CLAIM-12".into(),
                    ..Adjustment::default()
                }],
            },
        ],
        ..Remittance::default()
    };
    let (m, d) = through(
        &env,
        outgoing("REMADV", "", remadv::body(&remit, &env.delimiters)),
    );
    let (read, verdict) = remadv::read(&m, &d);
    assert!(verdict.is_clean());
    assert_eq!(read, remadv::carried(&remit));

    let change = OrderChange {
        purpose: "04".into(),
        order_number: "PO-88001".into(),
        change_number: "CH-1".into(),
        order_date: Some(day(2026, 11, 18)),
        lines: vec![ChangeLine {
            line_id: "2".into(),
            change: "CA".into(),
            quantity: Some(dec("15")),
            uom: "CT".into(),
            unit_price: Some(dec("31.2")),
            product_ids: vec![ProductId {
                qualifier: "EN".into(),
                value: "4098765100024".into(),
            }],
            dates: vec![],
        }],
        ..OrderChange::default()
    };
    let (m, d) = through(
        &env,
        outgoing("ORDCHG", "", ordchg::body(&change, &env.delimiters)),
    );
    let (read, verdict) = ordchg::read(&m, &d);
    assert!(verdict.is_clean());
    let mut expected = change.clone();
    expected.lines_stated = Some(1);
    assert_eq!(read, expected);

    let advice = InventoryAdvice {
        purpose: "00".into(),
        report_id: "R-1".into(),
        date: Some(day(2026, 11, 20)),
        items: vec![InventoryItem {
            product_ids: vec![ProductId {
                qualifier: "EN".into(),
                value: "4098765100017".into(),
            }],
            qualifier: "33".into(),
            quantity: Some(dec("500")),
            uom: "PCE".into(),
        }],
        ..InventoryAdvice::default()
    };
    let (m, d) = through(
        &env,
        outgoing("INVRPT", "", invrpt::body(&advice, &env.delimiters)),
    );
    let (read, verdict) = invrpt::read(&m, &d);
    assert!(verdict.is_clean());
    assert_eq!(read, invrpt::carried(&advice));
}

//! The messages phase 2 sends: the 855, the 856 and the 810. Each is
//! written, read back with its reader, and must say what was meant; an
//! edited count or total is caught; no change to a byte makes a reader
//! panic. The parties and numbers are made up.

use chrono::NaiveDate;
use erpcraft_edi::Delimiters;
use erpcraft_edi::segment::Segment;
use erpcraft_edi::x12::asn856::{self, Item, ItemLot, Node, Pack, PackKind, ShipNotice, ShipOrder};
use erpcraft_edi::x12::invoice810::{self, Charge, Invoice, InvoiceLine, Tax};
use erpcraft_edi::x12::po850::{Party, ProductId, Reference};
use erpcraft_edi::x12::poack855::{self, AckLine, LineStatus, OrderAck};
use erpcraft_edi::x12::{self, Envelope, TransactionSet};
use rust_decimal::Decimal;

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 20).unwrap()
}

fn envelope(functional_id: &str) -> Envelope {
    Envelope {
        sender_qualifier: "ZZ".into(),
        sender_id: "CEDARHOLLOW".into(),
        receiver_qualifier: "ZZ".into(),
        receiver_id: "NORTHFIELDMERC".into(),
        group_sender: "CEDARHOLLOW".into(),
        group_receiver: "NORTHFIELDMERC".into(),
        at: day().and_hms_opt(10, 0, 0).unwrap(),
        version: "00401".into(),
        group_version: "004010".into(),
        functional_id: functional_id.into(),
        interchange_control: 7,
        group_control: 7,
        usage: 'P',
        ack_requested: false,
        delimiters: Delimiters::X12,
    }
}

fn round_trip(set_id: &str, functional_id: &str, body: Vec<Segment>) -> (String, TransactionSet) {
    let text = x12::write(&envelope(functional_id), set_id, &[body]).unwrap();
    let mut read = x12::read(text.as_bytes()).unwrap();
    let set = read.remove(0).groups.remove(0).sets.remove(0);
    assert!(set.errors.is_empty(), "{text}");
    (text, set)
}

fn ids() -> Vec<ProductId> {
    vec![
        ProductId {
            qualifier: "UK".into(),
            value: "10400001000202".into(),
        },
        ProductId {
            qualifier: "VN".into(),
            value: "CH-2040-C".into(),
        },
    ]
}

fn ack() -> OrderAck {
    OrderAck {
        purpose: "00".into(),
        ack_type: poack855::CHANGED.into(),
        order_number: "4500018842".into(),
        order_date: Some(day()),
        seller_order: "SO-1".into(),
        date: Some(day()),
        lines: vec![AckLine {
            line_id: "2".into(),
            quantity: Some(dec("120")),
            uom: "CA".into(),
            unit_price: Some(dec("31.2")),
            product_ids: ids(),
            statuses: vec![LineStatus {
                code: poack855::LINE_QUANTITY_CHANGED.into(),
                quantity: Some(dec("100")),
                uom: "CA".into(),
                ..LineStatus::default()
            }],
        }],
        ..OrderAck::default()
    }
}

fn notice(standard_carton: bool) -> ShipNotice {
    let item = |packs: Vec<Pack>| Item {
        line_id: "2".into(),
        product_ids: ids(),
        shipped: Some(dec("1")),
        uom: "CA".into(),
        ordered: Some(dec("120")),
        ordered_uom: "CA".into(),
        description: String::new(),
        packs,
        // One lot with its expiry date in the LIN, or two as REF segments.
        lots: if standard_carton {
            vec![
                ItemLot {
                    lot: "L-77".into(),
                    expiry: None,
                    serial: false,
                },
                ItemLot {
                    lot: "SN-78".into(),
                    expiry: None,
                    serial: true,
                },
            ]
        } else {
            vec![ItemLot {
                lot: "L-77".into(),
                expiry: NaiveDate::from_ymd_opt(2026, 12, 31),
                serial: false,
            }]
        },
    };
    let carton = |sscc: &str, contents: Vec<Node>| Pack {
        kind: PackKind::Carton,
        sscc: Some(sscc.into()),
        contents,
    };
    let contents = if standard_carton {
        vec![Node::Item(item(vec![
            carton("000400001000000017", vec![]),
            carton("000400001000000024", vec![]),
        ]))]
    } else {
        vec![Node::Pack(Pack {
            kind: PackKind::Pallet,
            sscc: Some("000400001000000031".into()),
            contents: vec![
                Node::Pack(carton("000400001000000017", vec![Node::Item(item(vec![]))])),
                Node::Pack(carton("000400001000000024", vec![Node::Item(item(vec![]))])),
            ],
        })]
    };
    ShipNotice {
        purpose: "00".into(),
        shipment_id: "DN-1".into(),
        date: Some(day()),
        time: "1000".into(),
        structure: if standard_carton { "0002" } else { "0001" }.into(),
        references: vec![Reference {
            qualifier: "BM".into(),
            value: "BOL123".into(),
            description: String::new(),
        }],
        parties: vec![Party {
            role: "ST".into(),
            name: "NORTHFIELD DC 07".into(),
            id_qualifier: "92".into(),
            id: "0007".into(),
            ..Party::default()
        }],
        orders: vec![ShipOrder {
            order_number: "4500018842".into(),
            order_date: Some(day()),
            contents,
            ..ShipOrder::default()
        }],
        ..ShipNotice::default()
    }
}

fn invoice() -> Invoice {
    Invoice {
        date: Some(day()),
        number: "SI-1".into(),
        order_date: Some(day()),
        order_number: "4500018842".into(),
        kind: invoice810::DEBIT.into(),
        lines: vec![InvoiceLine {
            line_id: "2".into(),
            quantity: Some(dec("120")),
            uom: "CA".into(),
            unit_price: Some(dec("31.2")),
            product_ids: ids(),
            description: String::new(),
            charges: vec![Charge {
                indicator: "A".into(),
                code: "ZZZZ".into(),
                amount: dec("44"),
                description: "Discount".into(),
            }],
        }],
        total: Some(dec("4161.8")),
        taxes: vec![
            Tax {
                kind: "ST".into(),
                amount: dec("185"),
                percent: Some(dec("5")),
            },
            Tax {
                kind: "ST".into(),
                amount: dec("259"),
                percent: Some(dec("7")),
            },
        ],
        charges: vec![Charge {
            indicator: "C".into(),
            code: "ZZZZ".into(),
            amount: dec("17.8"),
            description: "Freight".into(),
        }],
        ..Invoice::default()
    }
}

#[test]
fn an_855_reads_back_as_written() {
    let meant = ack();
    let (text, set) = round_trip("855", "PR", meant.body());
    assert!(
        text.contains("BAK*00*AC*4500018842*20261120****SO-1*20261120~"),
        "{text}"
    );
    assert!(
        text.contains("PO1*2*120*CA*31.2**UK*10400001000202*VN*CH-2040-C~ACK*IQ*100*CA~CTT*1~"),
        "{text}"
    );
    let (read, verdict) = poack855::read(&set);
    assert!(verdict.segments.is_empty() && verdict.codes.is_empty());
    let mut expected = meant;
    expected.lines_stated = Some(1);
    assert_eq!(read, expected);

    // A count edited in transit is seen.
    let edited = text.replace("CTT*1~", "CTT*2~");
    let set = &x12::read(edited.as_bytes()).unwrap()[0].groups[0].sets[0];
    assert!(!poack855::read(set).0.notes.is_empty());
}

#[test]
fn an_856_tree_reads_back_both_ways_round() {
    for standard_carton in [false, true] {
        let meant = notice(standard_carton);
        let (text, set) = round_trip("856", "SH", meant.body());
        let (read, verdict) = asn856::read(&set);
        assert!(verdict.segments.is_empty(), "{text}: {verdict:?}");
        let mut expected = meant.clone();
        expected.levels_stated = Some(meant.levels() as u32);
        assert_eq!(read, expected, "{text}");
        assert_eq!(
            asn856::items(&read).len(),
            if standard_carton { 1 } else { 2 }
        );
    }
    let (text, _) = round_trip("856", "SH", notice(false).body());
    // Shipment 1, order 2 under it, pallet 3 under the order, cartons and
    // items under it, each numbered in order.
    assert!(text.contains("HL*1**S*1~"), "{text}");
    assert!(text.contains("HL*2*1*O*1~PRF*4500018842***20261120~HL*3*2*T*1~MAN*GM*000400001000000031~HL*4*3*P*1~MAN*GM*000400001000000017~HL*5*4*I*0~LIN*2*UK*10400001000202*VN*CH-2040-C*LT*L-77~SN1**1*CA**120*CA~DTM*036*20261231~"), "{text}");
    assert!(text.contains("CTT*7~"), "{text}");
}

#[test]
fn a_guides_hierarchy_rearranges_the_tree_and_it_reads_back() {
    use erpcraft_edi::guide::{self, Hierarchy, Options};
    use erpcraft_edi::outbound::Typed;
    for (hierarchy, levels, structure, pallets, packs) in [
        (Hierarchy::Sotpi, "HL*3*2*T*1~", "0001", 1, 2),
        (Hierarchy::Sopi, "HL*3*2*P*1~", "0001", 0, 2),
        (Hierarchy::Soip, "HL*3*2*I*1~", "0002", 0, 2),
        (Hierarchy::Soi, "HL*3*2*I*0~", "0004", 0, 0),
    ] {
        let mut typed = Typed::Notice(notice(false));
        guide::apply_options(
            &Options {
                hierarchy: Some(hierarchy),
                ..Options::default()
            },
            &mut typed,
        );
        let Typed::Notice(mut meant) = typed else {
            unreachable!()
        };
        assert_eq!(meant.structure, structure);
        let (text, set) = round_trip("856", "SH", meant.body());
        assert!(text.contains(levels), "{hierarchy:?}: {text}");
        let level = |code: &str| {
            text.matches(&format!("*{code}*1~")).count()
                + text.matches(&format!("*{code}*0~")).count()
        };
        assert_eq!(level("T"), pallets, "{text}");
        assert_eq!(level("P"), packs, "{text}");
        // Two cartons of one line: one item of both, when items lead.
        if matches!(hierarchy, Hierarchy::Soip | Hierarchy::Soi) {
            assert_eq!(text.matches("SN1**2*CA**120*CA~").count(), 1, "{text}");
        }
        let (read, verdict) = asn856::read(&set);
        assert!(verdict.segments.is_empty(), "{hierarchy:?}: {verdict:?}");
        meant.levels_stated = Some(meant.levels() as u32);
        assert_eq!(read, meant, "{hierarchy:?}");
    }
}

#[test]
fn an_856_parent_that_comes_later_or_nests_too_deep_is_refused() {
    let (text, _) = round_trip("856", "SH", notice(false).body());
    let later = text.replace("HL*3*2*T*1~", "HL*3*9*T*1~");
    let set = &x12::read(later.as_bytes()).unwrap()[0].groups[0].sets[0];
    let (_, verdict) = asn856::read(set);
    assert!(!verdict.segments.is_empty());

    // Cartons in items in cartons, far deeper than any notice: refused,
    // never followed down.
    let mut deep = vec![
        Segment::new("BSN", &["00", "X", "20261120", "1000", "0001"]),
        Segment::new("HL", &["1", "", "S", "1"]),
        Segment::new("HL", &["2", "1", "O", "1"]),
    ];
    for n in 3..2000 {
        let code = if n % 2 == 1 { "P" } else { "I" };
        deep.push(Segment::new(
            "HL",
            &[n.to_string(), (n - 1).to_string(), code.into(), "1".into()],
        ));
    }
    let (_, set) = round_trip("856", "SH", deep);
    let (_, verdict) = asn856::read(&set);
    assert!(!verdict.segments.is_empty());
}

#[test]
fn an_810_reads_back_and_its_total_adds_up() {
    let meant = invoice();
    assert_eq!(invoice810::computed_total(&meant), meant.total);
    let (text, set) = round_trip("810", "IN", meant.body().unwrap());
    assert!(text.contains("TDS*416180~"), "{text}");
    assert!(text.contains("TXI*ST*185*5~TXI*ST*259*7~"), "{text}");
    let (read, verdict) = invoice810::read(&set);
    assert!(verdict.segments.is_empty());
    let mut expected = meant.clone();
    expected.lines_stated = Some(1);
    assert_eq!(read, expected);

    // A total edited in transit no longer adds up.
    let edited = text.replace("TDS*416180~", "TDS*416190~");
    let set = &x12::read(edited.as_bytes()).unwrap()[0].groups[0].sets[0];
    let (read, _) = invoice810::read(set);
    assert_ne!(invoice810::computed_total(&read), read.total);

    // An amount of more than two places is never rounded to be written.
    let mut odd = meant;
    odd.charges[0].amount = dec("17.805");
    assert!(odd.body().is_none());
}

#[test]
fn no_change_to_a_message_makes_reading_it_panic() {
    let texts = [
        round_trip("855", "PR", ack().body()).0,
        round_trip("856", "SH", notice(false).body()).0,
        round_trip("856", "SH", notice(true).body()).0,
        round_trip("810", "IN", invoice().body().unwrap()).0,
    ];
    for text in texts {
        let bytes = text.as_bytes();
        for at in 0..bytes.len() {
            for with in [b'*', b'~', b'>', b'9', b'-', b'.', b'H', 0, 0xFF] {
                let mut changed = bytes.to_vec();
                changed[at] = with;
                let Ok(interchanges) = x12::read(&changed) else {
                    continue;
                };
                for set in interchanges
                    .iter()
                    .flat_map(|i| &i.groups)
                    .flat_map(|g| &g.sets)
                {
                    let _ = poack855::read(set);
                    let (notice, _) = asn856::read(set);
                    let _ = asn856::items(&notice);
                    let _ = notice.body();
                    let (invoice, _) = invoice810::read(set);
                    let _ = invoice810::computed_total(&invoice);
                }
            }
        }
    }
}

fn change() -> erpcraft_edi::x12::change860::OrderChange {
    use erpcraft_edi::x12::change860::{ChangeLine, OrderChange};
    OrderChange {
        purpose: "04".into(),
        order_type: "SA".into(),
        order_number: "4500018842".into(),
        change_number: "2".into(),
        order_date: Some(day()),
        lines: vec![
            ChangeLine {
                line_id: "2".into(),
                change: "QD".into(),
                quantity: Some(dec("100")),
                uom: "CA".into(),
                unit_price: Some(dec("31.2")),
                product_ids: ids(),
                dates: vec![erpcraft_edi::x12::po850::DateOf {
                    qualifier: "002".into(),
                    date: day(),
                }],
            },
            ChangeLine {
                line_id: "3".into(),
                change: "DI".into(),
                ..ChangeLine::default()
            },
        ],
        ..OrderChange::default()
    }
}

#[test]
fn an_860_and_an_846_read_back_as_written() {
    use erpcraft_edi::x12::change860;
    use erpcraft_edi::x12::inventory846::{self, InventoryAdvice, InventoryItem};
    let meant = change();
    let (_, set) = round_trip("860", "PC", meant.body());
    let (read, verdict) = change860::read(&set);
    assert_eq!(verdict, Default::default());
    let mut expected = meant.clone();
    expected.lines_stated = Some(2);
    assert_eq!(read, expected);

    let advice = InventoryAdvice {
        purpose: "00".into(),
        report_type: "SI".into(),
        report_id: "20261120".into(),
        date: Some(day()),
        items: vec![InventoryItem {
            product_ids: ids(),
            qualifier: "33".into(),
            quantity: Some(dec("713")),
            uom: "CA".into(),
        }],
        ..InventoryAdvice::default()
    };
    let (text, set) = round_trip("846", "IB", advice.body());
    assert!(
        text.contains("LIN**UK*10400001000202*VN*CH-2040-C"),
        "{text}"
    );
    let (read, verdict) = inventory846::read(&set);
    assert_eq!(verdict, Default::default());
    let mut expected = advice;
    expected.items_stated = Some(1);
    assert_eq!(read, expected);
}

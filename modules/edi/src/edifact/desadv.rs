//! DESADV (D.96A): the despatch advice, written from the same
//! `ShipNotice` the 856 is written from, and read back to check it.
//!
//! In this module's words: `BGM` 351 with the shipment's number; `DTM`
//! 137 when it was made (with the time) and the shipment's dates (11
//! shipped, 17 expected delivery); `RFF` the bill of lading and other
//! references; `RFF` `ON` the buyer's order with its date (`DTM` 171);
//! `NAD` the parties; `TDT` 20 the carrier (its code and name); `EQD` the
//! trailer; `MEA` the gross weight. One order per message.
//!
//! The packing is a tree of `CPS` levels, each naming its parent; the
//! shipment is level 1. A carton or pallet is a level with `PAC` (1, and
//! `CT` or `PX`), `PCI` 33E (marked with an SSCC) and `GIN` `BJ` (the
//! SSCC); the lines in it follow its `PCI`. A line shipped in cartons of
//! its own (standard carton) is a level without `PAC`, holding the one
//! line, whose cartons are the levels below it. A line is `LIN` (the
//! order's line number, a GTIN), `PIA` (other numbers, and its lots: a lot
//! as `NB`, a serial number as `SN`), `IMD`, `QTY` 12 shipped and 21
//! ordered, and `DTM` 36 the expiry of its one lot. Then `CNT` 2, the
//! lines counted.

use crate::edifact::Message;
use crate::edifact::common::{
    date_of, date_out, dtm, imd, lin_product, nad, num, pia_products, product_segments, qty,
    read_dtm, read_imd, read_nad, read_num, read_rff, rff, seg,
};
use crate::edifact::contrl::{MISSING, NOT_SUPPORTED, Verdict};
use crate::segment::{Delimiters, Segment};
use crate::x12::asn856::{
    Carrier, Equipment, Item, ItemLot, Lading, Node, Pack, PackKind, ShipNotice, ShipOrder,
};
use crate::x12::po850::Reference;

/// Levels of packing in one message, at most.
const MAX_LEVELS: usize = 100_000;

/// Writes a ship notice. `None` when it holds more than one order, which a
/// DESADV of this module does not carry.
pub fn body(notice: &ShipNotice, d: &Delimiters) -> Option<Vec<Segment>> {
    let mark = d.decimal;
    let order = match notice.orders.as_slice() {
        [one] => one,
        _ => return None,
    };
    let mut out = vec![seg(
        "BGM",
        vec![
            comps!["351"],
            comps![notice.shipment_id.clone()],
            comps!["9"],
        ],
    )];
    if let Some(date) = notice.date {
        if notice.time.is_empty() {
            out.push(dtm("137", date));
        } else {
            out.push(seg(
                "DTM",
                vec![comps![
                    "137",
                    format!("{}{}", date.format("%Y%m%d"), notice.time),
                    "203"
                ]],
            ));
        }
    }
    for date in &notice.dates {
        out.push(dtm(&date_out(&date.qualifier), date.date));
    }
    out.extend(notice.references.iter().map(rff));
    out.push(rff(&Reference {
        qualifier: "ON".into(),
        value: order.order_number.clone(),
        description: String::new(),
    }));
    if let Some(date) = order.order_date {
        out.push(dtm("171", date));
    }
    out.extend(notice.parties.iter().map(nad));
    if let Some(carrier) = &notice.carrier {
        out.push(seg(
            "TDT",
            vec![
                comps!["20"],
                vec![],
                vec![],
                vec![],
                comps![carrier.id.clone(), "", "", carrier.routing.clone()],
            ],
        ));
    }
    if let Some(equipment) = &notice.equipment {
        out.push(seg(
            "EQD",
            vec![
                comps![equipment.kind.clone()],
                comps![equipment.number.clone()],
            ],
        ));
    }
    if let Some(lading) = &notice.lading
        && let Some(weight) = lading.weight
    {
        out.push(seg(
            "MEA",
            vec![
                comps!["PD"],
                comps!["AAB"],
                comps![lading.weight_uom.clone(), num(weight, mark)],
            ],
        ));
    }
    out.push(seg("CPS", vec![comps!["1"]]));
    let mut writer = Writer {
        out,
        next: 1,
        lines: 0,
        mark,
    };
    for node in &order.contents {
        writer.node(1, node);
    }
    let lines = writer.lines;
    let mut out = writer.out;
    out.push(seg("CNT", vec![comps!["2", lines.to_string()]]));
    Some(out)
}

struct Writer {
    out: Vec<Segment>,
    next: usize,
    lines: usize,
    mark: u8,
}

impl Writer {
    fn level(&mut self, parent: usize) -> usize {
        self.next += 1;
        self.out.push(seg(
            "CPS",
            vec![comps![self.next.to_string()], comps![parent.to_string()]],
        ));
        self.next
    }

    fn pack_marks(&mut self, pack: &Pack) {
        let kind = match pack.kind {
            PackKind::Pallet => "PX",
            PackKind::Carton => "CT",
        };
        self.out
            .push(seg("PAC", vec![comps!["1"], vec![], comps![kind]]));
        if let Some(sscc) = &pack.sscc {
            self.out.push(seg("PCI", vec![comps!["33E"]]));
            self.out
                .push(seg("GIN", vec![comps!["BJ"], comps![sscc.clone()]]));
        }
    }

    fn node(&mut self, parent: usize, node: &Node) {
        match node {
            Node::Pack(pack) => {
                let id = self.level(parent);
                self.pack_marks(pack);
                // Items first, then the packs inside, each a level below.
                for inner in &pack.contents {
                    if let Node::Item(item) = inner {
                        self.item(item);
                    }
                }
                for inner in &pack.contents {
                    if let Node::Pack(_) = inner {
                        self.node(id, inner);
                    }
                }
            }
            Node::Item(item) => {
                let id = self.level(parent);
                self.item(item);
                for pack in &item.packs {
                    self.level(id);
                    self.pack_marks(pack);
                }
            }
        }
    }

    fn item(&mut self, item: &Item) {
        self.lines += 1;
        let (lin, mut pia) = product_segments(&item.product_ids);
        self.out
            .push(seg("LIN", vec![comps![item.line_id.clone()], vec![], lin]));
        for lot in &item.lots {
            pia.push(seg(
                "PIA",
                vec![
                    comps!["1"],
                    comps![lot.lot.clone(), if lot.serial { "SN" } else { "NB" }],
                ],
            ));
        }
        self.out.extend(pia);
        if !item.description.is_empty() {
            self.out.push(imd(&item.description));
        }
        if let Some(q) = item.shipped {
            self.out.push(qty("12", q, &item.uom, self.mark));
        }
        if let Some(q) = item.ordered {
            self.out.push(qty("21", q, &item.ordered_uom, self.mark));
        }
        if let [one] = item.lots.as_slice()
            && let Some(expiry) = one.expiry
        {
            self.out.push(dtm("36", expiry));
        }
    }
}

/// One `CPS` level as read: its parent, what packs it is, its lines.
#[derive(Default)]
struct Level {
    id: String,
    parent: String,
    pack: Option<Pack>,
    items: Vec<Item>,
}

pub fn read(message: &Message, d: &Delimiters) -> (ShipNotice, Verdict) {
    let mut notice = ShipNotice {
        purpose: "00".into(),
        ..ShipNotice::default()
    };
    let mut verdict = Verdict::default();
    if message.kind != "DESADV" {
        verdict.codes.push(NOT_SUPPORTED);
        return (notice, verdict);
    }
    let mut order = ShipOrder::default();
    let mut seen_bgm = false;
    let mut after_order_ref = false;
    let mut levels: Vec<Level> = vec![];
    let last = message.segments.len().saturating_sub(1);
    for (position, segment) in message.segments.iter().enumerate() {
        if position == 0 || position == last {
            continue;
        }
        let tag = segment.tag.as_str();
        match tag {
            "BGM" => {
                seen_bgm = true;
                notice.shipment_id = segment.get(2).to_owned();
            }
            "CPS" => {
                if levels.len() >= MAX_LEVELS {
                    verdict.codes.push(NOT_SUPPORTED);
                    break;
                }
                levels.push(Level {
                    id: segment.get(1).to_owned(),
                    parent: segment.get(2).to_owned(),
                    ..Level::default()
                });
            }
            "DTM" if levels.is_empty() => match read_dtm(segment) {
                Some((q, date, time)) if q == "137" => {
                    notice.date = Some(date);
                    notice.time = time;
                }
                Some((q, date, _)) if q == "171" && after_order_ref => {
                    order.order_date = Some(date)
                }
                Some(_) => notice.dates.extend(date_of(segment)),
                None => notice.unhandled.push((position, tag.into())),
            },
            "RFF" if levels.is_empty() => {
                let reference = read_rff(segment);
                if reference.qualifier == "ON" && order.order_number.is_empty() {
                    order.order_number = reference.value;
                    after_order_ref = true;
                } else {
                    after_order_ref = false;
                    notice.references.push(reference);
                }
            }
            "NAD" if levels.is_empty() => notice.parties.push(read_nad(segment)),
            "TDT" => {
                notice.carrier = Some(Carrier {
                    id: segment.component(5, 1).to_owned(),
                    routing: segment.component(5, 4).to_owned(),
                    ..Carrier::default()
                })
            }
            "EQD" => {
                notice.equipment = Some(Equipment {
                    kind: segment.get(1).to_owned(),
                    number: segment.get(2).to_owned(),
                    ..Equipment::default()
                })
            }
            "MEA" if segment.get(2) == "AAB" => {
                notice.lading = Some(Lading {
                    weight: read_num(segment.component(3, 2), d),
                    weight_uom: segment.component(3, 1).to_owned(),
                    weight_qualifier: "G".into(),
                    ..Lading::default()
                })
            }
            "PAC" => match levels.last_mut() {
                Some(level) => {
                    level.pack = Some(Pack {
                        kind: if segment.component(3, 1) == "PX" {
                            PackKind::Pallet
                        } else {
                            PackKind::Carton
                        },
                        sscc: None,
                        contents: vec![],
                    })
                }
                None => notice.unhandled.push((position, tag.into())),
            },
            "PCI" => {}
            "GIN" if segment.get(1) == "BJ" => {
                match levels.last_mut().and_then(|l| l.pack.as_mut()) {
                    Some(pack) => pack.sscc = Some(segment.get(2).to_owned()),
                    None => notice.unhandled.push((position, tag.into())),
                }
            }
            "LIN" => match levels.last_mut() {
                Some(level) => {
                    let mut item = Item {
                        line_id: segment.get(1).to_owned(),
                        ..Item::default()
                    };
                    item.product_ids.extend(lin_product(segment));
                    level.items.push(item);
                }
                None => notice.unhandled.push((position, tag.into())),
            },
            "PIA" => match levels.last_mut().and_then(|l| l.items.last_mut()) {
                Some(item) => {
                    for id in pia_products(segment) {
                        match id.qualifier.as_str() {
                            "NB" | "SN" => item.lots.push(ItemLot {
                                serial: id.qualifier == "SN",
                                lot: id.value,
                                expiry: None,
                            }),
                            _ => item.product_ids.push(id),
                        }
                    }
                }
                None => notice.unhandled.push((position, tag.into())),
            },
            "IMD" => match levels.last_mut().and_then(|l| l.items.last_mut()) {
                Some(item) => item.description = read_imd(segment),
                None => notice.unhandled.push((position, tag.into())),
            },
            "QTY" => match levels.last_mut().and_then(|l| l.items.last_mut()) {
                Some(item) => {
                    let q = read_num(segment.component(1, 2), d);
                    let uom = segment.component(1, 3).to_owned();
                    match segment.component(1, 1) {
                        "12" => {
                            item.shipped = q;
                            item.uom = uom;
                        }
                        "21" => {
                            item.ordered = q;
                            item.ordered_uom = uom;
                        }
                        _ => notice.unhandled.push((position, tag.into())),
                    }
                }
                None => notice.unhandled.push((position, tag.into())),
            },
            "DTM" => match (
                levels.last_mut().and_then(|l| l.items.last_mut()),
                read_dtm(segment),
            ) {
                (Some(item), Some((q, date, _))) if q == "36" => match item.lots.as_mut_slice() {
                    [one] => one.expiry = Some(date),
                    _ => notice.unhandled.push((position, tag.into())),
                },
                _ => notice.unhandled.push((position, tag.into())),
            },
            "CNT" => {}
            _ => notice.unhandled.push((position, tag.into())),
        }
    }
    if !seen_bgm {
        verdict.codes.push(MISSING);
    }
    order.contents = tree(levels, &mut notice);
    notice.orders.push(order);
    (notice, verdict)
}

/// The levels read, as the tree of packs and lines they say.
fn tree(levels: Vec<Level>, notice: &mut ShipNotice) -> Vec<Node> {
    fn build(id: &str, levels: &[Level], depth: usize, notice: &mut ShipNotice) -> Vec<Node> {
        if depth > 8 {
            notice
                .notes
                .push("The packing nests deeper than eight levels.".into());
            return vec![];
        }
        let mut out = vec![];
        for level in levels.iter().filter(|l| l.parent == id && l.id != id) {
            match &level.pack {
                Some(pack) => {
                    let mut pack = pack.clone();
                    pack.contents = level.items.iter().cloned().map(Node::Item).collect();
                    pack.contents
                        .extend(build(&level.id, levels, depth + 1, notice));
                    out.push(Node::Pack(pack));
                }
                None => {
                    let children = build(&level.id, levels, depth + 1, notice);
                    match level.items.as_slice() {
                        [one] => {
                            let mut item = one.clone();
                            item.packs = children
                                .into_iter()
                                .filter_map(|n| match n {
                                    Node::Pack(p) => Some(p),
                                    Node::Item(_) => None,
                                })
                                .collect();
                            out.push(Node::Item(item));
                        }
                        items => {
                            out.extend(items.iter().cloned().map(Node::Item));
                            out.extend(children);
                        }
                    }
                }
            }
        }
        out
    }
    build("1", &levels, 0, notice)
}

/// What of a ship notice a DESADV carries, as it reads back.
pub fn carried(notice: &ShipNotice) -> ShipNotice {
    fn node(n: &Node) -> Node {
        match n {
            Node::Pack(p) => Node::Pack(pack(p)),
            Node::Item(i) => {
                let mut i = i.clone();
                i.packs = i.packs.iter().map(pack).collect();
                Node::Item(i)
            }
        }
    }
    fn pack(p: &Pack) -> Pack {
        // Lines first, then the packs inside: the order they are written in.
        let mut contents: Vec<Node> = p
            .contents
            .iter()
            .filter(|n| matches!(n, Node::Item(_)))
            .map(node)
            .collect();
        contents.extend(
            p.contents
                .iter()
                .filter(|n| matches!(n, Node::Pack(_)))
                .map(node),
        );
        Pack {
            kind: p.kind,
            sscc: p.sscc.clone(),
            contents,
        }
    }
    let mut out = notice.clone();
    out.purpose = "00".into();
    out.structure.clear();
    out.levels_stated = None;
    out.unhandled.clear();
    out.notes.clear();
    out.carrier = notice.carrier.as_ref().map(|c| Carrier {
        id: c.id.clone(),
        routing: c.routing.clone(),
        ..Carrier::default()
    });
    out.equipment = notice.equipment.as_ref().map(|e| Equipment {
        kind: e.kind.clone(),
        number: e.number.clone(),
        ..Equipment::default()
    });
    out.lading = notice
        .lading
        .as_ref()
        .filter(|l| l.weight.is_some())
        .map(|l| Lading {
            weight: l.weight,
            weight_uom: l.weight_uom.clone(),
            weight_qualifier: "G".into(),
            ..Lading::default()
        });
    for reference in &mut out.references {
        reference.description.clear();
    }
    for party in &mut out.parties {
        party.address_lines.truncate(4);
    }
    for order in &mut out.orders {
        order.release.clear();
        order.references.clear();
        order.parties.clear();
        order.contents = order.contents.iter().map(node).collect();
    }
    out
}

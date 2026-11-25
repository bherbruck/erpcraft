//! The 856 ship notice: what left, for which orders, in which cartons on
//! which pallets, by which carrier. Its body is a tree of HL segments,
//! each naming its parent; here it is a typed tree, and the HL numbers
//! are the writer's business.
//!
//! The levels this module writes and reads (HL03): `S` shipment, `O`
//! order, `T` tare (a pallet), `P` pack (a carton), `I` item. The tree is
//! shipment, orders, and under each order either packs holding items
//! ("pick and pack") or items holding packs ("standard carton"), with
//! pallets above packs if any. BSN05 says which; the code is the caller's
//! (a partner's guide says which code it wants for which layout).

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::segment::Segment;
use crate::x12::TransactionSet;
use crate::x12::ack::{
    ELEMENT_INVALID_CHARACTER, SEGMENT_MISSING, SEGMENT_UNEXPECTED, SET_NOT_SUPPORTED,
    SegmentError, SetVerdict,
};
use crate::x12::fields::{
    Fields, date, date_segment, opt_number, party_segments, product_elements, read_party,
    read_product_ids, reference_segment, segment,
};
use crate::x12::po850::{DateOf, Party, ProductId, Reference};

/// Levels below the shipment, at most: order, pallet, carton, item and a
/// carton under an item. A file that nests deeper is refused.
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ShipNotice {
    /// BSN01: `00` original.
    pub purpose: String,
    /// BSN02: the shipper's number for the shipment.
    pub shipment_id: String,
    /// BSN03 and BSN04.
    pub date: Option<NaiveDate>,
    pub time: String,
    /// BSN05: the hierarchy structure code.
    pub structure: String,
    pub lading: Option<Lading>,
    pub carrier: Option<Carrier>,
    pub equipment: Option<Equipment>,
    /// Such as `BM` the bill of lading, `CN` the carrier's PRO number.
    pub references: Vec<Reference>,
    /// Such as `011` shipped, `017` expected delivery.
    pub dates: Vec<DateOf>,
    /// Such as `SF` ship from, `ST` ship to.
    pub parties: Vec<Party>,
    pub orders: Vec<ShipOrder>,
    /// CTT01: the number of HL segments the sender counted.
    pub levels_stated: Option<u32>,
    pub unhandled: Vec<(usize, String)>,
    pub notes: Vec<String>,
}

/// TD1: what the shipment is packed in, how many, and its weight.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Lading {
    /// TD101, such as `CTN25` cartons, `PLT94` pallets.
    pub packaging: String,
    /// TD102.
    pub quantity: Option<Decimal>,
    /// TD106: `G` gross.
    pub weight_qualifier: String,
    pub weight: Option<Decimal>,
    /// TD108, such as `LB` or `KG`.
    pub weight_uom: String,
}

/// TD5: who carries it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Carrier {
    /// TD502: `2` for a SCAC.
    pub id_qualifier: String,
    /// TD503: the carrier's code.
    pub id: String,
    /// TD504: how, such as `M` motor.
    pub method: String,
    /// TD505: the carrier's name or the routing.
    pub routing: String,
    /// TD512: the service level.
    pub service: String,
}

/// TD3: the trailer or container.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Equipment {
    /// TD301, such as `TL` trailer.
    pub kind: String,
    pub initial: String,
    pub number: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ShipOrder {
    /// PRF01: the buyer's order number, as received.
    pub order_number: String,
    /// PRF02.
    pub release: String,
    /// PRF04: the order's date, as received.
    pub order_date: Option<NaiveDate>,
    pub references: Vec<Reference>,
    pub parties: Vec<Party>,
    pub contents: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Node {
    Pack(Pack),
    Item(Item),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PackKind {
    /// HL03 `T`.
    Pallet,
    /// HL03 `P`.
    Carton,
}

impl PackKind {
    fn code(self) -> &'static str {
        match self {
            PackKind::Pallet => "T",
            PackKind::Carton => "P",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pack {
    pub kind: PackKind,
    /// MAN02 of `MAN*GM`: the SSCC on its label.
    pub sscc: Option<String>,
    pub contents: Vec<Node>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Item {
    /// LIN01: the order's line id (PO101), echoed.
    pub line_id: String,
    /// LIN02 on: the order's product ids, echoed.
    pub product_ids: Vec<ProductId>,
    /// SN102 and SN103: shipped, in the partner's unit.
    pub shipped: Option<Decimal>,
    pub uom: String,
    /// SN105 and SN106: ordered.
    pub ordered: Option<Decimal>,
    pub ordered_uom: String,
    /// PID05.
    pub description: String,
    /// Packs under the item ("standard carton").
    pub packs: Vec<Pack>,
    /// The lots or serial numbers of the line shipped. One lot is written
    /// in the LIN (`LT`) with its expiry date in a `DTM*036`; several, as a
    /// `REF*LT` each. They are the delivery line's, not the carton's:
    /// packing does not record which lot is in which carton.
    pub lots: Vec<ItemLot>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ItemLot {
    pub lot: String,
    pub expiry: Option<NaiveDate>,
    /// A serial number, written with `SN` (LIN) and `SE` (REF) rather than
    /// the lot's `LT`.
    pub serial: bool,
}

impl ItemLot {
    fn lin_qualifier(&self) -> &'static str {
        if self.serial { "SN" } else { "LT" }
    }

    fn ref_qualifier(&self) -> &'static str {
        if self.serial { "SE" } else { "LT" }
    }
}

impl ShipNotice {
    /// The number of HL segments the tree takes.
    pub fn levels(&self) -> usize {
        fn count(nodes: &[Node]) -> usize {
            nodes
                .iter()
                .map(|n| match n {
                    Node::Pack(p) => 1 + count(&p.contents),
                    Node::Item(i) => {
                        1 + i
                            .packs
                            .iter()
                            .map(|p| 1 + count(&p.contents))
                            .sum::<usize>()
                    }
                })
                .sum()
        }
        1 + self
            .orders
            .iter()
            .map(|o| 1 + count(&o.contents))
            .sum::<usize>()
    }

    /// The segments from BSN to CTT, without ST and SE. The HL segments
    /// are numbered here, each naming its parent.
    pub fn body(&self) -> Vec<Segment> {
        let mut w = Writer {
            out: Vec::new(),
            next: 0,
        };
        w.out.push(segment(
            "BSN",
            vec![
                self.purpose.clone(),
                self.shipment_id.clone(),
                date(self.date),
                self.time.clone(),
                self.structure.clone(),
            ],
        ));
        let shipment = w.hl(None, "S", !self.orders.is_empty());
        if let Some(l) = &self.lading {
            w.out.push(segment(
                "TD1",
                vec![
                    l.packaging.clone(),
                    opt_number(l.quantity),
                    String::new(),
                    String::new(),
                    String::new(),
                    l.weight_qualifier.clone(),
                    opt_number(l.weight),
                    l.weight_uom.clone(),
                ],
            ));
        }
        if let Some(c) = &self.carrier {
            let mut td5 = vec![
                String::new(),
                c.id_qualifier.clone(),
                c.id.clone(),
                c.method.clone(),
                c.routing.clone(),
            ];
            if !c.service.is_empty() {
                td5.extend(std::iter::repeat_n(String::new(), 6));
                td5.push(c.service.clone());
            }
            w.out.push(segment("TD5", td5));
        }
        if let Some(e) = &self.equipment {
            w.out.push(segment(
                "TD3",
                vec![e.kind.clone(), e.initial.clone(), e.number.clone()],
            ));
        }
        w.out.extend(self.references.iter().map(reference_segment));
        w.out.extend(self.dates.iter().map(date_segment));
        for party in &self.parties {
            w.out.extend(party_segments(party));
        }
        for order in &self.orders {
            let id = w.hl(Some(shipment), "O", !order.contents.is_empty());
            w.out.push(segment(
                "PRF",
                vec![
                    order.order_number.clone(),
                    order.release.clone(),
                    String::new(),
                    date(order.order_date),
                ],
            ));
            w.out.extend(order.references.iter().map(reference_segment));
            for party in &order.parties {
                w.out.extend(party_segments(party));
            }
            // A stack of what is still to write, deepest last: no
            // recursion, whatever the tree.
            let mut stack: Vec<(usize, &Node)> =
                order.contents.iter().rev().map(|n| (id, n)).collect();
            let mut packs_of_items: Vec<(usize, &Pack)> = Vec::new();
            while let Some((parent, node)) = stack.pop() {
                match node {
                    Node::Pack(pack) => {
                        let me = w.pack(parent, pack);
                        stack.extend(pack.contents.iter().rev().map(|n| (me, n)));
                    }
                    Node::Item(item) => {
                        let me = w.item(parent, item);
                        packs_of_items.clear();
                        packs_of_items.extend(item.packs.iter().map(|p| (me, p)));
                        for (parent, pack) in packs_of_items.drain(..) {
                            let pack_id = w.pack(parent, pack);
                            stack.extend(pack.contents.iter().rev().map(|n| (pack_id, n)));
                        }
                    }
                }
            }
        }
        let levels = w.next;
        w.out.push(segment("CTT", vec![levels.to_string()]));
        w.out
    }
}

struct Writer {
    out: Vec<Segment>,
    next: usize,
}

impl Writer {
    fn hl(&mut self, parent: Option<usize>, code: &str, children: bool) -> usize {
        self.next += 1;
        self.out.push(segment(
            "HL",
            vec![
                self.next.to_string(),
                parent.map(|p| p.to_string()).unwrap_or_default(),
                code.to_owned(),
                if children { "1" } else { "0" }.to_owned(),
            ],
        ));
        self.next
    }

    fn pack(&mut self, parent: usize, pack: &Pack) -> usize {
        let id = self.hl(Some(parent), pack.kind.code(), !pack.contents.is_empty());
        if let Some(sscc) = &pack.sscc {
            self.out
                .push(segment("MAN", vec!["GM".into(), sscc.clone()]));
        }
        id
    }

    fn item(&mut self, parent: usize, item: &Item) -> usize {
        let id = self.hl(Some(parent), "I", !item.packs.is_empty());
        let mut lin = vec![item.line_id.clone()];
        lin.extend(product_elements(&item.product_ids));
        if let [one] = item.lots.as_slice() {
            lin.extend([one.lin_qualifier().to_owned(), one.lot.clone()]);
        }
        self.out.push(segment("LIN", lin));
        self.out.push(segment(
            "SN1",
            vec![
                String::new(),
                opt_number(item.shipped),
                item.uom.clone(),
                String::new(),
                opt_number(item.ordered),
                item.ordered_uom.clone(),
            ],
        ));
        if !item.description.is_empty() {
            self.out.push(segment(
                "PID",
                vec![
                    "F".into(),
                    String::new(),
                    String::new(),
                    String::new(),
                    item.description.clone(),
                ],
            ));
        }
        match item.lots.as_slice() {
            [] => {}
            [one] => {
                if let Some(expiry) = one.expiry {
                    self.out.push(segment(
                        "DTM",
                        vec!["036".into(), expiry.format("%Y%m%d").to_string()],
                    ));
                }
            }
            several => {
                for lot in several {
                    self.out.push(segment(
                        "REF",
                        vec![lot.ref_qualifier().into(), lot.lot.clone()],
                    ));
                }
            }
        }
        id
    }
}

/// One HL level as read, before the tree is built.
enum Level {
    Shipment,
    Order(ShipOrder),
    Pack(Pack),
    Item(Item),
}

struct Flat {
    parent: Option<usize>,
    level: Level,
    depth: usize,
}

/// Reads a transaction set as an 856. An HL whose parent does not exist
/// or comes after it is a syntax error, and the tree is built from the
/// levels that can be placed.
pub fn read(set: &TransactionSet) -> (ShipNotice, SetVerdict) {
    let mut fields = Fields::default();
    let mut notice = ShipNotice::default();
    if set.id != "856" {
        fields.verdict.codes.push(SET_NOT_SUPPORTED);
        return (notice, fields.verdict);
    }
    let mut seen_bsn = false;
    let mut levels: Vec<Flat> = Vec::new();
    // HL01 as sent, to the level's place in `levels`.
    let mut ids: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut in_party = false;
    for (index, segment) in set.segments.iter().enumerate() {
        let position = index + 1;
        let tag = segment.tag.as_str();
        if !matches!(tag, "N1" | "N2" | "N3" | "N4") {
            in_party = false;
        }
        let current = levels.last_mut();
        match (tag, current) {
            ("ST" | "SE", _) => {}
            ("BSN", None) if !seen_bsn && position == 2 => {
                seen_bsn = true;
                notice.purpose = fields.required(position, segment, 1);
                notice.shipment_id = fields.required(position, segment, 2);
                notice.date = fields.date(position, segment, 3);
                notice.time = segment.get(4).to_owned();
                notice.structure = segment.get(5).to_owned();
            }
            ("HL", _) => {
                let id = fields.required(position, segment, 1);
                let parent_id = segment.get(2);
                let code = fields.required(position, segment, 3);
                let parent = if parent_id.is_empty() {
                    None
                } else {
                    match ids.get(parent_id) {
                        Some(p) => Some(*p),
                        None => {
                            // A parent that does not exist, or comes later.
                            fields.error(position, segment, 2, ELEMENT_INVALID_CHARACTER);
                            continue;
                        }
                    }
                };
                let parent_level = parent.map(|p| &levels[p].level);
                let depth = parent.map_or(0, |p| levels[p].depth + 1);
                let level = match (code.as_str(), parent_level) {
                    ("S", None) if levels.is_empty() => Some(Level::Shipment),
                    ("O", Some(Level::Shipment)) => Some(Level::Order(ShipOrder::default())),
                    ("T", Some(Level::Order(_))) => Some(Level::Pack(Pack {
                        kind: PackKind::Pallet,
                        sscc: None,
                        contents: Vec::new(),
                    })),
                    ("P", Some(Level::Order(_) | Level::Pack(_) | Level::Item(_))) => {
                        Some(Level::Pack(Pack {
                            kind: PackKind::Carton,
                            sscc: None,
                            contents: Vec::new(),
                        }))
                    }
                    ("I", Some(Level::Order(_) | Level::Pack(_))) => {
                        Some(Level::Item(Item::default()))
                    }
                    _ => None,
                };
                let Some(level) = level.filter(|_| depth <= MAX_DEPTH && !ids.contains_key(&id))
                else {
                    fields.verdict.segments.push(SegmentError::segment(
                        "HL",
                        position,
                        SEGMENT_UNEXPECTED,
                    ));
                    continue;
                };
                ids.insert(id, levels.len());
                levels.push(Flat {
                    parent,
                    level,
                    depth,
                });
            }
            (
                "TD1",
                Some(Flat {
                    level: Level::Shipment,
                    ..
                }),
            ) => {
                notice.lading = Some(Lading {
                    packaging: segment.get(1).to_owned(),
                    quantity: fields.number(position, segment, 2),
                    weight_qualifier: segment.get(6).to_owned(),
                    weight: fields.number(position, segment, 7),
                    weight_uom: segment.get(8).to_owned(),
                });
            }
            (
                "TD5",
                Some(Flat {
                    level: Level::Shipment,
                    ..
                }),
            ) => {
                notice.carrier = Some(Carrier {
                    id_qualifier: segment.get(2).to_owned(),
                    id: segment.get(3).to_owned(),
                    method: segment.get(4).to_owned(),
                    routing: segment.get(5).to_owned(),
                    service: segment.get(12).to_owned(),
                });
            }
            (
                "TD3",
                Some(Flat {
                    level: Level::Shipment,
                    ..
                }),
            ) => {
                notice.equipment = Some(Equipment {
                    kind: segment.get(1).to_owned(),
                    initial: segment.get(2).to_owned(),
                    number: segment.get(3).to_owned(),
                });
            }
            ("REF", Some(flat)) => {
                let reference = Reference {
                    qualifier: fields.required(position, segment, 1),
                    value: segment.get(2).to_owned(),
                    description: segment.get(3).to_owned(),
                };
                match &mut flat.level {
                    Level::Shipment => notice.references.push(reference),
                    Level::Order(order) => order.references.push(reference),
                    Level::Item(item)
                        if reference.qualifier == "LT" || reference.qualifier == "SE" =>
                    {
                        item.lots.push(ItemLot {
                            serial: reference.qualifier == "SE",
                            lot: reference.value,
                            expiry: None,
                        })
                    }
                    _ => notice.unhandled.push((position, tag.to_owned())),
                }
            }
            (
                "DTM",
                Some(Flat {
                    level: Level::Shipment,
                    ..
                }),
            ) => {
                let qualifier = fields.required(position, segment, 1);
                if let Some(date) = fields.date(position, segment, 2) {
                    notice.dates.push(DateOf { qualifier, date });
                }
            }
            (
                "DTM",
                Some(Flat {
                    level: Level::Item(item),
                    ..
                }),
            ) if segment.get(1) == "036" => {
                let date = fields.date(position, segment, 2);
                match item.lots.as_mut_slice() {
                    [one] => one.expiry = date,
                    _ => notice.unhandled.push((position, tag.to_owned())),
                }
            }
            ("N1" | "N2" | "N3" | "N4", Some(flat)) => {
                let parties = match &mut flat.level {
                    Level::Shipment => Some(&mut notice.parties),
                    Level::Order(order) => Some(&mut order.parties),
                    _ => None,
                };
                let placed = match parties {
                    Some(parties) => read_party(parties, in_party, segment),
                    None => false,
                };
                if placed {
                    in_party = true;
                } else {
                    notice.unhandled.push((position, tag.to_owned()));
                }
            }
            (
                "PRF",
                Some(Flat {
                    level: Level::Order(order),
                    ..
                }),
            ) => {
                order.order_number = fields.required(position, segment, 1);
                order.release = segment.get(2).to_owned();
                order.order_date = fields.date(position, segment, 4);
            }
            (
                "MAN",
                Some(Flat {
                    level: Level::Pack(pack),
                    ..
                }),
            ) if segment.get(1) == "GM" => {
                pack.sscc = Some(fields.required(position, segment, 2));
            }
            (
                "LIN",
                Some(Flat {
                    level: Level::Item(item),
                    ..
                }),
            ) => {
                item.line_id = segment.get(1).to_owned();
                let ids = read_product_ids(&mut fields, position, segment, 2, 30);
                let (lots, ids): (Vec<ProductId>, Vec<ProductId>) = ids
                    .into_iter()
                    .partition(|id| id.qualifier == "LT" || id.qualifier == "SN");
                item.product_ids = ids;
                item.lots.extend(lots.into_iter().map(|id| ItemLot {
                    serial: id.qualifier == "SN",
                    lot: id.value,
                    expiry: None,
                }));
            }
            (
                "SN1",
                Some(Flat {
                    level: Level::Item(item),
                    ..
                }),
            ) => {
                item.shipped = fields.number(position, segment, 2);
                item.uom = fields.required(position, segment, 3);
                item.ordered = fields.number(position, segment, 5);
                item.ordered_uom = segment.get(6).to_owned();
            }
            (
                "PID",
                Some(Flat {
                    level: Level::Item(item),
                    ..
                }),
            ) => {
                item.description = segment.get(5).to_owned();
            }
            ("CTT", _) => notice.levels_stated = fields.whole(position, segment, 1),
            _ => notice.unhandled.push((position, tag.to_owned())),
        }
    }
    if !seen_bsn {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("BSN", 2, SEGMENT_MISSING));
    }
    if levels.is_empty() {
        fields
            .verdict
            .segments
            .push(SegmentError::segment("HL", 3, SEGMENT_MISSING));
    }
    if let Some(stated) = notice.levels_stated
        && stated as usize != levels.len()
    {
        notice.notes.push(format!(
            "The notice says it has {stated} levels (CTT01) and has {}.",
            levels.len()
        ));
    }
    build(&mut notice, levels);
    (notice, fields.verdict)
}

/// Builds the tree from the levels read. A child always comes after its
/// parent, so walking backwards builds every child before its parent:
/// no recursion, whatever the input.
fn build(notice: &mut ShipNotice, levels: Vec<Flat>) {
    let mut children: Vec<Vec<Node>> = (0..levels.len()).map(|_| Vec::new()).collect();
    let mut orders: Vec<ShipOrder> = Vec::new();
    let mut flats: Vec<Option<Flat>> = levels.into_iter().map(Some).collect();
    for index in (0..flats.len()).rev() {
        let Some(flat) = flats[index].take() else {
            continue;
        };
        let mut mine = std::mem::take(&mut children[index]);
        mine.reverse();
        let built = match flat.level {
            Level::Shipment => continue,
            Level::Order(mut order) => {
                order.contents = mine;
                orders.push(order);
                continue;
            }
            Level::Pack(mut pack) => {
                pack.contents = mine;
                Node::Pack(pack)
            }
            Level::Item(mut item) => {
                item.packs = mine
                    .into_iter()
                    .filter_map(|n| match n {
                        Node::Pack(p) => Some(p),
                        Node::Item(_) => None,
                    })
                    .collect();
                Node::Item(item)
            }
        };
        if let Some(parent) = flat.parent {
            children[parent].push(built);
        }
    }
    orders.reverse();
    notice.orders = orders;
}

/// Every item of the notice with the order it belongs to, in the order
/// written: what a check of quantities needs.
pub fn items(notice: &ShipNotice) -> Vec<(&ShipOrder, &Item)> {
    let mut out = Vec::new();
    for order in &notice.orders {
        let mut stack: Vec<&Node> = order.contents.iter().rev().collect();
        while let Some(node) = stack.pop() {
            match node {
                Node::Pack(p) => stack.extend(p.contents.iter().rev()),
                Node::Item(i) => out.push((order, i)),
            }
        }
    }
    out
}

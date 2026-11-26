//! Time-phased planning: the calculation, apart from the database (see
//! ARCHITECTURE.md, "Time-phased planning (MRP)").
//!
//! Given what is on hand, what is due to come in and when, what is needed
//! and when, how each item and warehouse is supplied (bought with a lead
//! time, or made by a bill with a lead time) and the bills themselves, it
//! nets requirements level by level and proposes planned orders with their
//! release and due dates, what each serves, and the exceptions a person
//! should look at. It changes nothing and reads nothing.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, NaiveDate};
use erpcraft_kernel::{Decimal, JsonSchema, schemars};
use serde::Serialize;
use uuid::Uuid;

/// An item at a warehouse: what is planned on its own.
pub type Node = (Uuid, Uuid);

/// How a node is supplied.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Buy { vendor: Option<Uuid> },
    Make { bom: Uuid },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub source: Source,
    pub lead_days: i64,
    pub safety_stock: Decimal,
    pub min_order_qty: Option<Decimal>,
    pub multiple: Option<Decimal>,
}

/// What a demand is for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Peg {
    /// A sales order line still to deliver.
    SalesOrder { document_id: Uuid, line_id: Uuid },
    /// A released production order's component.
    ProductionOrder { document_id: Uuid },
    /// What is kept on hand beyond every need.
    SafetyStock,
    /// A component of a planned order to make, by its place in the plan.
    PlannedOrder { planned: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Demand {
    pub node: Node,
    pub date: NaiveDate,
    pub qty: Decimal,
    pub peg: Peg,
}

/// What is scheduled to come in: an order that exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptKind {
    PurchaseOrder,
    ProductionOrder,
    DraftPurchaseOrder,
    DraftProductionOrder,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Receipt {
    pub node: Node,
    pub date: NaiveDate,
    pub qty: Decimal,
    pub kind: ReceiptKind,
    pub document_id: Uuid,
}

/// A bill, in stock units: what one run makes and takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Bill {
    pub product: Uuid,
    pub output: Decimal,
    pub lines: Vec<(Uuid, Decimal)>,
}

#[derive(Debug, Clone, Default)]
pub struct Input {
    pub as_of: NaiveDate,
    pub on_hand: BTreeMap<Node, Decimal>,
    pub demands: Vec<Demand>,
    pub receipts: Vec<Receipt>,
    /// Every node planned, with how it is supplied.
    pub params: BTreeMap<Node, Params>,
    /// Every active bill, by its id.
    pub bills: BTreeMap<Uuid, Bill>,
}

/// One demand a planned order serves, and how much of it.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Pegged {
    #[serde(flatten)]
    pub peg: Peg,
    pub date: NaiveDate,
    pub qty: Decimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct PlannedOrder {
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    /// `buy` or `make`.
    pub source: String,
    pub vendor_id: Option<Uuid>,
    pub bom_id: Option<Uuid>,
    /// In stock units.
    pub qty: Decimal,
    /// When to order or start it.
    pub release_date: NaiveDate,
    /// When it is to be in stock.
    pub due_date: NaiveDate,
    pub pegs: Vec<Pegged>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Exception {
    /// `expedite`, `defer`, `cancel`, `past_due` or `release_late`.
    pub kind: String,
    pub item_id: Uuid,
    pub warehouse_id: Uuid,
    /// The order it is about; none for a planned order.
    pub document_id: Option<Uuid>,
    pub receipt: Option<ReceiptKind>,
    /// The planned order it is about, by its place in the plan.
    pub planned: Option<usize>,
    /// The date it has (due, or release for a planned order).
    pub date: NaiveDate,
    /// The date it should have: needed by (expedite), not needed before
    /// (defer).
    pub to_date: Option<NaiveDate>,
    pub qty: Decimal,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Plan {
    pub planned_orders: Vec<PlannedOrder>,
    pub exceptions: Vec<Exception>,
    /// The lowest level each item is used at in any bill: 0 for what is
    /// made of others and in none, 1 for their components, and so on.
    pub levels: BTreeMap<Uuid, u32>,
}

/// The low-level code of every item in the bills: refused, with the items
/// that lead back to themselves, when the bills hold a cycle.
pub fn levels(bills: &BTreeMap<Uuid, Bill>) -> Result<BTreeMap<Uuid, u32>, Vec<Uuid>> {
    let mut children: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    for bill in bills.values() {
        let entry = children.entry(bill.product).or_default();
        for (component, _) in &bill.lines {
            entry.insert(*component);
        }
    }
    let mut level: BTreeMap<Uuid, u32> = BTreeMap::new();
    // Depth first from every product, the path kept to find a cycle; an
    // item's level is the deepest it is reached at.
    fn walk(
        item: Uuid,
        depth: u32,
        children: &BTreeMap<Uuid, BTreeSet<Uuid>>,
        level: &mut BTreeMap<Uuid, u32>,
        path: &mut Vec<Uuid>,
    ) -> Result<(), Vec<Uuid>> {
        if let Some(at) = path.iter().position(|p| *p == item) {
            let mut cycle = path[at..].to_vec();
            cycle.push(item);
            return Err(cycle);
        }
        if level.get(&item).is_some_and(|l| *l >= depth) && depth > 0 {
            return Ok(());
        }
        let current = level.entry(item).or_insert(depth);
        *current = (*current).max(depth);
        path.push(item);
        if let Some(below) = children.get(&item) {
            for child in below {
                walk(*child, depth + 1, children, level, path)?;
            }
        }
        path.pop();
        Ok(())
    }
    for product in children.keys() {
        walk(*product, 0, &children, &mut level, &mut vec![])?;
    }
    Ok(level)
}

/// What can meet a demand at a node, in the order it is taken.
struct Supply {
    date: NaiveDate,
    left: Decimal,
    /// The receipt it is, if it is one; else on hand or a planned order.
    receipt: Option<usize>,
    planned: Option<usize>,
}

fn lot(short: Decimal, params: &Params) -> Decimal {
    let mut qty = short.max(params.min_order_qty.unwrap_or(Decimal::ZERO));
    if let Some(multiple) = params.multiple.filter(|m| *m > Decimal::ZERO) {
        qty = (qty / multiple).ceil() * multiple;
    }
    qty.normalize()
}

/// The plan: planned orders, what they serve, and the exceptions.
pub fn plan(input: &Input) -> Result<Plan, Vec<Uuid>> {
    let levels = levels(&input.bills)?;
    let level_of = |item: &Uuid| levels.get(item).copied().unwrap_or(0);
    let mut nodes: Vec<Node> = input.params.keys().copied().collect();
    nodes.sort_by_key(|n| (level_of(&n.0), *n));

    let mut demands: Vec<Demand> = input.demands.clone();
    let mut out = Plan {
        levels: levels.clone(),
        ..Plan::default()
    };
    // Per receipt: the earliest date anything needed it.
    let mut needed: Vec<Option<NaiveDate>> = vec![None; input.receipts.len()];

    for node in nodes {
        let params = &input.params[&node];
        let mut supplies: Vec<Supply> = vec![];
        let on_hand = input.on_hand.get(&node).copied().unwrap_or_default();
        if on_hand > Decimal::ZERO {
            supplies.push(Supply {
                date: input.as_of,
                left: on_hand,
                receipt: None,
                planned: None,
            });
        }
        let mut receipts: Vec<(usize, &Receipt)> = input
            .receipts
            .iter()
            .enumerate()
            .filter(|(_, r)| r.node == node)
            .collect();
        receipts.sort_by_key(|(i, r)| (r.date, *i));
        for (index, r) in &receipts {
            supplies.push(Supply {
                date: r.date.max(input.as_of),
                left: r.qty,
                receipt: Some(*index),
                planned: None,
            });
            if r.date < input.as_of
                && matches!(
                    r.kind,
                    ReceiptKind::PurchaseOrder | ReceiptKind::ProductionOrder
                )
            {
                out.exceptions.push(Exception {
                    kind: "past_due".into(),
                    item_id: node.0,
                    warehouse_id: node.1,
                    document_id: Some(r.document_id),
                    receipt: Some(r.kind.clone()),
                    planned: None,
                    date: r.date,
                    to_date: None,
                    qty: r.qty,
                });
            }
        }

        let mut wanted: Vec<Demand> = demands.iter().filter(|d| d.node == node).cloned().collect();
        if params.safety_stock > Decimal::ZERO {
            wanted.push(Demand {
                node,
                date: input.as_of,
                qty: params.safety_stock,
                peg: Peg::SafetyStock,
            });
        }
        // A demand before the run's date is wanted now.
        for d in &mut wanted {
            d.date = d.date.max(input.as_of);
        }
        wanted.sort_by(|a, b| (a.date, &a.peg).cmp(&(b.date, &b.peg)));

        for demand in wanted {
            let mut short = demand.qty;
            // What exists or is planned already, earliest first.
            supplies.sort_by_key(|s| (s.date, s.receipt.is_none() && s.planned.is_none()));
            for supply in supplies.iter_mut().filter(|s| s.left > Decimal::ZERO) {
                if short <= Decimal::ZERO {
                    break;
                }
                let take = short.min(supply.left);
                supply.left -= take;
                short -= take;
                if let Some(r) = supply.receipt {
                    needed[r] = Some(needed[r].map_or(demand.date, |n| n.min(demand.date)));
                }
                if let Some(p) = supply.planned {
                    out.planned_orders[p].pegs.push(Pegged {
                        peg: demand.peg.clone(),
                        date: demand.date,
                        qty: take.normalize(),
                    });
                }
            }
            if short <= Decimal::ZERO {
                continue;
            }
            // A planned order, due when it is needed.
            let qty = lot(short, params);
            let due = demand.date;
            let release = due - Duration::days(params.lead_days);
            let index = out.planned_orders.len();
            let (source, vendor, bom) = match &params.source {
                Source::Buy { vendor } => ("buy", *vendor, None),
                Source::Make { bom } => ("make", None, Some(*bom)),
            };
            out.planned_orders.push(PlannedOrder {
                item_id: node.0,
                warehouse_id: node.1,
                source: source.into(),
                vendor_id: vendor,
                bom_id: bom,
                qty,
                release_date: release,
                due_date: due,
                pegs: vec![Pegged {
                    peg: demand.peg.clone(),
                    date: demand.date,
                    qty: short.normalize(),
                }],
            });
            if release < input.as_of {
                out.exceptions.push(Exception {
                    kind: "release_late".into(),
                    item_id: node.0,
                    warehouse_id: node.1,
                    document_id: None,
                    receipt: None,
                    planned: Some(index),
                    date: release,
                    to_date: None,
                    qty,
                });
            }
            if qty > short {
                supplies.push(Supply {
                    date: due,
                    left: qty - short,
                    receipt: None,
                    planned: Some(index),
                });
            }
            // What it takes, needed when it starts.
            if let Source::Make { bom } = &params.source
                && let Some(bill) = input.bills.get(bom)
                && bill.output > Decimal::ZERO
            {
                for (component, per_run) in &bill.lines {
                    // Up to the places stock is kept in, never short.
                    let million = Decimal::from(1_000_000);
                    let need = (per_run * qty / bill.output * million).ceil() / million;
                    if need > Decimal::ZERO {
                        demands.push(Demand {
                            node: (*component, node.1),
                            date: release.max(input.as_of),
                            qty: need.normalize(),
                            peg: Peg::PlannedOrder { planned: index },
                        });
                    }
                }
            }
        }
    }

    // Scheduled receipts against when they are needed.
    for (index, r) in input.receipts.iter().enumerate() {
        if !input.params.contains_key(&r.node) {
            continue;
        }
        let due = r.date.max(input.as_of);
        let exception = match needed[index] {
            None => Some(("cancel", None)),
            Some(need) if need < due => Some(("expedite", Some(need))),
            Some(need) if need > due => Some(("defer", Some(need))),
            Some(_) => None,
        };
        if let Some((kind, to_date)) = exception {
            out.exceptions.push(Exception {
                kind: kind.into(),
                item_id: r.node.0,
                warehouse_id: r.node.1,
                document_id: Some(r.document_id),
                receipt: Some(r.kind.clone()),
                planned: None,
                date: r.date,
                to_date,
                qty: r.qty,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 12, d).unwrap()
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    const WH: u128 = 100;
    const BIKE: u128 = 1;
    const FRAME: u128 = 2;
    const TUBE: u128 = 3;

    fn buy(lead: i64) -> Params {
        Params {
            source: Source::Buy {
                vendor: Some(id(900)),
            },
            lead_days: lead,
            safety_stock: Decimal::ZERO,
            min_order_qty: None,
            multiple: None,
        }
    }

    fn make(bom: u128, lead: i64) -> Params {
        Params {
            source: Source::Make { bom: id(bom) },
            ..buy(lead)
        }
    }

    /// A bike of a frame; a frame of two tubes.
    fn bills() -> BTreeMap<Uuid, Bill> {
        BTreeMap::from([
            (
                id(51),
                Bill {
                    product: id(BIKE),
                    output: dec("1"),
                    lines: vec![(id(FRAME), dec("1"))],
                },
            ),
            (
                id(52),
                Bill {
                    product: id(FRAME),
                    output: dec("1"),
                    lines: vec![(id(TUBE), dec("2"))],
                },
            ),
        ])
    }

    fn input() -> Input {
        Input {
            as_of: day(1),
            bills: bills(),
            params: BTreeMap::from([
                ((id(BIKE), id(WH)), make(51, 3)),
                ((id(FRAME), id(WH)), make(52, 5)),
                ((id(TUBE), id(WH)), buy(10)),
            ]),
            demands: vec![Demand {
                node: (id(BIKE), id(WH)),
                date: day(20),
                qty: dec("10"),
                peg: Peg::SalesOrder {
                    document_id: id(700),
                    line_id: id(701),
                },
            }],
            ..Input::default()
        }
    }

    #[test]
    fn levels_are_the_deepest_use_and_a_cycle_is_named() {
        let levels = levels(&bills()).unwrap();
        assert_eq!(levels[&id(BIKE)], 0);
        assert_eq!(levels[&id(FRAME)], 1);
        assert_eq!(levels[&id(TUBE)], 2);
        let mut cyclic = bills();
        cyclic.insert(
            id(53),
            Bill {
                product: id(TUBE),
                output: dec("1"),
                lines: vec![(id(BIKE), dec("1"))],
            },
        );
        let cycle = super::levels(&cyclic).unwrap_err();
        assert!(
            cycle.contains(&id(BIKE)) && cycle.contains(&id(TUBE)),
            "{cycle:?}"
        );
    }

    #[test]
    fn three_levels_are_planned_back_from_the_due_date() {
        let plan = plan(&input()).unwrap();
        let by_item = |item: u128| {
            plan.planned_orders
                .iter()
                .find(|p| p.item_id == id(item))
                .unwrap()
        };
        let bike = by_item(BIKE);
        assert_eq!(
            (bike.qty, bike.due_date, bike.release_date),
            (dec("10"), day(20), day(17))
        );
        let frame = by_item(FRAME);
        assert_eq!(
            (frame.qty, frame.due_date, frame.release_date),
            (dec("10"), day(17), day(12))
        );
        let tube = by_item(TUBE);
        assert_eq!(
            (tube.qty, tube.due_date, tube.release_date),
            (dec("20"), day(12), day(2))
        );
        // Pegged up the tree.
        assert_eq!(tube.pegs[0].peg, Peg::PlannedOrder { planned: 1 });
        assert_eq!(frame.pegs[0].peg, Peg::PlannedOrder { planned: 0 });
        assert!(matches!(bike.pegs[0].peg, Peg::SalesOrder { .. }));
        assert!(plan.exceptions.is_empty(), "{:?}", plan.exceptions);
    }

    #[test]
    fn stock_safety_minimums_and_exceptions() {
        let mut input = input();
        // Six tubes on hand; a purchase of 4 tubes due on the 15th, an order
        // of 4 frames due late; safety stock of 5 tubes; tubes in lots of 25.
        input.on_hand.insert((id(TUBE), id(WH)), dec("6"));
        let tube = input.params.get_mut(&(id(TUBE), id(WH))).unwrap();
        tube.safety_stock = dec("5");
        tube.min_order_qty = Some(dec("25"));
        input.receipts = vec![
            Receipt {
                node: (id(TUBE), id(WH)),
                date: day(15),
                qty: dec("4"),
                kind: ReceiptKind::PurchaseOrder,
                document_id: id(801),
            },
            Receipt {
                node: (id(FRAME), id(WH)),
                date: day(25),
                qty: dec("4"),
                kind: ReceiptKind::ProductionOrder,
                document_id: id(802),
            },
        ];
        // The bike's planned order is late to release: due in two days.
        input.demands[0].date = day(3);
        let plan = plan(&input).unwrap();
        let kinds: Vec<(&str, Option<Uuid>)> = plan
            .exceptions
            .iter()
            .map(|e| (e.kind.as_str(), e.document_id))
            .collect();
        assert!(kinds.contains(&("release_late", None)), "{kinds:?}");
        // The 4 frames due on the 25th are needed now: expedited, and the
        // other 6 planned.
        assert!(kinds.contains(&("expedite", Some(id(802)))), "{kinds:?}");
        // Tubes: 5 kept safe, 1 left, the 4 coming are expedited.
        let tubes: Vec<&PlannedOrder> = plan
            .planned_orders
            .iter()
            .filter(|p| p.item_id == id(TUBE))
            .collect();
        assert_eq!(tubes.len(), 1, "{tubes:?}");
        assert_eq!(tubes[0].qty, dec("25"));
        assert!(
            plan.exceptions
                .iter()
                .any(|e| e.kind == "expedite" && e.document_id == Some(id(801)))
        );
    }

    #[test]
    fn an_order_nothing_needs_is_cancelled_and_one_needed_later_deferred() {
        let mut input = input();
        input.receipts = vec![
            Receipt {
                node: (id(BIKE), id(WH)),
                date: day(5),
                qty: dec("10"),
                kind: ReceiptKind::ProductionOrder,
                document_id: id(803),
            },
            Receipt {
                node: (id(BIKE), id(WH)),
                date: day(6),
                qty: dec("7"),
                kind: ReceiptKind::PurchaseOrder,
                document_id: id(804),
            },
        ];
        let plan = plan(&input).unwrap();
        assert!(plan.planned_orders.is_empty(), "{:?}", plan.planned_orders);
        let e = |doc: u128| {
            plan.exceptions
                .iter()
                .find(|e| e.document_id == Some(id(doc)))
        };
        assert_eq!(e(803).unwrap().kind, "defer");
        assert_eq!(e(803).unwrap().to_date, Some(day(20)));
        assert_eq!(e(804).unwrap().kind, "cancel");
    }
}

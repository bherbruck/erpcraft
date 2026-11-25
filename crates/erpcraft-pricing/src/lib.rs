//! Price lists: the price and discount an order line is proposed.
//!
//! A company keeps price lists per side (sales, purchase) and currency,
//! for a party, a group of parties, or everyone, valid between dates, with
//! lines for an item or a group of items from a quantity on. Sales and
//! purchasing ask [`propose`] for a line's price when the line gives none,
//! keep what was proposed with [`keep`], and ask [`check`] when the order
//! is approved, which puts every change of a proposed price to the
//! company's policy `price.override`. On a purchase without a listed price
//! the last price paid to the vendor is proposed. See "Price lists" in
//! ARCHITECTURE.md.

use chrono::NaiveDate;
use erpcraft_kernel::{
    Ctx, Decimal, EntityMeta, FieldMeta, FieldType, Migration, Module, PgConnection, PolicyPoint,
    Result,
};
use serde_json::json;
use uuid::Uuid;

/// The policy point asked when an approved order's price or discount is
/// not what was proposed.
pub const OVERRIDE: &str = "price.override";

pub struct PricingModule;

impl Module for PricingModule {
    fn name(&self) -> &'static str {
        "pricing"
    }

    fn label(&self) -> String {
        "Pricing".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec!["stock"]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                name: "pricing",
                sql: include_str!("../migrations/0001_pricing.sql"),
            },
            Migration {
                version: 2,
                name: "line_company",
                sql: include_str!("../migrations/0002_line_company.sql"),
            },
        ]
    }

    fn entities(&self) -> Vec<EntityMeta> {
        let created = || FieldMeta::new("created_at", "Created", FieldType::Timestamp).readonly();
        vec![
            EntityMeta::new("price_group", "Price group")
                .field(
                    FieldMeta::new(
                        "kind",
                        "Of",
                        FieldType::labelled(&[
                            ("customer", "Customers"),
                            ("vendor", "Vendors"),
                            ("item", "Items"),
                        ]),
                    )
                    .required()
                    .immutable(),
                )
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(created())
                .order_by(&["kind", "code"])
                .search(&["code", "name"]),
            EntityMeta::new("price_group_member", "Price group member")
                .field(
                    FieldMeta::new("group_id", "Group", FieldType::link("price_group")).required(),
                )
                .field(
                    FieldMeta::new("kind", "Of", FieldType::Text)
                        .readonly()
                        .help("The group's kind."),
                )
                .field(
                    FieldMeta::new("member_id", "Member", FieldType::entity_named_by("kind"))
                        .required()
                        .help("A customer, vendor or item, in one group of its kind at most."),
                )
                .field(created())
                .order_by(&["group_id"]),
            EntityMeta::new("price_list", "Price list")
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .required()
                        .immutable(),
                )
                .field(FieldMeta::new("code", "Code", FieldType::Text).required())
                .field(FieldMeta::new("name", "Name", FieldType::Text).required())
                .field(
                    FieldMeta::new(
                        "side",
                        "For",
                        FieldType::labelled(&[("sales", "Sales"), ("purchase", "Purchases")]),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new(
                        "currency",
                        "Currency",
                        FieldType::link_by("currency", "code"),
                    )
                    .required(),
                )
                .field(
                    FieldMeta::new(
                        "scope_type",
                        "Whom",
                        FieldType::labelled(&[
                            ("customer", "A customer"),
                            ("customer_group", "A group of customers"),
                            ("vendor", "A vendor"),
                            ("vendor_group", "A group of vendors"),
                        ]),
                    )
                    .help("Empty: everyone on its side."),
                )
                .field(
                    FieldMeta::new("scope_id", "Who", FieldType::entity_named_by("scope_type"))
                        .help("The customer, vendor or group."),
                )
                .field(FieldMeta::new("valid_from", "Valid from", FieldType::Date))
                .field(FieldMeta::new("valid_to", "Valid to", FieldType::Date))
                .field(FieldMeta::new("active", "Active", FieldType::Bool))
                .field(created())
                .order_by(&["code"])
                .search(&["code", "name"])
                .written_together(&["scope_type", "scope_id"])
                .company_field("company_id"),
            EntityMeta::new("price_list_line", "Price list line")
                .field(
                    FieldMeta::new("price_list_id", "Price list", FieldType::link("price_list"))
                        .required()
                        .immutable(),
                )
                .field(
                    FieldMeta::new("item_id", "Item", FieldType::link("item"))
                        .help("An item, or a group of items."),
                )
                .field(FieldMeta::new(
                    "item_group_id",
                    "Item group",
                    FieldType::link("price_group").filter("kind", "item"),
                ))
                .field(
                    FieldMeta::new("min_qty", "From quantity", FieldType::Decimal)
                        .quantity()
                        .help("A quantity break: the line applies from this quantity on."),
                )
                .field(
                    FieldMeta::new("unit_price", "Unit price", FieldType::Decimal)
                        .unit_price()
                        .help("In the list's currency. Empty: only a discount."),
                )
                .field(
                    FieldMeta::new("discount_percent", "Discount", FieldType::Decimal)
                        .percent(6)
                        .help("Off the price, this list's or one found elsewhere."),
                )
                .field(created())
                .field(
                    FieldMeta::new("company_id", "Company", FieldType::link("company"))
                        .immutable()
                        .help("The list's company; filled in from the list."),
                )
                .order_by(&["price_list_id", "min_qty"])
                .child_of("price_list", "price_list_id")
                .company_field("company_id"),
        ]
    }

    fn policies(&self) -> Vec<PolicyPoint> {
        vec![
            PolicyPoint::new(
                OVERRIDE,
                "Asked when a sales or purchase order is approved, for each line whose price or \
                 discount is not what the price lists (or the last price paid) proposed. Unless \
                 configured otherwise it is allowed; a company refuses it or asks for approval, \
                 for example when `facts.side == 'sales' && facts.change_percent < -5`.",
            )
            .text("side", "sales or purchase")
            .text("item", "The item's code")
            .number("price", "The line's unit price")
            .number("list_price", "The unit price proposed")
            .number(
                "change_percent",
                "How far the net price is from the net price proposed, in percent: below zero \
                 is cheaper",
            )
            .as_percent()
            .number("discount", "The line's discount percent, 0 for none")
            .number("list_discount", "The discount percent proposed, 0 for none")
            .number(
                "discount_amount",
                "The amount taken off the line, 0 for none",
            )
            .text("source", "list, or last_paid for the last price paid")
            .refusal("the price differs from the price list"),
        ]
    }
}

/// Which side of the business a price is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Sales,
    Purchase,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::Sales => "sales",
            Side::Purchase => "purchase",
        }
    }

    fn party(self) -> &'static str {
        match self {
            Side::Sales => "customer",
            Side::Purchase => "vendor",
        }
    }
}

/// What a line's price is proposed from. Prices are per the item's stock
/// unit, and `qty` is in it; a line in another unit scales the proposal by
/// its factor ([`Proposal::per`]).
#[derive(Debug, Clone)]
pub struct Query {
    pub company_id: Uuid,
    pub side: Side,
    /// The customer or vendor.
    pub party_id: Uuid,
    pub item_id: Uuid,
    pub qty: Decimal,
    pub currency: String,
    /// The order's date: lists valid on it count.
    pub date: NaiveDate,
}

/// A proposed price and discount.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub unit_price: Option<Decimal>,
    pub discount_percent: Option<Decimal>,
    /// `list` or `last_paid`.
    pub source: &'static str,
    /// The list line the price came from, else the discount's.
    pub price_list_line_id: Option<Uuid>,
}

impl Proposal {
    /// The proposal for a line in a unit of `factor` stock units: the
    /// price per stock unit times the factor, rounded to the six places of
    /// a unit price.
    pub fn per(mut self, factor: Decimal) -> Self {
        // The last price paid is per stock unit exactly (10 for a box of 3
        // is 3.333...), so that a box comes back at 10, not 9.999999.
        self.unit_price = self
            .unit_price
            .map(|p| (p * factor).round_dp(6).normalize());
        self
    }
}

/// The price and discount the lists propose for a line, and on a purchase
/// without a listed price the last price paid. `None` when nothing says.
pub async fn propose(conn: &mut PgConnection, q: &Query) -> Result<Option<Proposal>> {
    let party = q.side.party();
    let rows: Vec<(Uuid, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        "SELECT l.id, l.unit_price, l.discount_percent
         FROM price_list p
         JOIN price_list_line l ON l.price_list_id = p.id
         WHERE p.company_id = $1 AND p.side = $2 AND p.currency = $3 AND p.active
           AND (p.valid_from IS NULL OR p.valid_from <= $4)
           AND (p.valid_to IS NULL OR p.valid_to >= $4)
           AND (p.scope_type IS NULL
                OR (p.scope_type = $5 AND p.scope_id = $6)
                OR (p.scope_type = $5 || '_group' AND p.scope_id IN
                       (SELECT group_id FROM price_group_member WHERE kind = $5 AND member_id = $6)))
           AND (l.item_id = $7
                OR l.item_group_id IN (SELECT group_id FROM price_group_member
                                       WHERE kind = 'item' AND member_id = $7))
           AND l.min_qty <= $8
         ORDER BY CASE WHEN p.scope_type = $5 THEN 0 WHEN p.scope_type IS NOT NULL THEN 1 ELSE 2 END,
                  l.item_id IS NULL, l.min_qty DESC, p.code",
    )
    .bind(q.company_id)
    .bind(q.side.as_str())
    .bind(&q.currency)
    .bind(q.date)
    .bind(party)
    .bind(q.party_id)
    .bind(q.item_id)
    .bind(q.qty)
    .fetch_all(&mut *conn)
    .await?;
    let price = rows.iter().find_map(|r| r.1.map(|p| (r.0, p)));
    let discount = rows.iter().find_map(|r| r.2.map(|d| (r.0, d)));
    if let Some((line, price)) = price {
        return Ok(Some(Proposal {
            unit_price: Some(price),
            discount_percent: discount.map(|d| d.1),
            source: "list",
            price_list_line_id: Some(line),
        }));
    }
    if q.side == Side::Purchase
        && let Some((price, last_discount)) = last_paid(conn, q).await?
    {
        return Ok(Some(Proposal {
            unit_price: Some(price),
            discount_percent: discount.map(|d| d.1).or(last_discount),
            source: "last_paid",
            price_list_line_id: discount.map(|d| d.0),
        }));
    }
    Ok(discount.map(|(line, d)| Proposal {
        unit_price: None,
        discount_percent: Some(d),
        source: "list",
        price_list_line_id: Some(line),
    }))
}

/// The unit price and discount of the latest posted vendor invoice line of
/// the vendor for the item, in the currency: per stock unit, whatever unit
/// its order line was in.
async fn last_paid(
    conn: &mut PgConnection,
    q: &Query,
) -> Result<Option<(Decimal, Option<Decimal>)>> {
    Ok(sqlx::query_as(
        "SELECT l.unit_price / ol.factor, l.discount_percent
         FROM vendor_invoice_line l
         JOIN vendor_invoice v ON v.document_id = l.document_id
         JOIN document d ON d.id = v.document_id
         JOIN purchase_order_line ol ON ol.id = l.order_line_id
         JOIN purchase_order o ON o.document_id = ol.document_id
         JOIN company c ON c.id = d.company_id
         WHERE d.company_id = $1 AND d.status IN ('posted', 'paid', 'closed')
           AND COALESCE(v.vendor_id, o.vendor_id) = $2 AND ol.item_id = $3
           AND COALESCE(v.currency, o.currency, c.base_currency) = $4
         ORDER BY d.doc_date DESC, d.number DESC NULLS LAST LIMIT 1",
    )
    .bind(q.company_id)
    .bind(q.party_id)
    .bind(q.item_id)
    .bind(&q.currency)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Keep what an order's lines were proposed, replacing what was kept: a
/// line id and its proposal.
pub async fn keep(
    conn: &mut PgConnection,
    document_id: Uuid,
    proposals: &[(Uuid, Proposal)],
) -> Result<()> {
    sqlx::query("DELETE FROM price_proposal WHERE document_id = $1")
        .bind(document_id)
        .execute(&mut *conn)
        .await?;
    for (line, p) in proposals {
        sqlx::query(
            "INSERT INTO price_proposal (document_id, line_id, unit_price, discount_percent,
                                         source, price_list_line_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(document_id)
        .bind(line)
        .bind(p.unit_price)
        .bind(p.discount_percent)
        .bind(p.source)
        .bind(p.price_list_line_id)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// What an order's line was proposed, by line id: price, discount, source.
pub async fn kept(
    conn: &mut PgConnection,
    document_id: Uuid,
) -> Result<Vec<(Uuid, Option<Decimal>, Option<Decimal>, String)>> {
    Ok(sqlx::query_as(
        "SELECT line_id, unit_price, discount_percent, source FROM price_proposal
         WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// A line of an order as it is approved.
#[derive(Debug, Clone)]
pub struct LinePrice {
    pub line_id: Uuid,
    pub line_no: i32,
    pub item_code: String,
    pub unit_price: Decimal,
    pub discount_percent: Option<Decimal>,
    /// Off the whole line, after the percent.
    pub discount_amount: Option<Decimal>,
    pub qty: Decimal,
}

/// Ask the policy [`OVERRIDE`] about each line whose price or discount is
/// not what was proposed. Lines that were proposed nothing are not asked
/// about.
pub async fn check(
    ctx: &mut Ctx<'_>,
    company_id: Uuid,
    document_id: Uuid,
    side: Side,
    lines: &[LinePrice],
) -> Result<()> {
    let proposals = kept(&mut *ctx.conn, document_id).await?;
    for line in lines {
        let Some((_, list_price, list_discount, source)) =
            proposals.iter().find(|p| p.0 == line.line_id)
        else {
            continue;
        };
        let discount = line.discount_percent.unwrap_or_default();
        let list_discount = list_discount.unwrap_or_default();
        let list_price = list_price.unwrap_or(line.unit_price);
        let amount_off = line.discount_amount.unwrap_or_default();
        if list_price == line.unit_price && list_discount == discount && amount_off.is_zero() {
            continue;
        }
        // Per unit, after both discounts: an amount off the line counts as
        // a share of each unit.
        let per_unit_off = if line.qty.is_zero() {
            Decimal::ZERO
        } else {
            amount_off / line.qty
        };
        let net = |price: Decimal, discount: Decimal| {
            price * (Decimal::ONE_HUNDRED - discount) / Decimal::ONE_HUNDRED
        };
        let list_net = net(list_price, list_discount);
        let line_net = net(line.unit_price, discount) - per_unit_off;
        let change = if list_net.is_zero() {
            Decimal::ZERO
        } else {
            ((line_net - list_net) / list_net * Decimal::ONE_HUNDRED).round_dp(4)
        };
        ctx.decide(
            OVERRIDE,
            company_id,
            json!({
                "side": side.as_str(),
                "item": line.item_code,
                "price": line.unit_price.normalize().to_string(),
                "list_price": list_price.normalize().to_string(),
                "change_percent": change.normalize().to_string(),
                "discount": discount.normalize().to_string(),
                "list_discount": list_discount.normalize().to_string(),
                "discount_amount": amount_off.normalize().to_string(),
                "source": source,
            }),
        )
        .await
        .map_err(|e| e.with_line("lines", line.line_no as usize))?;
    }
    Ok(())
}

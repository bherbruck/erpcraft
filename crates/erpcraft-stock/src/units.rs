//! Units of measure: the stock unit of an item (`item.uom`), the other
//! units it is bought and sold in (`item_uom`), and converting a line's
//! quantity into stock units, exactly.

use erpcraft_kernel::{Decimal, Error, PgConnection, Result};
use uuid::Uuid;

/// What a unit is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Purchase,
    Sales,
}

/// The places of a stock quantity.
const STOCK_PLACES: u32 = 6;

/// The unit a line of `item_id` is in, and its factor: the stock unit,
/// factor one, when `uom` is absent or is the stock unit; else the item's
/// other unit of that code, which must be allowed for `purpose`.
pub async fn unit(
    conn: &mut PgConnection,
    item_id: Uuid,
    uom: Option<&str>,
    purpose: Purpose,
) -> Result<(String, Decimal)> {
    let (code, stock): (String, String) =
        sqlx::query_as("SELECT code, uom FROM item WHERE id = $1")
            .bind(item_id)
            .fetch_one(&mut *conn)
            .await?;
    let Some(uom) = uom.map(str::trim).filter(|u| !u.is_empty() && *u != stock) else {
        return Ok((stock, Decimal::ONE));
    };
    let found: Option<(Decimal, bool, bool)> = sqlx::query_as(
        "SELECT factor, purchase, sales FROM item_uom WHERE item_id = $1 AND uom = $2",
    )
    .bind(item_id)
    .bind(uom)
    .fetch_optional(&mut *conn)
    .await?;
    match found {
        Some((factor, purchase, sales)) => {
            let allowed = match purpose {
                Purpose::Purchase => purchase,
                Purpose::Sales => sales,
            };
            if !allowed {
                return Err(Error::validation(format!(
                    "item {code} is not {} in {uom}",
                    match purpose {
                        Purpose::Purchase => "bought",
                        Purpose::Sales => "sold",
                    }
                ))
                .with_field("uom"));
            }
            Ok((uom.to_owned(), factor))
        }
        None => Err(Error::validation(format!(
            "item {code} is kept in {stock} and has no unit {uom}: add it to the item's units \
             (item_uom) with its factor"
        ))
        .with_field("uom")),
    }
}

/// `qty` of a unit of `factor` in stock units, when it comes out within the
/// stock ledger's places exactly; never rounded.
pub fn to_stock(qty: Decimal, factor: Decimal) -> Option<Decimal> {
    let stock = qty.checked_mul(factor)?.normalize();
    (stock.scale() <= STOCK_PLACES).then_some(stock)
}

/// [`to_stock`], refused with a message when it does not come out exactly.
pub fn to_stock_or(qty: Decimal, factor: Decimal, uom: &str) -> Result<Decimal> {
    to_stock(qty, factor).ok_or_else(|| {
        Error::validation(format!(
            "{} {uom} is {} stock units, more places than stock is kept in",
            qty.normalize(),
            (qty * factor).normalize()
        ))
    })
}

/// A price per a unit of `factor`, per stock unit: for comparing and
/// proposing. Rounded to the places of a unit price (six).
pub fn price_per_stock_unit(price: Decimal, factor: Decimal) -> Decimal {
    if factor.is_zero() {
        return price;
    }
    (price / factor).round_dp(6).normalize()
}

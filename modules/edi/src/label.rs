//! The data of a carton's GS1-128 label, and its text for a ZPL printer.
//! The label is made from the same record as the ship notice, so the
//! SSCC the dock scans is the one the notice names. Rendering it as an
//! image or PDF is out of scope.

use erpcraft_kernel::{Ctx, Error, Result, entity};
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

use crate::outbound;
use crate::shipment::PERMISSION;

#[derive(Debug, Serialize, JsonSchema)]
pub struct Label {
    pub pack_id: Uuid,
    pub kind: String,
    /// The eighteen digits.
    pub sscc: String,
    /// What the barcode carries in human-readable form: `(00)` and the
    /// SSCC.
    pub element_string: String,
    pub ship_from: Option<String>,
    /// The ship-to as the partner's order named it.
    pub ship_to: Vec<String>,
    pub carrier: Option<String>,
    pub bill_of_lading: Option<String>,
    /// The partner's order number.
    pub order_number: String,
    /// This pack's place among the delivery's packs of its kind, and how
    /// many there are.
    pub number: i64,
    pub of: i64,
    /// The label for a ZPL printer, 4 by 6 inches at 203 dpi.
    pub zpl: String,
}

/// Text for a ZPL field: the printer's control characters taken out.
fn zpl_text(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '^' | '~') && !c.is_control())
        .take(40)
        .collect()
}

pub async fn label(ctx: &mut Ctx<'_>, pack_id: Uuid) -> Result<Label> {
    let row: Option<(Uuid, String, String, bool, i32)> = sqlx::query_as(
        "SELECT delivery_id, kind, sscc, voided, position FROM edi_pack WHERE id = $1",
    )
    .bind(pack_id)
    .fetch_optional(&mut *ctx.conn)
    .await?;
    let (delivery_id, kind, sscc, voided, _) = row.ok_or_else(|| Error::not_found("edi_pack"))?;
    if voided {
        return Err(Error::state(
            "this pack was replaced by a later packing: print the labels of that one",
        ));
    }
    let view = erpcraft_kernel::document::get(ctx, "sales_delivery", delivery_id).await?;
    ctx.require(PERMISSION, view.header.company_id)?;
    let order = outbound::order_id_of(ctx, &view)
        .await?
        .ok_or_else(|| Error::validation("this delivery follows from no sales order"))?;
    let sent = outbound::sent_order(ctx, order)
        .await?
        .ok_or_else(|| Error::validation("this delivery's order did not come by EDI"))?;
    let (number, of): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM edi_pack p2 WHERE p2.delivery_id = p.delivery_id
                   AND NOT p2.voided AND p2.kind = p.kind AND p2.position <= p.position),
                (SELECT count(*) FROM edi_pack p3 WHERE p3.delivery_id = p.delivery_id
                   AND NOT p3.voided AND p3.kind = p.kind)
         FROM edi_pack p WHERE p.id = $1",
    )
    .bind(pack_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    let (carrier, bill_of_lading): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT COALESCE(carrier_name, carrier_scac), bill_of_lading FROM edi_shipment
         WHERE delivery_id = $1",
    )
    .bind(delivery_id)
    .fetch_one(&mut *ctx.conn)
    .await?;
    let ship_from = entity::get(ctx, "company", view.header.company_id)
        .await
        .ok()
        .and_then(|c| c["name"].as_str().map(str::to_owned));
    let parties: Vec<crate::x12::po850::Party> =
        serde_json::from_value(sent.header["parties"].clone()).unwrap_or_default();
    let ship_to: Vec<String> = parties
        .iter()
        .find(|p| p.role == "ST")
        .map(|p| {
            let mut lines = vec![p.name.clone()];
            lines.extend(p.address_lines.iter().cloned());
            lines.push(
                [p.city.as_str(), p.region.as_str(), p.postal_code.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            lines.retain(|l| !l.is_empty());
            lines
        })
        .unwrap_or_default();

    let mut zpl = String::from("^XA\n^CI28\n");
    let mut y = 40;
    let mut line = |text: &str, size: u32| {
        zpl.push_str(&format!(
            "^FO40,{y}^A0N,{size},{size}^FD{}^FS\n",
            zpl_text(text)
        ));
        y += size + 10;
    };
    line("SHIP FROM", 24);
    line(ship_from.as_deref().unwrap_or_default(), 30);
    line("SHIP TO", 24);
    for l in &ship_to {
        line(l, 30);
    }
    line(
        &format!("CARRIER {}", carrier.as_deref().unwrap_or_default()),
        30,
    );
    if let Some(bol) = &bill_of_lading {
        line(&format!("BOL {bol}"), 30);
    }
    line(&format!("PO {}", sent.order_number), 40);
    line(&format!("{} {number} OF {of}", kind.to_uppercase()), 30);
    // The SSCC as GS1-128: start in code set C (>;), FNC1 (>8), then the
    // application identifier 00 and the eighteen digits.
    zpl.push_str(&format!(
        "^FO60,{}^BY3^BCN,200,N,N,N^FD>;>800{sscc}^FS\n",
        y + 20
    ));
    zpl.push_str(&format!(
        "^FO100,{}^A0N,36,36^FD(00) {sscc}^FS\n^XZ\n",
        y + 240
    ));

    Ok(Label {
        pack_id,
        kind,
        element_string: format!("(00){sscc}"),
        sscc,
        ship_from,
        ship_to,
        carrier,
        bill_of_lading,
        order_number: sent.order_number,
        number,
        of,
        zpl,
    })
}

//! Where a warehouse sits, for what a posting line is about.
//!
//! Warehouses form a tree: a site holds warehouses, a warehouse holds
//! locations or bins. The general ledger takes a line's default dimension
//! values (a site's, a warehouse's) from what the line is about, most
//! specific first. A module that moves stock gives it the movement's
//! [`Locations::scopes`]: the warehouse, the locations above it, its site,
//! then the item.

use std::collections::HashMap;

use erpcraft_kernel::{PgConnection, Result};
use uuid::Uuid;

/// The places of some warehouses, read once for a posting.
#[derive(Debug, Default, Clone)]
pub struct Locations {
    chains: HashMap<Uuid, Vec<(&'static str, Uuid)>>,
}

impl Locations {
    /// What a movement of `item_id` in `warehouse_id` is about, most
    /// specific first: the warehouse, each location it is part of upwards,
    /// its site, then the item.
    pub fn scopes(&self, warehouse_id: Uuid, item_id: Option<Uuid>) -> Vec<(&'static str, Uuid)> {
        let mut scopes = self
            .chains
            .get(&warehouse_id)
            .cloned()
            .unwrap_or_else(|| vec![("warehouse", warehouse_id)]);
        if let Some(item) = item_id {
            scopes.push(("item", item));
        }
        scopes
    }

    /// The site of a warehouse, if it has one.
    pub fn site(&self, warehouse_id: Uuid) -> Option<Uuid> {
        self.chains.get(&warehouse_id).and_then(|chain| {
            chain
                .iter()
                .find(|(scope, _)| *scope == "site")
                .map(|(_, id)| *id)
        })
    }
}

/// The places of these warehouses.
pub async fn locations(
    conn: &mut PgConnection,
    warehouses: impl IntoIterator<Item = Uuid>,
) -> Result<Locations> {
    let mut wanted: Vec<Uuid> = warehouses.into_iter().collect();
    wanted.sort_unstable();
    wanted.dedup();
    // Each warehouse with the locations above it, nearest first, and the
    // site of the warehouse itself.
    let rows: Vec<(Uuid, Vec<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "WITH RECURSIVE up AS (
             SELECT w.id AS start, w.id, w.parent_id, 0 AS depth
             FROM warehouse w WHERE w.id = ANY ($1)
             UNION ALL
             SELECT up.start, p.id, p.parent_id, up.depth + 1
             FROM up JOIN warehouse p ON p.id = up.parent_id
             WHERE up.depth < 32
         )
         SELECT up.start, array_agg(up.id ORDER BY up.depth), w.site_id
         FROM up JOIN warehouse w ON w.id = up.start
         GROUP BY up.start, w.site_id",
    )
    .bind(&wanted)
    .fetch_all(&mut *conn)
    .await?;
    let chains = rows
        .into_iter()
        .map(|(start, chain, site)| {
            let mut scopes: Vec<(&'static str, Uuid)> =
                chain.into_iter().map(|id| ("warehouse", id)).collect();
            scopes.extend(site.map(|site| ("site", site)));
            (start, scopes)
        })
        .collect();
    Ok(Locations { chains })
}

/// What each document line of these entries is about, for
/// `erpcraft_gl::add_scopes`: the place and item of the line's first
/// entry. A line whose entries are in two warehouses (a transfer) needs
/// its scopes per ledger line instead.
pub async fn line_scopes(
    conn: &mut PgConnection,
    entries: &[crate::StockEntry],
) -> Result<HashMap<Uuid, Vec<(&'static str, Uuid)>>> {
    let places = locations(conn, entries.iter().map(|e| e.warehouse_id)).await?;
    let mut by_line = HashMap::new();
    for entry in entries {
        if let Some(line) = entry.source_line_id {
            by_line
                .entry(line)
                .or_insert_with(|| places.scopes(entry.warehouse_id, Some(entry.item_id)));
        }
    }
    Ok(by_line)
}

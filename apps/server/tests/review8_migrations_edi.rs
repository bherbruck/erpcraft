//! Review 8: the EDI upgrades (0003, 0004) on rows written under 0002, as
//! a real installation has them.

mod common;

use common::upgrade::Upgrade;

const BASE: &str = "
INSERT INTO company (id, code, name, base_currency)
    VALUES ('00000000-0000-0000-0000-0000000000c1', 'C1', 'One', 'USD');
INSERT INTO customer (id, code, name)
    VALUES ('00000000-0000-0000-0000-0000000000a1', 'CUST', 'Customer');
INSERT INTO party_address (id, party_type, party_id, kind, country_code)
    VALUES ('00000000-0000-0000-0000-0000000000d1', 'customer',
            '00000000-0000-0000-0000-0000000000a1', 'shipping', 'US');
INSERT INTO edi_partner (id, company_id, code, name, customer_id, our_qualifier, our_id,
                         their_qualifier, their_id)
    VALUES ('00000000-0000-0000-0000-0000000000e1', '00000000-0000-0000-0000-0000000000c1',
            'P', 'Partner', '00000000-0000-0000-0000-0000000000a1', 'ZZ', 'US', 'ZZ', 'THEM');
INSERT INTO edi_location_xref (id, partner_id, qualifier, code, address_id)
    VALUES ('00000000-0000-0000-0000-0000000000f1', '00000000-0000-0000-0000-0000000000e1',
            '92', 'STORE1', '00000000-0000-0000-0000-0000000000d1');
";

const STORE_ADDRESS: &str =
    "SELECT count(*) FROM edi_location_xref WHERE code = 'STORE1' AND address_id IS NOT NULL";

/// A store number saved for an address the customer later dropped (a
/// customer save whose list leaves an address out deletes it). The upgrade
/// keeps the store and clears its address.
#[tokio::test]
async fn a_location_whose_address_was_removed_survives_the_upgrade() {
    let u = Upgrade::at(&[("edi", 2)]).await;
    u.sql(BASE).await;
    u.sql("DELETE FROM party_address WHERE id = '00000000-0000-0000-0000-0000000000d1'")
        .await;
    let upgraded = u.upgrade().await;
    let stores = u.count("SELECT count(*) FROM edi_location_xref").await;
    let addressed = u.count(STORE_ADDRESS).await;
    u.stop().await;
    upgraded.unwrap();
    assert_eq!((stores, addressed), (1, 0));
}

/// Same, with the partner re-pointed at another customer after the store
/// number was saved.
#[tokio::test]
async fn a_location_of_a_former_customer_survives_the_upgrade() {
    let u = Upgrade::at(&[("edi", 2)]).await;
    u.sql(BASE).await;
    u.sql(
        "INSERT INTO customer (id, code, name)
             VALUES ('00000000-0000-0000-0000-0000000000a2', 'CUST2', 'Other');
         UPDATE edi_partner SET customer_id = '00000000-0000-0000-0000-0000000000a2';",
    )
    .await;
    let upgraded = u.upgrade().await;
    let stores = u.count("SELECT count(*) FROM edi_location_xref").await;
    let addressed = u.count(STORE_ADDRESS).await;
    u.stop().await;
    upgraded.unwrap();
    assert_eq!((stores, addressed), (1, 0));
}

/// After the upgrade, a location never points at an address that is gone
/// or at one of a former customer.
#[tokio::test]
async fn after_the_upgrade_a_location_loses_an_address_that_goes() {
    let u = Upgrade::at(&[("edi", 2)]).await;
    u.sql(BASE).await;
    u.upgrade().await.unwrap();
    assert_eq!(u.count(STORE_ADDRESS).await, 1);
    u.sql("DELETE FROM party_address WHERE id = '00000000-0000-0000-0000-0000000000d1'")
        .await;
    assert_eq!(u.count(STORE_ADDRESS).await, 0);

    u.sql(
        "INSERT INTO party_address (id, party_type, party_id, kind, country_code)
             VALUES ('00000000-0000-0000-0000-0000000000d2', 'customer',
                     '00000000-0000-0000-0000-0000000000a1', 'shipping', 'US');
         UPDATE edi_location_xref SET address_id = '00000000-0000-0000-0000-0000000000d2';
         INSERT INTO customer (id, code, name)
             VALUES ('00000000-0000-0000-0000-0000000000a2', 'CUST2', 'Other');",
    )
    .await;
    assert_eq!(u.count(STORE_ADDRESS).await, 1);
    u.sql("UPDATE edi_partner SET customer_id = '00000000-0000-0000-0000-0000000000a2'")
        .await;
    assert_eq!(u.count(STORE_ADDRESS).await, 0);
    u.stop().await;
}

/// A GTIN for every partner, saved with the items before any partner
/// existed, is kept: every company gets it.
#[tokio::test]
async fn a_gtin_for_every_partner_is_kept_without_partners() {
    let u = Upgrade::at(&[("edi", 2)]).await;
    u.sql(
        "INSERT INTO company (id, code, name, base_currency)
             VALUES ('00000000-0000-0000-0000-0000000000c1', 'C1', 'One', 'USD');
         INSERT INTO item (id, code, name) VALUES ('00000000-0000-0000-0000-0000000000b1', 'I', 'Item');
         INSERT INTO edi_item_xref (id, partner_id, item_id, kind, value)
             VALUES (gen_random_uuid(), NULL, '00000000-0000-0000-0000-0000000000b1', 'gtin',
                     '00012345678905');
         INSERT INTO edi_uom_xref (id, partner_id, partner_uom, uom)
             VALUES (gen_random_uuid(), NULL, 'CA', 'case');",
    )
    .await;
    let upgraded = u.upgrade().await;
    let items = u
        .count(
            "SELECT count(*) FROM edi_item_xref
             WHERE company_id = '00000000-0000-0000-0000-0000000000c1' AND partner_id IS NULL",
        )
        .await;
    let units = u.count("SELECT count(*) FROM edi_uom_xref").await;
    u.stop().await;
    upgraded.unwrap();
    assert_eq!((items, units), (1, 1));
}

/// A GTIN for every partner beside one partner's own for the same item,
/// with two companies: each company gets the shared one, the partner's
/// stays its company's alone.
#[tokio::test]
async fn shared_and_partner_gtins_are_copied_per_company() {
    let u = Upgrade::at(&[("edi", 2)]).await;
    u.sql(BASE).await;
    u.sql(
        "INSERT INTO company (id, code, name, base_currency)
             VALUES ('00000000-0000-0000-0000-0000000000c2', 'C2', 'Two', 'USD');
         INSERT INTO item (id, code, name) VALUES ('00000000-0000-0000-0000-0000000000b1', 'I', 'Item');
         INSERT INTO edi_item_xref (id, partner_id, item_id, kind, value)
             VALUES (gen_random_uuid(), NULL, '00000000-0000-0000-0000-0000000000b1', 'gtin',
                     '00012345678905'),
                    (gen_random_uuid(), '00000000-0000-0000-0000-0000000000e1',
                     '00000000-0000-0000-0000-0000000000b1', 'gtin', '00012345678905');
         INSERT INTO edi_uom_xref (id, partner_id, partner_uom, uom)
             VALUES (gen_random_uuid(), NULL, 'CA', 'case');",
    )
    .await;
    let upgraded = u.upgrade().await;
    let shared = u
        .count("SELECT count(DISTINCT company_id) FROM edi_item_xref WHERE partner_id IS NULL")
        .await;
    let partners = u
        .count(
            "SELECT count(*) FROM edi_item_xref
             WHERE partner_id IS NOT NULL AND company_id = '00000000-0000-0000-0000-0000000000c1'",
        )
        .await;
    let total = u.count("SELECT count(*) FROM edi_item_xref").await;
    let units = u.count("SELECT count(*) FROM edi_uom_xref").await;
    u.stop().await;
    upgraded.unwrap();
    assert_eq!((shared, partners, total, units), (2, 1, 3, 2));
}

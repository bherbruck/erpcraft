//! Review 9, operations: what `--check-migrations` and the newer-database
//! refusal compare.

mod common;

use common::TestApp;

/// The database records `edi 3` as a different migration than the binary's
/// `edi 3` (a migration edited in place, as 5df884f and 4f7dbd5 did to
/// edi 0003, or two branches that both numbered a stock migration 13).
/// Status compares only (module, version): the database is reported up to
/// date and the binary's version of the script never runs.
#[tokio::test]
async fn a_recorded_migration_with_another_name_is_not_current() {
    let t = TestApp::start().await;
    sqlx::query(
        "UPDATE erpcraft_migration SET name = 'something_else_entirely'
         WHERE module = 'edi' AND version = 3",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let status = t.kernel.migration_status().await.unwrap();
    let current = status.is_current();
    t.stop().await;
    assert!(
        !current,
        "edi 3 is recorded as another migration than this binary's edi 3, yet the database \
         is reported up to date"
    );
}

/// collections 0002 checks a dunning level's fee against the currency's
/// decimals on every update, as banking 5 checked the IBAN; banking 6 had
/// to undo that for IBANs. A level saved before 0002 with a fee of 2.505
/// (numeric(20, 4) took it) is left as it is by the upgrade, and then no
/// other field of it can change: renaming it is refused for its fee.
#[tokio::test]
async fn a_dunning_level_from_before_collections_2_can_still_be_edited() {
    let u = common::upgrade::Upgrade::at(&[("collections", 1)]).await;
    u.sql(
        "INSERT INTO company (id, code, name, base_currency)
             VALUES ('00000000-0000-0000-0000-0000000000c1', 'OLD', 'Old Inc', 'USD');
         INSERT INTO account (id, company_id, code, name, account_type)
             VALUES ('00000000-0000-0000-0000-0000000000a1', '00000000-0000-0000-0000-0000000000c1',
                     '4900', 'Dunning fees', 'income');
         INSERT INTO dunning_level (id, company_id, level, name, days_overdue, fee, fee_account_id)
             VALUES ('00000000-0000-0000-0000-0000000000d1', '00000000-0000-0000-0000-0000000000c1',
                     1, 'Reminder', 14, 2.505, '00000000-0000-0000-0000-0000000000a1');",
    )
    .await;
    u.upgrade().await.unwrap();
    let renamed = sqlx::raw_sql(
        "UPDATE dunning_level SET name = 'First reminder'
         WHERE id = '00000000-0000-0000-0000-0000000000d1'",
    )
    .execute(&u.pool)
    .await;
    u.stop().await;
    assert!(
        renamed.is_ok(),
        "renaming a dunning level kept from before collections 2: {renamed:?}"
    );
}

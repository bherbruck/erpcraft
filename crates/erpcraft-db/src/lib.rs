//! Postgres pool and module-scoped migrations.

use erpcraft_core::Result;
use sqlx::Connection;
use sqlx::postgres::PgPoolOptions;

pub use sqlx::{PgConnection, PgPool};

/// Arbitrary constant; serializes migration runs across server instances.
const MIGRATION_LOCK: i64 = 0x6572_7063_7261_6674;

pub async fn connect(url: &str) -> Result<PgPool> {
    Ok(PgPoolOptions::new()
        .max_connections(16)
        .connect(url)
        .await?)
}

/// One SQL script. Versions are per module and applied in ascending order.
#[derive(Debug, Clone)]
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// The migrations one module owns.
#[derive(Debug, Clone)]
pub struct MigrationSet {
    pub module: String,
    pub migrations: Vec<Migration>,
}

/// Apply every migration not yet recorded. Sets run in the order given, so a
/// module may reference tables of the modules registered before it. Returns
/// the number of migrations applied.
pub async fn migrate(pool: &PgPool, sets: &[MigrationSet]) -> Result<usize> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *conn)
        .await?;
    let result = run(&mut conn, sets).await;
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK)
        .execute(&mut *conn)
        .await?;
    result
}

async fn run(conn: &mut PgConnection, sets: &[MigrationSet]) -> Result<usize> {
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS erpcraft_migration (
            module text NOT NULL,
            version integer NOT NULL,
            name text NOT NULL,
            applied_at timestamptz NOT NULL DEFAULT now(),
            PRIMARY KEY (module, version)
        )",
    )
    .execute(&mut *conn)
    .await?;

    let mut applied = 0;
    for set in sets {
        let mut migrations = set.migrations.clone();
        migrations.sort_by_key(|m| m.version);
        for migration in migrations {
            let done: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM erpcraft_migration WHERE module = $1 AND version = $2)",
            )
            .bind(&set.module)
            .bind(migration.version)
            .fetch_one(&mut *conn)
            .await?;
            if done {
                continue;
            }
            let mut tx = conn.begin().await?;
            sqlx::raw_sql(migration.sql).execute(&mut *tx).await?;
            sqlx::query(
                "INSERT INTO erpcraft_migration (module, version, name) VALUES ($1, $2, $3)",
            )
            .bind(&set.module)
            .bind(migration.version)
            .bind(migration.name)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!(module = %set.module, version = migration.version, name = migration.name, "applied migration");
            applied += 1;
        }
    }
    Ok(applied)
}

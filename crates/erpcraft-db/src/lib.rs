//! Postgres pool and module-scoped migrations.

use erpcraft_core::{Error, Result};
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

/// A migration as the database records it, or as the binary knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub module: String,
    pub version: i32,
    pub name: String,
}

/// Where the database stands against the binary's migrations.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// Known to the binary and not applied yet: applied by the next start
    /// (or `--migrate-only`).
    pub pending: Vec<Recorded>,
    /// Applied to the database and not known to the binary: the database
    /// was migrated by a newer binary (or one with other modules). Running
    /// against it is refused.
    pub unknown: Vec<Recorded>,
    /// Applied and known.
    pub applied: usize,
}

impl Status {
    pub fn is_current(&self) -> bool {
        self.pending.is_empty() && self.unknown.is_empty()
    }
}

/// Compare the migrations the database records with `sets`, changing
/// nothing. A database never migrated has every migration pending.
pub async fn status(pool: &PgPool, sets: &[MigrationSet]) -> Result<Status> {
    let mut conn = pool.acquire().await?;
    status_on(&mut conn, sets).await
}

async fn status_on(conn: &mut PgConnection, sets: &[MigrationSet]) -> Result<Status> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('erpcraft_migration') IS NOT NULL")
        .fetch_one(&mut *conn)
        .await?;
    let recorded: Vec<(String, i32, String)> = if exists {
        sqlx::query_as(
            "SELECT module, version, name FROM erpcraft_migration ORDER BY module, version",
        )
        .fetch_all(&mut *conn)
        .await?
    } else {
        vec![]
    };
    let known = |module: &str, version: i32| {
        sets.iter()
            .any(|s| s.module == module && s.migrations.iter().any(|m| m.version == version))
    };
    let mut status = Status::default();
    for (module, version, name) in &recorded {
        if known(module, *version) {
            status.applied += 1;
        } else {
            status.unknown.push(Recorded {
                module: module.clone(),
                version: *version,
                name: name.clone(),
            });
        }
    }
    for set in sets {
        let mut migrations = set.migrations.clone();
        migrations.sort_by_key(|m| m.version);
        for m in migrations {
            if !recorded
                .iter()
                .any(|(module, version, _)| *module == set.module && *version == m.version)
            {
                status.pending.push(Recorded {
                    module: set.module.clone(),
                    version: m.version,
                    name: m.name.to_owned(),
                });
            }
        }
    }
    Ok(status)
}

/// The refusal for a database that has migrations this binary does not know.
pub fn downgrade_error(unknown: &[Recorded]) -> Error {
    let listed: Vec<String> = unknown
        .iter()
        .take(10)
        .map(|r| format!("{} {} ({})", r.module, r.version, r.name))
        .collect();
    Error::state(format!(
        "the database has {} migration(s) this binary does not know: {}{}. It was migrated by a \
         newer erpcraft (or one with other modules); run that version, or restore a backup \
         taken before the upgrade. Running an older binary against it is refused.",
        unknown.len(),
        listed.join(", "),
        if unknown.len() > 10 { ", ..." } else { "" }
    ))
}

async fn run(conn: &mut PgConnection, sets: &[MigrationSet]) -> Result<usize> {
    // Never apply anything to a database a newer binary has migrated.
    let status = status_on(conn, sets).await?;
    if !status.unknown.is_empty() {
        return Err(downgrade_error(&status.unknown));
    }
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

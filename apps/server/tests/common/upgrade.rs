//! Testing a data migration on data written before it.
//!
//! A migration that moves or rewrites rows passes on an empty database and
//! fails on a real one. [`Upgrade::at`] gives a database migrated up to
//! some modules' older versions (everything else in full), the test writes
//! the rows a real installation would have under those versions, with
//! plain SQL as the code of the time wrote them, then [`Upgrade::upgrade`]
//! runs every migration that is left and the test looks at what came out.
//!
//! ```ignore
//! let u = Upgrade::at(&[("edi", 2)]).await;
//! u.sql("INSERT INTO edi_item_xref ...").await;
//! u.upgrade().await.unwrap();
//! assert_eq!(u.count("SELECT count(*) FROM edi_item_xref").await, 2);
//! u.stop().await;
//! ```
//!
//! A module held back must not be one that a module registered after it
//! needs at its newer version.

use std::str::FromStr;

use erpcraft_db::MigrationSet;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use uuid::Uuid;

pub struct Upgrade {
    pub pool: PgPool,
    root: PgPool,
    database: String,
    all: Vec<MigrationSet>,
}

impl Upgrade {
    /// A new database with every module's migrations, except that each
    /// module named stops at the version given.
    pub async fn at(held_back: &[(&str, i64)]) -> Upgrade {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| super::DEFAULT_URL.into());
        let options =
            PgConnectOptions::from_str(&url).expect("DATABASE_URL is not a valid Postgres URL");
        let root = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .expect("cannot reach Postgres; start it with `docker compose up -d`");
        let database = format!("erpcraft_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {database}"))
            .execute(&root)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.database(&database))
            .await
            .unwrap();
        let kernel = erpcraft_server::modules(pool.clone()).build().unwrap();
        let all = kernel.registry().migration_sets().to_vec();
        for (module, _) in held_back {
            assert!(
                all.iter().any(|set| set.module == *module),
                "no module {module} has migrations"
            );
        }
        let before: Vec<MigrationSet> = all
            .iter()
            .cloned()
            .map(|mut set| {
                if let Some((_, version)) = held_back.iter().find(|(m, _)| *m == set.module) {
                    set.migrations.retain(|m| i64::from(m.version) <= *version);
                }
                set
            })
            .collect();
        erpcraft_db::migrate(&pool, &before)
            .await
            .expect("migrating to the older versions failed");
        Upgrade {
            pool,
            root,
            database,
            all,
        }
    }

    /// Run statements as they are, several at once.
    pub async fn sql(&self, sql: &str) {
        sqlx::raw_sql(sql)
            .execute(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.pool)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// Every migration left, as a server that starts runs them.
    pub async fn upgrade(&self) -> Result<(), String> {
        erpcraft_db::migrate(&self.pool, &self.all)
            .await
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    pub async fn stop(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.root)
            .await
            .unwrap();
    }
}

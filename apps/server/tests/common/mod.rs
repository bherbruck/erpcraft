//! Test harness: each test gets its own database with all migrations
//! applied, an administrator, and the full router.

#![allow(dead_code)]

use std::str::FromStr;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use erpcraft_kernel::{Kernel, KernelBuilder};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

const DEFAULT_URL: &str = "postgres://erpcraft:erpcraft@127.0.0.1:54320/erpcraft";
pub const ADMIN_EMAIL: &str = "admin@example.com";
pub const PASSWORD: &str = "correct-horse-battery";

pub struct TestApp {
    pub app: Router,
    pub kernel: Kernel,
    pub pool: PgPool,
    /// Administrator token.
    pub admin: String,
    root: PgPool,
    database: String,
}

/// A company with a small chart of accounts and one open period for 2026.
pub struct Company {
    pub id: String,
    pub cash: String,
    pub revenue: String,
    pub group: String,
    pub period: String,
}

impl TestApp {
    pub async fn start() -> TestApp {
        TestApp::start_with(|builder| builder).await
    }

    /// Like [`TestApp::start`], with extra modules registered after the
    /// in-tree ones.
    pub async fn start_with(extend: impl FnOnce(KernelBuilder) -> KernelBuilder) -> TestApp {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_URL.into());
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
            .max_connections(12)
            .connect_with(options.database(&database))
            .await
            .unwrap();
        let kernel = extend(erpcraft_server::modules(pool.clone()))
            .build()
            .unwrap();
        kernel.migrate().await.unwrap();
        assert!(
            erpcraft_auth::bootstrap_admin(&kernel, ADMIN_EMAIL, PASSWORD)
                .await
                .unwrap()
        );
        let admin = erpcraft_auth::login(&kernel, ADMIN_EMAIL, PASSWORD)
            .await
            .unwrap()
            .token;

        TestApp {
            app: erpcraft_server::app(kernel.clone()),
            kernel,
            pool,
            admin,
            root,
            database,
        }
    }

    pub async fn stop(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE {} WITH (FORCE)", self.database))
            .execute(&self.root)
            .await
            .unwrap();
    }

    pub async fn send(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let body = match body {
            Some(value) => Body::from(value.to_string()),
            None => Body::empty(),
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)))
        };
        (status, value)
    }

    pub async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send("GET", path, Some(token), None, &[]).await
    }

    pub async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send("POST", path, Some(token), Some(body), &[]).await
    }

    pub async fn patch(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send("PATCH", path, Some(token), Some(body), &[]).await
    }

    /// Run a command over HTTP.
    pub async fn command(&self, token: &str, name: &str, input: Value) -> (StatusCode, Value) {
        self.post(token, &format!("/api/commands/{name}"), input)
            .await
    }

    /// Send one MCP request and return its `result`.
    pub async fn mcp(&self, token: &str, method: &str, params: Value) -> Value {
        let (status, body) = self
            .post(
                token,
                "/mcp",
                json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.get("error").is_none(), "{body}");
        body["result"].clone()
    }

    /// Every tool `tools/list` shows the caller, following `nextCursor`
    /// through all its pages.
    pub async fn all_tools(&self, token: &str) -> Vec<Value> {
        let mut tools = vec![];
        let mut cursor = Value::Null;
        loop {
            let page = self
                .mcp(token, "tools/list", json!({ "cursor": cursor }))
                .await;
            tools.extend(page["tools"].as_array().unwrap().iter().cloned());
            match page.get("nextCursor") {
                Some(next) => cursor = next.clone(),
                None => return tools,
            }
        }
    }

    /// Call an MCP tool and return the whole tool result.
    pub async fn tool(&self, token: &str, name: &str, arguments: Value) -> Value {
        self.mcp(
            token,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    /// Create a record as the administrator and return its id.
    pub async fn create(&self, entity: &str, data: Value) -> String {
        let (status, body) = self
            .post(&self.admin, &format!("/api/entities/{entity}"), data)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_owned()
    }

    pub async fn company(&self, code: &str, currency: &str) -> Company {
        let id = self
            .create(
                "company",
                json!({ "code": code, "name": format!("{code} Inc"), "base_currency": currency }),
            )
            .await;
        let account = |code: &'static str, name: &'static str, kind: &'static str, group: bool| {
            let company = id.clone();
            async move {
                self.create(
                    "account",
                    json!({ "company_id": company, "code": code, "name": name, "account_type": kind,
                            "is_group": group, "is_bank": code == "1000" }),
                )
                .await
            }
        };
        let group = account("1", "Assets", "asset", true).await;
        // Cash is where payments go through.
        let cash = account("1000", "Cash", "asset", false).await;
        let revenue = account("4000", "Revenue", "income", false).await;
        let period = self
            .create(
                "fiscal_period",
                json!({ "company_id": id, "name": "2026", "start_date": "2026-01-01", "end_date": "2026-12-31" }),
            )
            .await;
        Company {
            id,
            cash,
            revenue,
            group,
            period,
        }
    }

    /// Create a balanced draft journal entry: debit cash, credit revenue.
    pub async fn draft(&self, token: &str, company: &Company, amount: &str) -> Value {
        let (status, body) = self
            .post(
                token,
                "/api/documents/journal_entry",
                json!({
                    "company_id": company.id,
                    "doc_date": "2026-03-15",
                    "memo": "Cash sale",
                    "body": { "lines": [
                        { "account_id": company.cash, "debit": amount },
                        { "account_id": company.revenue, "credit": amount },
                    ]},
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    /// Create a user holding one role for one company; returns their token.
    pub async fn user_with_role(
        &self,
        email: &str,
        permissions: &[&str],
        company_id: &str,
    ) -> String {
        let (status, user) = self
            .command(
                &self.admin,
                "user.create",
                json!({ "email": email, "name": "Test User", "password": PASSWORD }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{user}");
        let role = self
            .create(
                "role",
                json!({ "name": format!("role for {email}"), "permissions": permissions }),
            )
            .await;
        let (status, body) = self
            .command(
                &self.admin,
                "user.assign_role",
                json!({ "user_id": user["id"], "role_id": role, "company_id": company_id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        erpcraft_auth::login(&self.kernel, email, PASSWORD)
            .await
            .unwrap()
            .token
    }

    /// Create a user holding one role made of these permission rules, for
    /// one company or, with `None`, for all. Returns their token and id.
    pub async fn user_with_rules(
        &self,
        email: &str,
        rules: Value,
        company_id: Option<&str>,
    ) -> (String, String) {
        let (status, user) = self
            .command(
                &self.admin,
                "user.create",
                json!({ "email": email, "name": "Test User", "password": PASSWORD }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{user}");
        let role = self
            .create(
                "role",
                json!({ "name": format!("role for {email}"), "permissions": rules }),
            )
            .await;
        let (status, body) = self
            .command(
                &self.admin,
                "user.assign_role",
                json!({ "user_id": user["id"], "role_id": role, "company_id": company_id }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = erpcraft_auth::login(&self.kernel, email, PASSWORD)
            .await
            .unwrap()
            .token;
        (token, user["id"].as_str().unwrap().to_owned())
    }

    pub async fn set_attributes(&self, user_id: &str, attributes: Value) {
        let (status, body) = self
            .command(
                &self.admin,
                "user.set_attributes",
                json!({ "user_id": user_id, "attributes": attributes }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// Say which account carries the value of a company's stock: the
    /// default account rule for the purpose `inventory`.
    pub async fn inventory_rule(&self, company_id: &str, account_id: &str) -> String {
        self.create(
            "account_rule",
            json!({ "company_id": company_id, "purpose": "inventory", "account_id": account_id }),
        )
        .await
    }

    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(&self.pool).await.unwrap()
    }
}

/// Read a decimal that the API returned as a string.
pub fn dec(value: &Value) -> f64 {
    value
        .as_str()
        .unwrap_or_else(|| panic!("expected a decimal string, got {value}"))
        .parse()
        .unwrap()
}

pub fn id(value: &Value) -> String {
    value["id"].as_str().unwrap().to_owned()
}

//! The erpcraft server, and what an operator runs beside it.
//!
//! ```text
//! erpcraft                     migrate the database, then serve
//! erpcraft --migrate-only      migrate the database and exit
//! erpcraft --check-migrations  say where the database stands; change nothing
//! erpcraft verify [--company CODE] [--json]
//!                              check that the books agree (after a restore)
//! erpcraft healthcheck         ask the running server's /healthz (for Docker)
//! ```
//!
//! Every mode reads `DATABASE_URL`; serving and the health check read
//! `ERPCRAFT_BIND`. A database migrated by a newer binary is refused.

use std::env;
use std::error::Error;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: erpcraft [--migrate-only | --check-migrations]
       erpcraft verify [--company CODE] [--json]
       erpcraft healthcheck
       erpcraft --version";

/// Exit codes of `--check-migrations`.
const PENDING: u8 = 10;
const DOWNGRADE: u8 = 20;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => serve().await.map(|_| ExitCode::SUCCESS),
        Some("--migrate-only" | "migrate") => migrate_only().await,
        Some("--check-migrations") => check_migrations().await,
        Some("verify") => verify(&args[1..]).await,
        Some("healthcheck") => healthcheck().await,
        Some("--version" | "-V") => {
            println!("erpcraft {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Some("--help" | "-h") => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(other) => {
            eprintln!("unknown argument {other}\n{USAGE}");
            Ok(ExitCode::from(2))
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("erpcraft: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn kernel() -> Result<erpcraft_kernel::Kernel, Box<dyn Error>> {
    let database_url = env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set")?;
    let pool = erpcraft_db::connect(&database_url).await?;
    Ok(erpcraft_server::kernel(pool)?)
}

fn bind() -> String {
    env::var("ERPCRAFT_BIND").unwrap_or_else(|_| "127.0.0.1:8600".into())
}

async fn migrate_only() -> Result<ExitCode, Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let kernel = kernel().await?;
    let applied = kernel.migrate().await?;
    println!("applied {applied} migration(s); the database is up to date");
    Ok(ExitCode::SUCCESS)
}

async fn check_migrations() -> Result<ExitCode, Box<dyn Error>> {
    let kernel = kernel().await?;
    let status = kernel.migration_status().await?;
    println!("{} migration(s) applied", status.applied);
    for m in &status.pending {
        println!("pending: {} {} ({})", m.module, m.version, m.name);
    }
    for m in &status.unknown {
        println!(
            "unknown to this binary: {} {} ({})",
            m.module, m.version, m.name
        );
    }
    Ok(if !status.unknown.is_empty() {
        println!("the database was migrated by a newer erpcraft: this binary refuses to run");
        ExitCode::from(DOWNGRADE)
    } else if !status.pending.is_empty() {
        println!(
            "{} migration(s) will be applied when the server starts",
            status.pending.len()
        );
        ExitCode::from(PENDING)
    } else {
        println!("the database is up to date");
        ExitCode::SUCCESS
    })
}

async fn verify(args: &[String]) -> Result<ExitCode, Box<dyn Error>> {
    let mut company = None;
    let mut json = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--company" => company = Some(rest.next().ok_or("--company takes a code")?.clone()),
            "--json" => json = true,
            other => return Err(format!("unknown argument {other}\n{USAGE}").into()),
        }
    }
    let kernel = kernel().await?;
    let status = kernel.migration_status().await?;
    if !status.unknown.is_empty() {
        return Err(erpcraft_db::downgrade_error(&status.unknown).into());
    }
    if !status.pending.is_empty() {
        return Err(format!(
            "{} migration(s) are pending: migrate first (erpcraft --migrate-only)",
            status.pending.len()
        )
        .into());
    }
    let report = erpcraft_server::verify::verify(kernel.pool(), company.as_deref()).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.text());
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// GET /healthz on the address the server binds, without curl: for a
/// container image that has no shell.
async fn healthcheck() -> Result<ExitCode, Box<dyn Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let bind = bind();
    // A server bound to every address is asked on this one.
    let addr = bind
        .replace("0.0.0.0", "127.0.0.1")
        .replace("[::]", "[::1]");
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(&addr),
    )
    .await??;
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await?;
    let mut answer = vec![];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut answer),
    )
    .await??;
    let answer = String::from_utf8_lossy(&answer);
    if answer.starts_with("HTTP/1.1 200") {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!(
            "erpcraft: unhealthy: {}",
            answer.lines().next().unwrap_or("")
        );
        Ok(ExitCode::FAILURE)
    }
}

async fn serve() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();
    let bind = bind();
    let kernel = kernel().await?;
    // Refused, with the reason, when a newer binary migrated the database.
    let applied = kernel.migrate().await?;
    tracing::info!(applied, "database is up to date");

    if let (Ok(email), Ok(password)) = (
        env::var("ERPCRAFT_ADMIN_EMAIL"),
        env::var("ERPCRAFT_ADMIN_PASSWORD"),
    ) && erpcraft_auth::bootstrap_admin(&kernel, &email, &password).await?
    {
        tracing::info!(%email, "created the first administrator");
    }

    let background = tokio::spawn(erpcraft_server::background(kernel.clone()));

    // The admin UI is served only from a directory that holds a build.
    let ui_dir = match env::var_os("ERPCRAFT_UI_DIR").filter(|dir| !dir.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.join("index.html").is_file() {
                tracing::info!(dir = %dir.display(), "serving the admin UI");
                Some(dir)
            } else {
                tracing::warn!(
                    dir = %dir.display(),
                    "ERPCRAFT_UI_DIR has no index.html; the admin UI is not served"
                );
                None
            }
        }
        None => {
            tracing::info!("ERPCRAFT_UI_DIR is not set; the admin UI is not served");
            None
        }
    };

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "erpcraft is listening");
    axum::serve(
        listener,
        erpcraft_server::app_with_ui(kernel, ui_dir.as_deref()),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    background.abort();
    Ok(())
}

/// Ctrl-C, or SIGTERM as `docker stop` sends it.
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match term.as_mut() {
                    Some(t) => { t.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

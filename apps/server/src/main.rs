use std::env;
use std::error::Error;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let database_url = env::var("DATABASE_URL").map_err(|_| "DATABASE_URL is not set")?;
    let bind = env::var("ERPCRAFT_BIND").unwrap_or_else(|_| "127.0.0.1:8600".into());

    let pool = erpcraft_db::connect(&database_url).await?;
    let kernel = erpcraft_server::kernel(pool)?;
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
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    background.abort();
    Ok(())
}

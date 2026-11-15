use std::env;
use std::error::Error;

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
    ) {
        if erpcraft_auth::bootstrap_admin(&kernel, &email, &password).await? {
            tracing::info!(%email, "created the first administrator");
        }
    }

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "erpcraft is listening");
    axum::serve(listener, erpcraft_server::app(kernel))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

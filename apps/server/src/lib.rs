//! Assembles the reference ERP: the kernel plus the in-tree modules.

use std::time::Duration;

use axum::Router;
use erpcraft_kernel::maintenance::{self, Retention};
use erpcraft_kernel::{Kernel, KernelBuilder, PgPool, Result, outbox};
use tower_http::trace::TraceLayer;

/// A kernel builder holding every in-tree module, in dependency order.
/// An installation adds its own modules after these.
pub fn modules(pool: PgPool) -> KernelBuilder {
    Kernel::builder(pool)
        .module(erpcraft_auth::AuthModule)
        .module(erpcraft_gl::GlModule)
        .module(erpcraft_accounting::AccountingModule)
}

/// The kernel with every in-tree module.
pub fn kernel(pool: PgPool) -> Result<Kernel> {
    modules(pool).build()
}

/// How many events one pass over the outbox takes.
const OUTBOX_BATCH: i64 = 100;
/// How long to wait when the outbox is empty.
const OUTBOX_IDLE: Duration = Duration::from_secs(1);
/// How often expired idempotency keys and old events are removed.
const CLEAN_EVERY: Duration = Duration::from_secs(3600);

/// The work a server does besides answering requests: delivering outbox
/// events and cleaning up. Runs until the task is dropped.
pub async fn background(kernel: Kernel) {
    let mut last_clean: Option<tokio::time::Instant> = None;
    loop {
        // A pass runs as its own task, so a subscriber that panics costs
        // one pass and not the worker.
        let pass = tokio::spawn({
            let kernel = kernel.clone();
            async move { outbox::deliver_batch(&kernel, OUTBOX_BATCH).await }
        });
        let busy = match pass.await {
            Ok(Ok(delivered)) => delivered.total() as i64 == OUTBOX_BATCH,
            Ok(Err(error)) => {
                tracing::error!(%error, "outbox delivery pass failed");
                false
            }
            Err(error) => {
                tracing::error!(%error, "outbox delivery pass panicked");
                false
            }
        };

        if last_clean.is_none_or(|at| at.elapsed() >= CLEAN_EVERY) {
            last_clean = Some(tokio::time::Instant::now());
            match maintenance::clean(&kernel, Retention::default()).await {
                Ok(cleaned) => tracing::debug!(?cleaned, "cleaned up"),
                Err(error) => tracing::error!(%error, "cleanup failed"),
            }
        }
        if !busy {
            tokio::time::sleep(OUTBOX_IDLE).await;
        }
    }
}

/// The HTTP API, the sign-in routes and the MCP endpoint on one router.
pub fn app(kernel: Kernel) -> Router {
    erpcraft_api::router(kernel.clone())
        .merge(erpcraft_auth::router(kernel.clone()))
        .merge(erpcraft_mcp::router(kernel))
        .layer(TraceLayer::new_for_http())
}

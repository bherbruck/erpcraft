//! Assembles the reference ERP: the kernel plus the in-tree modules.

use std::path::Path;
use std::time::Duration;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use erpcraft_kernel::maintenance::{self, Retention};
use erpcraft_kernel::{Kernel, KernelBuilder, PgPool, Result, outbox};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::trace::TraceLayer;

mod ui;

/// A kernel builder holding every in-tree module, in dependency order.
/// An installation adds its own modules after these.
pub fn modules(pool: PgPool) -> KernelBuilder {
    Kernel::builder(pool)
        .module(erpcraft_auth::AuthModule)
        .module(erpcraft_gl::GlModule)
        .module(erpcraft_accounting::AccountingModule)
        .module(erpcraft_stock::StockModule)
        .module(erpcraft_inventory::InventoryModule)
        .module(erpcraft_openitems::OpenItemsModule)
        .module(erpcraft_tax::TaxModule)
        .module(erpcraft_purchasing::PurchasingModule)
        .module(erpcraft_sales::SalesModule)
        .module(erpcraft_payments::PaymentsModule)
        .module(erpcraft_manufacturing::ManufacturingModule)
        .module(erpcraft_webhooks::WebhooksModule::from_env())
        .module(erpcraft_printing::PrintingModule)
        .module(erpcraft_setup::SetupModule)
        .module(erpcraft_banking::BankingModule)
        .module(erpcraft_assets::AssetsModule)
        .module(erpcraft_party::PartyModule)
        .module(erpcraft_einvoice::EinvoiceModule)
        .module(erpcraft_planning::PlanningModule)
        .module(erpcraft_intercompany::IntercompanyModule)
        .module(erpcraft_mail::MailModule::from_env())
        .module(erpcraft_payment_runs::PaymentRunsModule)
        .module(erpcraft_edi::EdiModule)
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
    // Documents written before their type kept a summary get one, a batch
    // per pass, until none remain or a batch makes no progress.
    let mut summaries = Summaries::Missing(None);
    loop {
        if let Summaries::Missing(before) = summaries {
            summaries = refresh_summaries(&kernel, before).await;
        }
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

/// Documents whose summary one background pass works out.
const SUMMARY_BATCH: i64 = 200;

/// Where the background worker is with missing document summaries.
#[derive(Clone, Copy)]
enum Summaries {
    /// Some may be missing; how many remained after the last batch.
    Missing(Option<i64>),
    Done,
}

/// One batch of missing document summaries. A batch that leaves as many as
/// the one before (a type whose summary comes out empty) ends the work.
async fn refresh_summaries(kernel: &Kernel, before: Option<i64>) -> Summaries {
    let stalled = |remaining: i64| before.is_some_and(|before| remaining >= before);
    match erpcraft_kernel::document::refresh_missing_summaries_batch(kernel, SUMMARY_BATCH).await {
        Ok(done) if done.refreshed == 0 || stalled(done.remaining) => {
            if done.remaining > 0 {
                tracing::warn!(
                    remaining = done.remaining,
                    "document summaries are missing that could not be worked out now"
                );
            }
            Summaries::Done
        }
        Ok(done) => {
            tracing::info!(
                refreshed = done.refreshed,
                remaining = done.remaining,
                "worked out missing document summaries"
            );
            if done.remaining > 0 {
                Summaries::Missing(Some(done.remaining))
            } else {
                Summaries::Done
            }
        }
        Err(error) => {
            tracing::error!(%error, "working out missing document summaries failed");
            Summaries::Done
        }
    }
}

/// The HTTP API, the sign-in routes and the MCP endpoint on one router.
pub fn app(kernel: Kernel) -> Router {
    app_with_ui(kernel, None)
}

/// Like [`app`], and when `ui_dir` is given also the admin UI: the
/// directory holds its built files (`ui/dist`), `index.html` among them.
/// Without it, `/` is the JSON index and unknown paths are JSON errors.
pub fn app_with_ui(kernel: Kernel, ui_dir: Option<&Path>) -> Router {
    let api = match ui_dir {
        Some(dir) => ui::serve(erpcraft_api::routes(kernel.clone()), dir),
        None => erpcraft_api::router(kernel.clone()),
    };
    catch_panics(
        api.merge(erpcraft_auth::router(kernel.clone()))
            .merge(erpcraft_mcp::router(kernel))
            .layer(TraceLayer::new_for_http()),
    )
}

/// `router`, with a panic in any handler answered as the API answers any
/// internal error, a JSON 500 whose detail stays in the log, instead of a
/// dropped connection. The outermost layer, so it also covers the others.
pub fn catch_panics(router: Router) -> Router {
    router.layer(CatchPanicLayer::custom(panic_response))
}

fn panic_response(panic: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let detail = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("a panic without a message");
    tracing::error!(detail, "a request handler panicked");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":{"code":"internal","message":"internal error"}}"#,
    )
        .into_response()
}

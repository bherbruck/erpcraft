//! Assembles the reference ERP: the kernel plus the in-tree modules.

use axum::Router;
use erpcraft_kernel::{Kernel, PgPool, Result};
use tower_http::trace::TraceLayer;

/// The kernel with every in-tree module, in dependency order.
pub fn kernel(pool: PgPool) -> Result<Kernel> {
    Kernel::builder(pool)
        .module(erpcraft_auth::AuthModule)
        .module(erpcraft_gl::GlModule)
        .module(erpcraft_accounting::AccountingModule)
        .build()
}

/// The HTTP API, the sign-in routes and the MCP endpoint on one router.
pub fn app(kernel: Kernel) -> Router {
    erpcraft_api::router(kernel.clone())
        .merge(erpcraft_auth::router(kernel.clone()))
        .merge(erpcraft_mcp::router(kernel))
        .layer(TraceLayer::new_for_http())
}

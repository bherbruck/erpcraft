//! Business analysis: what sold, to whom and at what margin; what was
//! bought; how old, slow or fast the stock is; and the key figures of the
//! business. Everything is read from what was posted. See "Business
//! analysis" in ARCHITECTURE.md.

use std::sync::Arc;

use erpcraft_kernel::{DynCommand, Module, command};

mod common;
mod kpis;
mod sales;
mod stock;

pub struct AnalysisModule;

impl Module for AnalysisModule {
    fn catalogues(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("de", include_str!("../i18n/de.json")),
            ("fr", include_str!("../i18n/fr.json")),
        ]
    }

    fn name(&self) -> &'static str {
        "analysis"
    }

    fn label(&self) -> String {
        "Business analysis".into()
    }

    fn depends_on(&self) -> Vec<&'static str> {
        vec![
            "gl",
            "stock",
            "sales",
            "purchasing",
            "pricing",
            "open_items",
            "tax",
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![
            command(sales::SalesAnalysis),
            command(sales::PurchaseAnalysis),
            command(sales::Top),
            command(stock::Aging),
            command(stock::SlowMovers),
            command(stock::Turnover),
            command(kpis::Kpis),
        ]
    }
}

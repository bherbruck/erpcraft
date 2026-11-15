//! The erpcraft kernel.
//!
//! A [`Kernel`] is a Postgres pool plus a [`Registry`] of everything the
//! installed modules declare: entities, document types, ledgers and
//! commands. Every change to business state runs as a command through
//! [`command::execute`], which supplies the transaction, authorization,
//! idempotency, audit record and outbox event.
//!
//! Compiled modules are trusted server code. The module API is an
//! architectural boundary, not a security sandbox.

pub mod authz;
mod builtin;
pub mod command;
pub mod document;
pub mod entity;
pub mod numbering;
pub mod registry;

use std::sync::Arc;

pub use async_trait::async_trait;
pub use erpcraft_core::{
    DateTime, Decimal, Error, NaiveDate, Result, Utc, Uuid, new_id, round_money,
};
pub use erpcraft_db::{Migration, PgConnection, PgPool};
pub use schemars::{self, JsonSchema};

pub use authz::{Actor, Authenticator};
pub use command::{Command, CommandSpec, Ctx, DynCommand, Options, command};
pub use document::{Document, DocumentHandler, DocumentView, Ledger, Posting};
pub use registry::{
    CollectionMeta, DocTypeMeta, EntityMeta, FieldMeta, FieldType, Module, Registry, StateMeta,
};

#[derive(Clone)]
pub struct Kernel {
    pool: PgPool,
    registry: Arc<Registry>,
}

impl Kernel {
    /// Start a kernel. The kernel's own module is always registered first.
    pub fn builder(pool: PgPool) -> KernelBuilder {
        KernelBuilder {
            pool,
            modules: vec![Box::new(builtin::KernelModule)],
        }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Resolve a credential to the actor it belongs to, using the
    /// authenticator a module registered.
    pub async fn authenticate(&self, token: &str) -> Result<Actor> {
        match self.registry.authenticator() {
            Some(authenticator) => authenticator.authenticate(self, token).await,
            None => Err(Error::internal("no module supplies an authenticator")),
        }
    }

    /// Apply every pending migration of every registered module.
    pub async fn migrate(&self) -> Result<usize> {
        erpcraft_db::migrate(&self.pool, self.registry.migration_sets()).await
    }
}

pub struct KernelBuilder {
    pool: PgPool,
    modules: Vec<Box<dyn Module>>,
}

impl KernelBuilder {
    /// Register a module. A module must come after the modules it depends on.
    pub fn module(mut self, module: impl Module) -> Self {
        self.modules.push(Box::new(module));
        self
    }

    pub fn build(self) -> Result<Kernel> {
        Ok(Kernel {
            pool: self.pool,
            registry: Arc::new(Registry::build(self.modules)?),
        })
    }
}

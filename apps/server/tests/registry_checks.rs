//! What the registry refuses when the server starts.

use std::sync::Arc;

use erpcraft_kernel::{
    Command, CommandSpec, Ctx, DynCommand, Module, PrimaryWhenSpec, Result, async_trait, command,
};
use serde_json::Value;

struct Odd;

impl Module for Odd {
    fn name(&self) -> &'static str {
        "odd"
    }

    fn commands(&self) -> Vec<Arc<dyn DynCommand>> {
        vec![command(Reclose)]
    }
}

/// A command on fiscal periods that applies to open ones but says it is
/// the primary action on closed ones.
struct Reclose;

#[async_trait]
impl Command for Reclose {
    type Input = Value;
    type Output = Value;

    fn spec(&self) -> CommandSpec {
        CommandSpec::action("fiscal_period.reclose", "Close an open period again.")
            .resource("fiscal_period")
            .from_states(&["open"])
            .primary_when(PrimaryWhenSpec::states(&["closed"]))
    }

    async fn run(&self, _ctx: &mut Ctx<'_>, input: Value) -> Result<Value> {
        Ok(input)
    }
}

/// Review 9: primary_when's states must be states the command applies from.
#[tokio::test]
async fn a_command_is_not_primary_in_a_state_it_does_not_apply_from() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    let error = erpcraft_server::modules(pool)
        .module(Odd)
        .build()
        .err()
        .expect("the registry took a primary state the command does not apply from");
    let message = error.to_string();
    assert!(
        message.contains("is primary in state 'closed', which it does not apply from"),
        "{message}"
    );
}

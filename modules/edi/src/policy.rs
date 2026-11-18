//! The business judgements EDI leaves to the company.

use erpcraft_kernel::PolicyPoint;

pub const AUTO_DRAFT: &str = "edi.auto_draft";
pub const DUPLICATE_ORDER: &str = "edi_order.duplicate";

pub fn points() -> Vec<PolicyPoint> {
    vec![
        PolicyPoint::new(
            AUTO_DRAFT,
            "Asked when a partner's order arrives with no blocking finding: is it drafted as a \
             sales order at once, by the partner's run-as user, without a person looking first? \
             Unless configured otherwise it is refused and the order waits as a proposal. A \
             company that trusts a partner allows it, for example for `facts.notes == 0 && \
             facts.total_net < 5000`. The draft is still checked and approved like any other.",
        )
        .text("partner_code", "The partner's code")
        .text("message", "The message, such as 850")
        .number("notes", "How many notes the proposal has")
        .number("lines", "How many lines the order has")
        .number(
            "total_net",
            "The order's lines at the partner's prices, without tax",
        )
        .as_money(None)
        .refuse_by_default()
        .refusal("orders from this partner are drafted by a person"),
        PolicyPoint::new(
            DUPLICATE_ORDER,
            "Asked when a partner's order carries an order number of that partner that was \
             drafted before: is it drafted again? Unless configured otherwise it is refused, \
             because a partner that resends an order would otherwise be sent the goods twice. A \
             partner that reuses its numbers is allowed by a rule, for example when \
             `facts.partner_code == 'P01'`.",
        )
        .text("partner_code", "The partner's code")
        .text("order_number", "The partner's order number")
        .number(
            "drafted_before",
            "How many sales orders were drafted for this number before",
        )
        .refuse_by_default()
        .refusal("this order number was drafted before"),
    ]
}

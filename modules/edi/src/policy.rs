//! The business judgements EDI leaves to the company.

use erpcraft_kernel::PolicyPoint;

pub const AUTO_DRAFT: &str = "edi.auto_draft";
pub const DUPLICATE_ORDER: &str = "edi_order.duplicate";
pub const PRICE_ZERO: &str = "edi_order.price_zero";
pub const AUTO_APPROVE: &str = "edi.auto_approve";
pub const SEND: &str = "edi.send";
pub const BEFORE_ASN: &str = "edi_invoice.before_asn";

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
        PolicyPoint::new(
            PRICE_ZERO,
            "Asked for each line of a partner's order priced at nothing: is it drafted free, \
             at a price of zero? Unless configured otherwise it is refused, and a person gives \
             the price meant (or 0, to supply it free). A partner that sends samples or \
             replacements at no charge is allowed by a rule, for example when \
             `facts.partner_code == 'P01'`; a rule may also ask for approval.",
        )
        .text("partner_code", "The partner's code")
        .text("line", "The partner's line id")
        .refuse_by_default()
        .refusal("a line priced at nothing needs a price, or 0 given for it"),
        PolicyPoint::new(
            AUTO_APPROVE,
            "Asked when a partner's order was drafted without a person (edi.auto_draft): is \
             the draft approved at once, by the partner's run-as user? Unless configured \
             otherwise it is refused and the draft waits for a person. Approving still asks \
             every policy of sales_order.approve.",
        )
        .text("partner_code", "The partner's code")
        .number("lines", "How many lines the order has")
        .number("total_net", "The draft's total, without tax")
        .as_money(None)
        .refuse_by_default()
        .refusal("drafts from this partner are approved by a person"),
        PolicyPoint::new(
            SEND,
            "Asked for every business message about to be sent to a partner (855, 856, 810): \
             may it go? Unless configured otherwise it may. A company that wants invoices to a \
             partner looked at first asks for approval, for example when `facts.message == \
             '810' && facts.total > 10000`.",
        )
        .text("partner_code", "The partner's code")
        .text("message", "The message: 855, 856 or 810")
        .number("total", "What an invoice comes to; zero for other messages")
        .as_money(None)
        .boolean("test", "Whether the partner is still in test")
        .refusal("this message may not be sent yet"),
        PolicyPoint::new(
            BEFORE_ASN,
            "Asked when an invoice (810) is about to be sent for an order whose ship notice \
             (856) the partner has not acknowledged: may it go before? Unless configured \
             otherwise it may. A partner that rejects invoices ahead of their ship notice is \
             refused here, for example when `facts.notice == 'none'`.",
        )
        .text("partner_code", "The partner's code")
        .text("order_number", "The partner's order number")
        .text(
            "notice",
            "The state of the order's ship notice: none, invalid, ready, sent, rejected, ...",
        )
        .refusal("the ship notice must be acknowledged first"),
    ]
}

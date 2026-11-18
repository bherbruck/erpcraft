//! Prompts: pieces of real work, written down.
//!
//! A prompt is something a user picks, with arguments, and it tells the
//! model how to do one job with the tools of this server: which checks to
//! make, in which order, and where to stop and ask. Every tool a prompt
//! mentions is written as `{tool:<command>}` and filled in from the
//! registry, so a prompt cannot name a command that does not exist: it is
//! then not offered at all, and a test fails. A prompt is also left out
//! for a caller who could not run one of its commands.

use erpcraft_kernel::Kernel;
use serde_json::{Value, json};

use crate::{Answer, Request, RpcError, listed, page};

const PAGE_SIZE: usize = 50;

struct Argument {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    required: bool,
}

struct Prompt {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    arguments: &'static [Argument],
    /// The text, with `{tool:<command>}` for a tool and `{arg:<name>}`
    /// for an argument.
    text: &'static str,
}

const PROMPTS: &[Prompt] = &[
    Prompt {
        name: "enter_vendor_invoice",
        title: "Enter a vendor invoice against a purchase order",
        description: "Find the purchase order, draft the vendor invoice from what was \
                      received, make it agree with the invoice the vendor sent, preview the \
                      posting and post it.",
        arguments: &[
            Argument {
                name: "purchase_order",
                title: "Purchase order",
                description: "The number of the purchase order, such as PO-2026-00012, or its id.",
                required: true,
            },
            Argument {
                name: "invoice",
                title: "The vendor's invoice",
                description: "What the vendor's invoice says: its number and date, and each \
                              line with quantity and unit price, as text.",
                required: true,
            },
        ],
        text: "Enter the vendor's invoice for purchase order {arg:purchase_order}.\n\
\n\
The vendor's invoice says:\n\
{arg:invoice}\n\
\n\
Work in this order, and stop to ask me whenever something does not agree.\n\
\n\
1. Find the order. If you were given a number, call {tool:document.list} with `doc_type` \
`purchase_order` and `q` set to the number; then read it with {tool:document.get}. It must be \
`open`. Its lines show the quantity ordered, received and invoiced.\n\
2. Only what has been received can be invoiced. If the invoice is for goods the order shows as \
not yet received, tell me, and do not go on until I say whether they have arrived. If they have, \
draft the receipt with {tool:purchase_order.receive}, set the quantities that arrived with \
{tool:document.update}, and post it with {tool:purchase_receipt.post}.\n\
3. Draft the invoice with {tool:purchase_order.invoice}, giving the order's id. It returns a \
draft vendor invoice with a line for everything received and not yet invoiced, at the order's \
prices. Nothing is posted yet.\n\
4. Make the draft say what the vendor's invoice says: the quantities and unit prices of the \
lines, the tax codes, the vendor's invoice number and the date. Change it with \
{tool:document.update}, sending the `version` you last read. Money and quantities are decimal \
strings. Do not change a price or quantity to make the totals agree: if the vendor charges \
something the order does not have, tell me.\n\
5. Preview the posting: call {tool:vendor_invoice.post} with `preview: true`. Show me what it \
would post: the amount owed to the vendor and any difference from the order price. If it is \
refused, tell me the message as it is; a refusal by a company policy (for example a price that \
differs too much from the order) is not yours to work around.\n\
6. When I agree, post it: {tool:vendor_invoice.post} again without `preview`, with an \
`idempotency_key`. Then tell me the invoice's number and, from {tool:open_item.list} with \
`kind` `payable`, what is now owed to this vendor.",
    },
    Prompt {
        name: "close_month",
        title: "Close a fiscal period",
        description: "Run the checks that should pass before a period is closed, show what \
                      they found, and close the period once the user agrees.",
        arguments: &[
            Argument {
                name: "company",
                title: "Company",
                description: "The code or name of the company.",
                required: true,
            },
            Argument {
                name: "period",
                title: "Period",
                description: "The name of the fiscal period to close, or a date in it.",
                required: true,
            },
        ],
        text: "Close the fiscal period {arg:period} of company {arg:company}.\n\
\n\
Closing is not undone lightly, so check first and close last. Report each check as you go: \
what you looked at, and whether it is clean.\n\
\n\
1. Find the company and the period: {tool:entity.list} with `entity` `company`, then with \
`entity` `fiscal_period` filtered to the company. The period must be `open`. Note its start and \
end date.\n\
2. Nothing should be left half done. For each document type the company uses, call \
{tool:document.list} with `status` `draft` and a `where` on `record.doc_date` between the \
period's dates. List every draft you find: each should be posted, deleted, or moved to a later \
date before the period closes. Ask me what to do with them; do not post or delete any yourself.\n\
3. The books balance: {tool:gl.trial_balance} with `as_of` the period's end date. Total debit \
must equal total credit.\n\
4. What is owed agrees with the books: {tool:open_item.list} for `payable` and for \
`receivable`. The total still open of each should equal the balance of the payables and the \
receivables account in the trial balance. Say so if they differ, and by how much.\n\
5. Stock agrees with the books: the total value from {tool:stock.on_hand} should equal the \
balance of the inventory accounts in the trial balance.\n\
6. Show me the result of the period: {tool:gl.income_statement} for the period's dates.\n\
7. Tell me what you found and ask whether to close. If I agree, preview \
{tool:fiscal_period.close} with `preview: true`, then run it with an `idempotency_key`. After \
that nothing can be posted in the period, by any document.\n\
\n\
If the period was closed by mistake, {tool:fiscal_period.reopen} opens it again. Closing a \
year's result into equity is a separate step, a `year_end_close` document, and is not part of \
this.",
    },
    Prompt {
        name: "explain_refusal",
        title: "Explain why something was refused",
        description: "Given the error a tool returned, work out what was refused, why, and \
                      what would have to change for it to go through.",
        arguments: &[
            Argument {
                name: "error",
                title: "Error",
                description: "The error as it was returned: the error object, or its message.",
                required: true,
            },
            Argument {
                name: "tool",
                title: "Tool",
                description: "The tool that was called, if known.",
                required: false,
            },
            Argument {
                name: "arguments",
                title: "Arguments",
                description: "What the tool was called with, if known, as JSON.",
                required: false,
            },
        ],
        text: "Explain this refusal to me in plain words: what was refused, why, and what \
would have to change for it to go through. Find out; do not guess, and do not try the call \
again with other values to see what happens.\n\
\n\
The error:\n\
{arg:error}\n\
\n\
The tool that was called: {arg:tool}\n\
Its arguments: {arg:arguments}\n\
\n\
An error object has a `code`, a `message` that is a complete sentence, and where known the \
`field` it is about, the `collection` and `line` of a document body (lines count from 1), and \
for a conflict a `reason`. Go by the code:\n\
\n\
- `validation`: the input is wrong or a rule of the business refused it. `field` and `line` \
say where. If the message speaks of a policy or of what the company accepts, a policy rule \
decided: read the resource erpcraft://policies for the point and its facts, and the rules \
themselves with {tool:entity.list} on `policy_rule`. If it says no open fiscal period contains \
a date, look at the periods with {tool:entity.list} on `fiscal_period`.\n\
- `conflict` with reason `state`: the record is in a state that does not allow this. Read it \
with {tool:document.get} to see its status; {tool:meta.describe} for its module shows the \
states and which commands apply from which. With reason `stale`: someone changed it after it \
was read; read it again. `duplicate`: a value that must be unique is taken. `in_use`: \
something still refers to it.\n\
- `forbidden`: the user, or the token, may not do this. {tool:meta.describe} shows under \
`access` what they may do. Say who would have to grant it; do not look for another way.\n\
- `not_found`: there is no such record, or the user may not see it. The two look the same on \
purpose.\n\
\n\
If the refusal is about one record, {tool:audit.history} for it shows who did what to it \
before, failed attempts included, which often says how it got into its state.\n\
\n\
End with one sentence on what I can do next, and who else would have to act if I cannot.",
    },
];

/// The commands a text names, in order of appearance.
fn named_commands(text: &str) -> Vec<&str> {
    let mut commands: Vec<&str> = text
        .split("{tool:")
        .skip(1)
        .filter_map(|rest| rest.split('}').next())
        .collect();
    commands.dedup();
    commands
}

/// Every prompt with the commands it names, for a test to hold against
/// the registry of a full installation.
pub fn prompt_commands() -> Vec<(&'static str, Vec<&'static str>)> {
    PROMPTS
        .iter()
        .map(|prompt| (prompt.name, named_commands(prompt.text)))
        .collect()
}

/// The text with every `{tool:...}` filled in. `None` if it names a
/// command that is not registered.
fn with_tools(kernel: &Kernel, text: &str) -> Option<String> {
    let mut filled = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("{tool:") {
        filled.push_str(&rest[..at]);
        let after = &rest[at + "{tool:".len()..];
        let end = after.find('}')?;
        let entry = kernel.registry().command(&after[..end]).ok()?;
        filled.push_str(&entry.meta.tool_name());
        rest = &after[end + 1..];
    }
    filled.push_str(rest);
    Some(filled)
}

/// The prompts this caller can use: every command named is registered,
/// and the caller could run it.
fn offered<'a>(request: &'a Request<'_>) -> impl Iterator<Item = &'static Prompt> + 'a {
    PROMPTS.iter().filter(|prompt| {
        named_commands(prompt.text).iter().all(|name| {
            request.kernel.registry().command(name).is_ok_and(|entry| {
                let permission = entry.meta.permission.as_deref();
                permission.is_none_or(|p| request.actor.can_any(p))
            })
        })
    })
}

/// `prompts/list`.
pub(crate) fn list(request: &Request<'_>) -> Answer {
    let prompts: Vec<Value> = offered(request)
        .map(|prompt| {
            let arguments: Vec<Value> = prompt
                .arguments
                .iter()
                .map(|argument| {
                    json!({
                        "name": argument.name,
                        "title": argument.title,
                        "description": argument.description,
                        "required": argument.required,
                    })
                })
                .collect();
            json!({
                "name": prompt.name,
                "title": prompt.title,
                "description": prompt.description,
                "arguments": arguments,
            })
        })
        .collect();
    let (prompts, next) = page(&prompts, "name", request.params, PAGE_SIZE)?;
    // Which prompts are offered depends on what the caller may do.
    Ok(listed(
        request,
        "prompts",
        prompts.to_vec(),
        next,
        "private",
    ))
}

/// `prompts/get`.
pub(crate) fn get(request: &Request<'_>) -> Answer {
    let params = request.params;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid("prompts/get needs the name of a prompt"))?;
    let prompt = offered(request)
        .find(|prompt| prompt.name == name)
        .ok_or_else(|| RpcError::invalid(format!("Unknown prompt: {name}")))?;
    let given = match params.get("arguments") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => return Err(RpcError::invalid("arguments must be an object")),
    };
    if let Some(unknown) = given
        .keys()
        .find(|key| !prompt.arguments.iter().any(|a| a.name == key.as_str()))
    {
        return Err(RpcError::invalid(format!(
            "prompt {name} has no argument '{unknown}'"
        )));
    }

    let mut text = with_tools(request.kernel, prompt.text).ok_or_else(|| {
        RpcError::new(
            crate::INTERNAL_ERROR,
            format!("prompt {name} names a command that is not registered"),
        )
    })?;
    for argument in prompt.arguments {
        let value = match given.get(argument.name) {
            Some(Value::String(value)) if !value.trim().is_empty() => value.trim().to_owned(),
            None | Some(Value::Null | Value::String(_)) if argument.required => {
                return Err(RpcError::invalid(format!(
                    "prompt {name} needs the argument '{}': {}",
                    argument.name, argument.description
                )));
            }
            None | Some(Value::Null | Value::String(_)) => "(not given)".to_owned(),
            Some(_) => {
                return Err(RpcError::invalid(format!(
                    "the argument '{}' of prompt {name} must be a text",
                    argument.name
                )));
            }
        };
        text = text.replace(&format!("{{arg:{}}}", argument.name), &value);
    }
    Ok(json!({
        "description": prompt.description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_well_formed() {
        let mut names: Vec<&str> = PROMPTS.iter().map(|p| p.name).collect();
        names.dedup();
        assert_eq!(names.len(), PROMPTS.len());
        for prompt in PROMPTS {
            // Every argument is used, and nothing else is asked for.
            for argument in prompt.arguments {
                assert!(
                    prompt.text.contains(&format!("{{arg:{}}}", argument.name)),
                    "{} does not use {}",
                    prompt.name,
                    argument.name
                );
            }
            for used in prompt.text.split("{arg:").skip(1) {
                let used = used.split('}').next().unwrap();
                assert!(
                    prompt.arguments.iter().any(|a| a.name == used),
                    "{} uses an argument it does not declare: {used}",
                    prompt.name
                );
            }
            // It is about tools, and names them as commands.
            let commands = named_commands(prompt.text);
            assert!(commands.len() >= 3, "{}", prompt.name);
            for command in commands {
                assert!(command.contains('.'), "{}: {command}", prompt.name);
            }
        }
        assert_eq!(prompt_commands().len(), PROMPTS.len());
    }
}

//! Reference material as resources.
//!
//! What an agent should know before it acts and that is the same for
//! every caller: how commands, errors, documents and policies work, which
//! policies there are, and what each module holds. The explanations are
//! not written here. They are the sections of `ARCHITECTURE.md`, compiled
//! in and cut out by their headings, so there is one text to keep right.
//!
//! Records and documents are not resources: what a caller may read of
//! them is decided per call, by row and field, which the `entity_*` and
//! `document_*` tools do.

use serde_json::{Value, json};

use crate::{
    Answer, INVALID_PARAMS, LEGACY_RESOURCE_NOT_FOUND, LIST_TTL_MS, Request, RpcError, listed, page,
};

const ARCHITECTURE: &str = include_str!("../../../ARCHITECTURE.md");

const PAGE_SIZE: usize = 50;
const MARKDOWN: &str = "text/markdown";
const JSON: &str = "application/json";

/// A section of the architecture document served as a guide.
struct Guide {
    uri: &'static str,
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// The headings whose sections make up the guide, in order.
    headings: &'static [&'static str],
}

const GUIDES: &[Guide] = &[
    Guide {
        uri: "erpcraft://guide/commands",
        name: "guide-commands",
        title: "How commands work",
        description: "What every command gets from the executor: a transaction, the \
                      permission check, idempotency keys, preview, the audit record. Read \
                      this before changing data.",
        headings: &["### Commands"],
    },
    Guide {
        uri: "erpcraft://guide/errors",
        name: "guide-errors",
        title: "How errors are reported",
        description: "The error object of a failed tool: code, message, field, line and \
                      reason, and what each reason asks you to do.",
        headings: &["### Errors"],
    },
    Guide {
        uri: "erpcraft://guide/documents",
        name: "guide-documents",
        title: "Documents, postings and reversals",
        description: "What a document is, how it changes a ledger through postings, why a \
                      posting is never edited but reversed, and how documents follow from \
                      each other (an order, its receipts, its invoices).",
        headings: &[
            "### Documents and postings",
            "### Documents that follow from each other",
        ],
    },
    Guide {
        uri: "erpcraft://guide/policies",
        name: "guide-policies",
        title: "Policies: what a company can configure",
        description: "The difference between an invariant and a policy, how policy rules \
                      are written and decided, and what a refusal by a policy means.",
        headings: &["### Policies"],
    },
];

const POLICIES: &str = "erpcraft://policies";
const MODULE: &str = "erpcraft://modules/";

/// The section under a heading: from the heading to the next one of the
/// same or a higher level. `None` if the document has no such heading.
fn section(document: &str, heading: &str) -> Option<String> {
    let level = heading.chars().take_while(|c| *c == '#').count();
    let mut lines = document
        .lines()
        .skip_while(|line| line.trim_end() != heading);
    let mut text = vec![lines.next()?];
    for line in lines {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes <= level && line[hashes..].starts_with(' ') {
            break;
        }
        text.push(line);
    }
    Some(text.join("\n").trim_end().to_owned())
}

impl Guide {
    fn text(&self) -> Option<String> {
        let sections: Option<Vec<String>> = self
            .headings
            .iter()
            .map(|heading| section(ARCHITECTURE, heading))
            .collect();
        Some(sections?.join("\n\n"))
    }
}

/// Every resource, as `resources/list` shows it.
fn all(request: &Request<'_>) -> Vec<Value> {
    let mut resources: Vec<Value> = GUIDES
        .iter()
        .map(|guide| {
            json!({
                "uri": guide.uri,
                "name": guide.name,
                "title": guide.title,
                "description": guide.description,
                "mimeType": MARKDOWN,
            })
        })
        .collect();
    resources.push(json!({
        "uri": POLICIES,
        "name": "policies",
        "title": "Policy points",
        "description": "Every business judgement a company can configure with policy_rule \
                        records: what is judged, the facts a rule's condition may read with \
                        their types, and what is decided when no rule applies.",
        "mimeType": JSON,
    }));
    for module in request.kernel.registry().describe(None).modules {
        resources.push(json!({
            "uri": format!("{MODULE}{}", module.name),
            "name": format!("module-{}", module.name),
            "title": format!("{}: what the module holds", module.label),
            "description": format!(
                "The entities, document types with their states and transitions, commands \
                 and policy points of the module `{}`, as the registry describes them. The \
                 tool meta_describe gives the same, with what you in particular may do.",
                module.name
            ),
            "mimeType": JSON,
        }));
    }
    resources
}

/// `resources/list`.
pub(crate) fn list(request: &Request<'_>) -> Answer {
    let resources = all(request);
    let (resources, next) = page(&resources, "uri", request.params, PAGE_SIZE)?;
    // The same for every caller.
    Ok(listed(
        request,
        "resources",
        resources.to_vec(),
        next,
        "public",
    ))
}

/// `resources/templates/list`.
pub(crate) fn templates(request: &Request<'_>) -> Answer {
    let templates = vec![json!({
        "uriTemplate": format!("{MODULE}{{module}}"),
        "name": "module",
        "title": "What one module holds",
        "description": "The entities, document types, commands and policy points of one \
                        module. The module names are those erpcraft_modules lists.",
        "mimeType": JSON,
    })];
    Ok(listed(
        request,
        "resourceTemplates",
        templates,
        None,
        "public",
    ))
}

/// `resources/read`.
pub(crate) fn read(request: &Request<'_>) -> Answer {
    let uri = request
        .params
        .get("uri")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid("resources/read needs the uri of a resource"))?;
    let not_found = || {
        // The code changed with the current revision.
        let code = match request.speaking.modern {
            true => INVALID_PARAMS,
            false => LEGACY_RESOURCE_NOT_FOUND,
        };
        RpcError::new(code, "Resource not found").with_data(json!({ "uri": uri }))
    };
    let registry = request.kernel.registry();
    let pretty = |value: &Value| serde_json::to_string_pretty(value).unwrap_or_default();

    let (mime, text) = if let Some(guide) = GUIDES.iter().find(|guide| guide.uri == uri) {
        let text = guide.text().ok_or_else(|| {
            RpcError::new(
                crate::INTERNAL_ERROR,
                format!("the text of {uri} is missing from the server"),
            )
        })?;
        (MARKDOWN, text)
    } else if uri == POLICIES {
        let policies = serde_json::to_value(registry.describe(None).policies).unwrap_or_default();
        (JSON, pretty(&json!({ "policies": policies })))
    } else if let Some(module) = uri.strip_prefix(MODULE) {
        let description = registry.describe(Some(module));
        if description.modules.is_empty() {
            return Err(not_found());
        }
        (
            JSON,
            pretty(&serde_json::to_value(description).unwrap_or_default()),
        )
    } else {
        return Err(not_found());
    };

    let mut result = json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] });
    if request.speaking.modern {
        result["ttlMs"] = json!(LIST_TTL_MS);
        result["cacheScope"] = json!("public");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_runs_to_the_next_heading_of_its_level() {
        let document =
            "# Top\n\nintro\n\n## One\n\nfirst\n\n### Inside\n\ndeeper\n\n## Two\n\nsecond\n";
        assert_eq!(
            section(document, "## One").unwrap(),
            "## One\n\nfirst\n\n### Inside\n\ndeeper"
        );
        assert_eq!(
            section(document, "### Inside").unwrap(),
            "### Inside\n\ndeeper"
        );
        assert_eq!(section(document, "## Two").unwrap(), "## Two\n\nsecond");
        assert_eq!(section(document, "## Three"), None);
        // A line of code that starts with a hash is not a heading.
        assert_eq!(
            section("## A\n#!/bin/sh\n## B\n", "## A").unwrap(),
            "## A\n#!/bin/sh"
        );
    }

    /// The guides are cut out of the architecture document by heading. If
    /// a heading there is renamed, this says so before a client finds out.
    #[test]
    fn every_guide_is_found_in_the_architecture_document() {
        for guide in GUIDES {
            let text = guide
                .text()
                .unwrap_or_else(|| panic!("{} names a heading that is gone", guide.uri));
            for heading in guide.headings {
                assert!(text.contains(heading), "{}", guide.uri);
            }
            assert!(text.len() > 500, "{} is nearly empty", guide.uri);
            assert!(
                text.len() < 20_000,
                "{} took in more than its section",
                guide.uri
            );
        }
        let documents = GUIDES[2].text().unwrap();
        assert!(documents.contains("posting"));
        assert!(documents.contains("reverse"));
        // The section after it is not part of it.
        assert!(!documents.contains("### Ledgers own their rules"));
    }
}

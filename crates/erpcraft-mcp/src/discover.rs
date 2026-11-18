//! Finding one's way without reading every tool.
//!
//! The specification pages `tools/list` but has no way to ask for part of
//! it, and a client that pages gets everything in the end: some seventy
//! tools, more with every module. So two tools of this server's own come
//! first in the list. `erpcraft_modules` says what there is, and
//! `erpcraft_tools` gives the tools of one module or for one resource,
//! complete with their schemas. They are built on what the registry knows,
//! as `meta.describe` is, and like the list they leave out what the caller
//! can never run.

use erpcraft_kernel::Error;
use erpcraft_kernel::command::CommandMeta;
use serde_json::{Value, json};

use crate::{Request, Speaking, tools};

pub(crate) const MODULES: &str = "erpcraft_modules";
pub(crate) const TOOLS: &str = "erpcraft_tools";

fn modules_output() -> Value {
    json!({
        "type": "object",
        "properties": {
            "modules": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "What to pass to erpcraft_tools and meta_describe." },
                        "label": { "type": "string" },
                        "depends_on": { "type": "array", "items": { "type": "string" } },
                        "documents": {
                            "type": "array", "items": { "type": "string" },
                            "description": "The document types of the module: business records with a workflow.",
                        },
                        "records": {
                            "type": "array", "items": { "type": "string" },
                            "description": "The entities of the module: plain records.",
                        },
                        "reports": { "type": "array", "items": { "type": "string" } },
                        "tools": { "type": "integer", "description": "How many of its tools you could run." },
                    },
                    "required": ["name", "label", "depends_on", "documents", "records", "reports", "tools"],
                },
            },
        },
        "required": ["modules"],
    })
}

fn tools_output() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tools": {
                "type": "array",
                "description": "Tool definitions as in tools/list: name, description, inputSchema and the rest.",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "description": { "type": "string" },
                        "inputSchema": { "type": "object" },
                    },
                    "required": ["name", "description", "inputSchema"],
                },
            },
        },
        "required": ["tools"],
    })
}

/// The definitions of the two tools, for the head of `tools/list`.
pub(crate) fn definitions(speaking: Speaking) -> Vec<Value> {
    let mut modules = json!({
        "name": MODULES,
        "description": "Start here. Lists the modules of this ERP with what each holds: its \
                        document types, its records, its reports, and how many of its tools you \
                        could run. Then call erpcraft_tools with a module to get those tools.",
        "inputSchema": { "type": "object", "additionalProperties": false },
    });
    let mut tools = json!({
        "name": TOOLS,
        "description": "The tools of one module, or the tools that act on one resource (an entity \
                        or a document type, such as purchase_order), with their input schemas, so \
                        that you need not read the whole tool list. Give a module, a resource, or \
                        both. Tools you could never run are left out.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "module": {
                    "type": "string",
                    "title": "Module",
                    "description": "A module name from erpcraft_modules, such as purchasing.",
                },
                "resource": {
                    "type": "string",
                    "title": "Resource",
                    "description": "An entity or document type, such as purchase_order or account.",
                },
            },
            "additionalProperties": false,
        },
    });
    let hints = json!({ "readOnlyHint": true, "openWorldHint": false });
    for (tool, title, output) in [
        (&mut modules, "Modules of this ERP", modules_output()),
        (
            &mut tools,
            "Tools of a module or for a resource",
            tools_output(),
        ),
    ] {
        if speaking.has("2025-03-26") {
            tool["annotations"] = hints.clone();
        }
        if speaking.has("2025-06-18") {
            tool["title"] = json!(title);
            tool["outputSchema"] = output;
        }
    }
    vec![modules, tools]
}

/// Answer a call to one of the two tools; `None` for any other name.
pub(crate) fn call(request: &Request<'_>, name: &str, arguments: &Value) -> Option<Value> {
    let outcome = match name {
        MODULES => Ok(modules(request)),
        TOOLS => tools_for(request, arguments),
        _ => return None,
    };
    Some(match outcome {
        Ok(structured) => tools::result(structured, false),
        Err(err) => tools::failed(&err, name),
    })
}

fn modules(request: &Request<'_>) -> Value {
    let description = request.kernel.registry().describe(None);
    let visible: Vec<&CommandMeta> = tools::visible(request.kernel, request.actor).collect();
    let modules: Vec<Value> = description
        .modules
        .iter()
        .map(|module| {
            let of_module = |m: &str| m == module.name;
            json!({
                "name": module.name,
                "label": module.label,
                "depends_on": module.depends_on,
                "documents": description.doc_types.iter()
                    .filter(|d| of_module(&d.module)).map(|d| &d.label).collect::<Vec<_>>(),
                "records": description.entities.iter()
                    .filter(|e| of_module(&e.module)).map(|e| &e.label).collect::<Vec<_>>(),
                "reports": visible.iter()
                    .filter(|c| of_module(&c.module))
                    .filter_map(|c| c.report.as_ref().map(|r| &r.title)).collect::<Vec<_>>(),
                "tools": visible.iter().filter(|c| of_module(&c.module)).count(),
            })
        })
        .collect();
    json!({ "modules": modules })
}

fn tools_for(request: &Request<'_>, arguments: &Value) -> Result<Value, Error> {
    let text = |field: &str| match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.trim()).filter(|v| !v.is_empty())),
        Some(_) => Err(Error::field(field, "must be a text")),
    };
    if let Some(unknown) = arguments.as_object().and_then(|object| {
        object
            .keys()
            .find(|key| !["module", "resource"].contains(&key.as_str()))
    }) {
        return Err(Error::validation(format!(
            "unknown field '{unknown}'; give `module`, `resource`, or both"
        ))
        .with_field(unknown));
    }
    let (module, resource) = (text("module")?, text("resource")?);
    if module.is_none() && resource.is_none() {
        return Err(Error::validation(
            "give a module, a resource, or both; erpcraft_modules lists the modules",
        ));
    }
    let registry = request.kernel.registry();
    if let Some(module) = module
        && !registry
            .describe(None)
            .modules
            .iter()
            .any(|m| m.name == module)
    {
        return Err(Error::field(
            "module",
            format!("names no module: '{module}'; erpcraft_modules lists the modules"),
        ));
    }
    let found: Vec<Value> = tools::visible(request.kernel, request.actor)
        .filter(|meta| module.is_none_or(|m| meta.module == m))
        .filter(|meta| {
            resource.is_none_or(|r| meta.resource == r || meta.creates.as_deref() == Some(r))
        })
        .map(|meta| tools::definition(meta, request.speaking))
        .collect();
    Ok(json!({ "tools": found }))
}

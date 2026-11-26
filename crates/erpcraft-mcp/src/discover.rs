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
use erpcraft_kernel::registry::{FieldMeta, FieldType};
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
                            "type": "array", "items": named_item(),
                            "description": "The document types of the module: business records with a workflow. Pass the name as `resource` to erpcraft_tools, as `doc_type` to the document_* tools.",
                        },
                        "records": {
                            "type": "array", "items": named_item(),
                            "description": "The entities of the module: plain records. Pass the name as `resource` to erpcraft_tools, as `entity` to the entity_* tools.",
                        },
                        "reports": { "type": "array", "items": { "type": "string" } },
                        "tools": { "type": "integer", "description": "How many of its tools you could run." },
                    },
                    "required": ["name", "label", "depends_on", "documents", "records", "reports", "tools"],
                },
            },
            "core_tools": {
                "type": "array",
                "description": "Tools for any record or document, whatever the module: finding, reading and writing them. erpcraft_tools with `names` gives their schemas.",
                "items": named_item(),
            },
        },
        "required": ["modules", "core_tools"],
    })
}

fn named_item() -> Value {
    json!({
        "type": "object",
        "properties": { "name": { "type": "string" }, "label": { "type": "string" } },
        "required": ["name", "label"],
    })
}

/// The tools every agent needs whatever it does, in the order it needs
/// them: find, read, write. They belong to the kernel, so a module's tools
/// do not include them.
const CORE: [&str; 10] = [
    "search.records",
    "entity.list",
    "entity.get",
    "entity.create",
    "entity.update",
    "document.list",
    "document.get",
    "document.create",
    "document.update",
    "document.blockers",
];

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
            "resource": {
                "type": "object",
                "description": "With `resource`: what a record or document of it holds, the fields to give when creating one (`required` marks those that must be given) and, for a document type, its lines and states.",
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
        "description": "The tools of one module, the tools that act on one resource (an entity \
                        or a document type, such as customer or sales_order), or tools by name, \
                        with their input schemas, so that you need not read the whole tool list. \
                        For a resource it also says its fields and lines, and gives the entity_* \
                        or document_* tools that read and write it. Tools you could never run are \
                        left out.",
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
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "title": "Names",
                    "description": "Tool names, such as gl_trial_balance: these tools, whatever their module.",
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
                    .filter(|d| of_module(&d.module))
                    .map(|d| json!({ "name": d.name, "label": d.label })).collect::<Vec<_>>(),
                "records": description.entities.iter()
                    .filter(|e| of_module(&e.module))
                    .map(|e| json!({ "name": e.name, "label": e.label })).collect::<Vec<_>>(),
                "reports": visible.iter()
                    .filter(|c| of_module(&c.module))
                    .filter_map(|c| c.report.as_ref().map(|r| &r.title)).collect::<Vec<_>>(),
                "tools": visible.iter().filter(|c| of_module(&c.module)).count(),
            })
        })
        .collect();
    let core: Vec<Value> = CORE
        .iter()
        .filter_map(|name| visible.iter().find(|c| c.name == *name))
        .map(|c| json!({ "name": c.tool_name(), "label": c.description.split(". ").next().unwrap_or_default() }))
        .collect();
    json!({ "modules": modules, "core_tools": core })
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
            .find(|key| !["module", "resource", "names"].contains(&key.as_str()))
    }) {
        return Err(Error::validation(format!(
            "unknown field '{unknown}'; give `module`, `resource`, `names`, or module and resource"
        ))
        .with_field(unknown));
    }
    let registry = request.kernel.registry();
    if let Some(names) = arguments.get("names").filter(|v| !v.is_null()) {
        let Some(names) = names
            .as_array()
            .and_then(|a| a.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
        else {
            return Err(Error::field("names", "must be a list of tool names"));
        };
        let visible: Vec<&CommandMeta> = tools::visible(request.kernel, request.actor).collect();
        let mut found = Vec::new();
        let mut unknown = Vec::new();
        for name in names {
            match visible.iter().find(|m| m.tool_name() == name) {
                Some(meta) => found.push(tools::definition(request.kernel, meta, request.speaking)),
                None => unknown.push(name),
            }
        }
        let mut answer = json!({ "tools": found });
        if !unknown.is_empty() {
            answer["unknown"] = json!(unknown);
        }
        return Ok(answer);
    }
    let (module, resource) = (text("module")?, text("resource")?);
    if module.is_none() && resource.is_none() {
        return Err(Error::validation(
            "give a module, a resource, or names; erpcraft_modules lists the modules",
        ));
    }
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
    let described = match resource {
        Some(r) => Some(describe_resource(request, r).ok_or_else(|| {
            Error::field(
                "resource",
                format!(
                    "names no entity or document type: '{r}'; erpcraft_modules lists them \
                     under records and documents"
                ),
            )
        })?),
        None => None,
    };
    // For a resource, the generic tools that read and write it come
    // first, where the caller may use them on it.
    let generic: Vec<&CommandMeta> = match (&described, resource) {
        (Some((kind, _)), Some(r)) => {
            let prefix = if *kind == "entity" {
                "entity."
            } else {
                "document."
            };
            tools::visible(request.kernel, request.actor)
                .filter(|meta| {
                    meta.name.strip_prefix(prefix).is_some_and(|action| {
                        ["list", "get", "create", "update", "delete", "blockers"].contains(&action)
                    })
                })
                .filter(|meta| module.is_none_or(|m| meta.module == m))
                .filter(|meta| {
                    let action = match meta.name.rsplit('.').next().unwrap_or_default() {
                        "list" | "get" | "blockers" => "read",
                        other => other,
                    };
                    request.actor.can_any(&format!("{r}:{action}"))
                })
                .collect()
        }
        _ => Vec::new(),
    };
    let found: Vec<Value> = generic
        .into_iter()
        .chain(
            tools::visible(request.kernel, request.actor)
                .filter(|meta| module.is_none_or(|m| meta.module == m))
                .filter(|meta| {
                    resource
                        .is_none_or(|r| meta.resource == r || meta.creates.as_deref() == Some(r))
                }),
        )
        .map(|meta| tools::definition(request.kernel, meta, request.speaking))
        .collect();
    let mut answer = json!({ "tools": found });
    if let Some((_, resource)) = described {
        answer["resource"] = resource;
    }
    Ok(answer)
}

/// An entity or document type as an agent needs it to write one: its
/// fields with their types in words, the fields to give, the lines of a
/// document and its states. A fraction of what meta_describe says of a
/// module.
fn describe_resource(request: &Request<'_>, name: &str) -> Option<(&'static str, Value)> {
    let registry = request.kernel.registry();
    if let Ok(entity) = registry.entity(name) {
        return Some((
            "entity",
            json!({
                "name": entity.name,
                "kind": "entity",
                "label": entity.label,
                "module": entity.module,
                "fields": fields(&entity.fields),
                "search": entity.search,
                "company_field": entity.company_field,
                "read_only": entity.read_only,
            }),
        ));
    }
    let doc = &registry.doc_type(name).ok()?.meta;
    Some((
        "document",
        json!({
            "name": doc.name,
            "kind": "document",
            "label": doc.label,
            "module": doc.module,
            "header": "document_create takes company_id, doc_date, memo and a body of these fields",
            "fields": fields(&doc.fields),
            "collections": doc.collections.iter().map(|c| json!({
                "name": c.name,
                "label": c.label,
                "fields": fields(&c.fields),
            })).collect::<Vec<_>>(),
            "states": doc.states.iter().map(|s| &s.name).collect::<Vec<_>>(),
            "initial_state": doc.initial_state,
            "summary": doc.summary.iter().map(|f| &f.name).collect::<Vec<_>>(),
        }),
    ))
}

/// The fields one may write, each as `{name, type, required?, help?}`,
/// with the type in words: `decimal string`, `link to customer (id)`.
fn fields(fields: &[FieldMeta]) -> Vec<Value> {
    fields
        .iter()
        .filter(|f| !f.readonly)
        .map(|f| {
            let ty = match &f.ty {
                FieldType::Link {
                    entity,
                    key,
                    filters,
                } => {
                    let mut ty = format!("link to {entity} ({key})");
                    if !filters.is_empty() {
                        let only: Vec<String> =
                            filters.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        ty.push_str(&format!(", only {}", only.join(", ")));
                    }
                    ty
                }
                FieldType::EntityLink { entity_field } => {
                    format!("id of a record of the entity {entity_field} names")
                }
                FieldType::DocumentLink { doc_type, .. } => format!("id of a {doc_type}"),
                FieldType::LineLink {
                    doc_type,
                    collection,
                    ..
                } => {
                    format!("id of a line of {doc_type}.{collection}")
                }
                FieldType::Lookup(lookup) => {
                    format!("{} of a row of {}", lookup.key, lookup.command)
                }
                FieldType::Enum { values, .. } => format!("one of {}", values.join(", ")),
                FieldType::Decimal => "decimal string".into(),
                other => serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v["kind"].as_str().map(str::to_owned))
                    .unwrap_or_default(),
            };
            let mut field = json!({ "name": f.name, "type": ty });
            if f.required {
                field["required"] = json!(true);
            }
            if let Some(help) = &f.help {
                field["help"] = json!(help);
            }
            field
        })
        .collect()
}

//! Commands as tools.
//!
//! A tool is described from what its command says about itself: nothing
//! here is written by hand for one command.

use erpcraft_kernel::command::{self, CommandMeta, Options};
use erpcraft_kernel::registry::humanize;
use erpcraft_kernel::{Actor, Error, Kernel};
use serde_json::{Value, json};

use crate::{Answer, Request, RpcError, Speaking, discover, listed, page};

/// Tools on one page of `tools/list`. The whole list is a few pages; an
/// agent that does not want it all asks `erpcraft_tools` for one module.
const PAGE_SIZE: usize = 50;

/// `tools/list`: the two tools for finding one's way first, then every
/// command the caller could run, by name. The order never changes while
/// the tools do not, so a client can cache the list.
pub(crate) fn list(request: &Request<'_>) -> Answer {
    let mut tools = discover::definitions(request.speaking);
    let mut commands: Vec<Value> = visible(request.kernel, request.actor)
        .map(|meta| definition(meta, request.speaking))
        .collect();
    // By the tool's own name: the registry orders by the command's, where
    // `asset.take_over` comes before `asset_disposal.post`.
    commands.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    tools.extend(commands);
    let (tools, next) = page(&tools, "name", request.params, PAGE_SIZE)?;
    // What a caller sees depends on what they may do.
    Ok(listed(request, "tools", tools.to_vec(), next, "private"))
}

/// The commands the caller could run: those whose permission they hold
/// for at least one company. In the order of the registry, which is by
/// name.
pub(crate) fn visible<'a>(
    kernel: &'a Kernel,
    actor: &'a Actor,
) -> impl Iterator<Item = &'a CommandMeta> {
    kernel
        .registry()
        .commands()
        .map(|entry| &entry.meta)
        .filter(|meta| meta.permission.as_deref().is_none_or(|p| actor.can_any(p)))
}

/// What to call a tool in a list for people: "Post journal entry",
/// "Trial balance".
fn title(meta: &CommandMeta) -> String {
    if let Some(report) = &meta.report {
        return report.title.clone();
    }
    let resource = meta.resource.replace('_', " ");
    match meta.resource_kind {
        // "Post" on a journal entry.
        Some(_) => format!("{} {resource}", meta.label),
        // "Describe" in `meta`: the resource is not a thing but a subject.
        None => format!("{} ({})", meta.label, humanize(&meta.resource)),
    }
}

/// The hints of the specification, from what the command declares.
///
/// - `readOnlyHint`: the command is a query.
/// - `destructiveHint`: it reverses, cancels or deletes. The specification
///   takes a tool to be destructive unless told otherwise, so it is said
///   for every tool that writes.
/// - `idempotentHint`: calling it again with the same arguments changes
///   nothing more. True without any help for a command that moves one
///   record out of certain states: the second call finds it moved and
///   either returns it unchanged or is refused. Not claimed for the
///   others, although an `idempotency_key` makes any of them safe to
///   repeat: the key is optional, and a client that trusted the hint and
///   retried a create without one would create twice.
/// - `openWorldHint`: false. A command acts on this ERP's data only.
fn annotations(meta: &CommandMeta) -> Value {
    let mut hints = json!({ "readOnlyHint": meta.read_only, "openWorldHint": false });
    if !meta.read_only {
        hints["destructiveHint"] = json!(meta.destructive);
        hints["idempotentHint"] = json!(meta.target && !meta.from_states.is_empty());
    }
    hints
}

/// A schema without the two keywords that belong at a root only. The
/// definitions come back so they can be put at the new root: references
/// to them are written from the root.
fn inner(schema: &Value) -> (Value, Option<Value>) {
    let mut schema = schema.clone();
    let mut definitions = None;
    if let Some(object) = schema.as_object_mut() {
        object.remove("$schema");
        definitions = object.remove("$defs");
    }
    (schema, definitions)
}

/// The schema of what `structuredContent` holds for a command with this
/// output schema: see [`structured`].
pub(crate) fn output_schema(output: &Value, read_only: bool) -> Value {
    let (output, definitions) = inner(output);
    let wrapped = |output: &Value| {
        json!({
            "type": "object",
            "properties": { "result": output },
            "required": ["result"],
        })
    };
    // The output as it is when it is an object, else under `result`.
    let plain = if output.get("type").and_then(Value::as_str) == Some("object") {
        output.clone()
    } else {
        json!({ "type": "object", "anyOf": [output, wrapped(&output)] })
    };
    let mut schema = if read_only {
        plain
    } else {
        let preview = json!({
            "type": "object",
            "description": "What a call with `preview: true` returns: what the command would have \
                            returned, and nothing was saved.",
            "properties": { "preview": { "const": true }, "result": output },
            "required": ["preview", "result"],
        });
        json!({ "type": "object", "anyOf": [plain, preview] })
    };
    if let Some(definitions) = definitions {
        schema["$defs"] = definitions;
    }
    schema
}

/// What a tool returns as `structuredContent`. The legacy revisions
/// require an object there, so every revision gets one: the output itself
/// when it is an object, otherwise under `result`; and a preview as
/// `{"preview": true, "result": ...}`, as over HTTP.
pub(crate) fn structured(output: Value, preview: bool) -> Value {
    match output {
        _ if preview => json!({ "preview": true, "result": output }),
        Value::Object(_) => output,
        other => json!({ "result": other }),
    }
}

/// Whether a command returns a document view, which `view` can shorten.
fn returns_document(meta: &CommandMeta) -> bool {
    let properties = &meta.output_schema["properties"];
    properties.get("doc_type").is_some() && properties.get("postings").is_some()
}

/// A tool definition, with what the client's revision knows.
pub(crate) fn definition(meta: &CommandMeta, speaking: Speaking) -> Value {
    let mut input = meta.input_schema.clone();
    if let Some(object) = input.as_object_mut() {
        // Without it the schema is JSON Schema 2020-12, which it is.
        object.remove("$schema");
        object.entry("type").or_insert(json!("object"));
        if returns_document(meta) {
            let properties = object.entry("properties").or_insert_with(|| json!({}));
            if let Some(properties) = properties.as_object_mut() {
                properties.insert(
                    "view".into(),
                    json!({
                        "type": "string",
                        "enum": ["full", "header"],
                        "title": "View",
                        "description": "How much of the document to return. `full` (the \
                            default): header, summary, body, postings with their ledger entries, \
                            and links. `header`: the header and summary only; body is null and \
                            postings and links are empty. Use `header` when you only need to \
                            know the outcome, as when importing many documents.",
                    }),
                );
            }
        }
        if !meta.read_only {
            let properties = object.entry("properties").or_insert_with(|| json!({}));
            if let Some(properties) = properties.as_object_mut() {
                let what = if meta.destructive {
                    "This tool cannot simply be undone: always preview it first."
                } else {
                    "Use it before anything that posts or cannot be taken back, and whenever \
                     you are not sure of the outcome."
                };
                properties.insert(
                    "preview".into(),
                    json!({
                        "type": "boolean",
                        "title": "Preview",
                        "description": format!(
                            "Set to true to run the command and get back what it would do, as \
                             {{\"preview\": true, \"result\": ...}}, without saving anything. {what} \
                             A preview reserves nothing: the real call checks everything again."
                        ),
                    }),
                );
                properties.insert(
                    "idempotency_key".into(),
                    json!({
                        "type": "string",
                        "title": "Idempotency key",
                        "description": "Give every change a value of its own, such as a UUID, and \
                                        send the same value again if you have to retry: after a \
                                        timeout or a lost answer the call then returns the first \
                                        result instead of applying the change twice. The same key \
                                        with different input is refused.",
                    }),
                );
            }
        }
    }
    let mut tool = json!({
        "name": meta.tool_name(),
        "description": meta.description,
        "inputSchema": input,
    });
    if speaking.has("2025-03-26") {
        tool["annotations"] = annotations(meta);
    }
    if speaking.has("2025-06-18") {
        tool["title"] = json!(title(meta));
        tool["outputSchema"] = output_schema(&meta.output_schema, meta.read_only);
    }
    tool
}

/// A tool result: the structured content, and the same as text for a
/// client that reads only text.
pub(crate) fn result(structured: Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": is_error,
    })
}

/// A tool that failed: the same error object as over HTTP, with the field,
/// line and reason when they are known. The model reads it and corrects
/// itself, which it could not do with a protocol error.
pub(crate) fn failed(err: &Error, tool: &str) -> Value {
    if let Error::Internal(detail) = err {
        tracing::error!(%detail, tool, "internal error");
    }
    result(json!({ "error": err.to_json() }), true)
}

/// `tools/call`.
pub(crate) async fn call(request: &Request<'_>) -> Answer {
    let params = request.params;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid("tools/call needs the name of a tool"))?;
    let mut arguments = match params.get("arguments").cloned() {
        None | Some(Value::Null) => json!({}),
        Some(value @ Value::Object(_)) => value,
        Some(_) => return Err(RpcError::invalid("arguments must be an object")),
    };
    if let Some(answer) = discover::call(request, name, &arguments) {
        return Ok(answer);
    }
    let Some(entry) = request.kernel.registry().command_by_tool(name) else {
        return Err(RpcError::invalid(format!("Unknown tool: {name}")));
    };

    let mut options = Options::default();
    if returns_document(&entry.meta)
        && let Some(object) = arguments.as_object_mut()
        && let Some(view) = object.remove("view")
    {
        options.view = serde_json::from_value(view)
            .map_err(|_| RpcError::invalid("view is full or header"))?;
    }
    if !entry.meta.read_only
        && let Some(object) = arguments.as_object_mut()
    {
        options.preview = object
            .remove("preview")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        options.idempotency_key = object
            .remove("idempotency_key")
            .and_then(|v| v.as_str().map(str::to_owned))
            .filter(|k| !k.is_empty());
    }
    let preview = options.preview;
    let outcome = command::execute(
        request.kernel,
        request.actor,
        &entry.meta.name,
        arguments,
        options,
    )
    .await;
    Ok(match outcome {
        Ok(output) => result(structured(output, preview), false),
        Err(err) => failed(&err, name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_schema_describes_what_is_returned() {
        // An object comes back as it is.
        let object = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": { "rows": { "type": "array", "items": { "$ref": "#/$defs/Row" } } },
            "$defs": { "Row": { "type": "object" } },
        });
        let read = output_schema(&object, true);
        assert_eq!(read["type"], "object");
        assert_eq!(read["properties"]["rows"]["type"], "array");
        assert_eq!(read["$defs"]["Row"]["type"], "object");
        assert!(read.get("$schema").is_none());

        // A command that writes may also have been previewed. The
        // definitions stay at the root, where the references point.
        let write = output_schema(&object, false);
        assert_eq!(write["type"], "object");
        assert_eq!(write["anyOf"][0]["properties"]["rows"]["type"], "array");
        assert_eq!(write["anyOf"][1]["properties"]["preview"]["const"], true);
        assert_eq!(write["anyOf"][1]["required"], json!(["preview", "result"]));
        assert!(write["anyOf"][0].get("$defs").is_none());
        assert_eq!(write["$defs"]["Row"]["type"], "object");

        // Anything else is under `result`, unless it turns out an object.
        let list = output_schema(&json!({ "type": "array" }), true);
        assert_eq!(list["type"], "object");
        assert_eq!(list["anyOf"][1]["properties"]["result"]["type"], "array");
        assert_eq!(
            structured(json!([1, 2]), false),
            json!({ "result": [1, 2] })
        );
        assert_eq!(structured(json!({ "a": 1 }), false), json!({ "a": 1 }));
        assert_eq!(
            structured(json!({ "a": 1 }), true),
            json!({ "preview": true, "result": { "a": 1 } })
        );
    }
}

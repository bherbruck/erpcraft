// A small JSON Schema to TypeScript converter, covering what the server's
// schemas use: objects, arrays, type unions, enum, const, oneOf, anyOf, $ref.

import type { Json, JsonObject } from "./lib";

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function literal(value: Json): string {
  return JSON.stringify(value);
}

function primitive(type: string, schema: JsonObject, root: JsonObject, indent: string): string {
  switch (type) {
    case "string":
      return "string";
    case "integer":
    case "number":
      return "number";
    case "boolean":
      return "boolean";
    case "null":
      return "null";
    case "array":
      return `Array<${schemaToType(schema.items, root, indent)}>`;
    case "object":
      return objectType(schema, root, indent);
    default:
      return "Json";
  }
}

function objectType(schema: JsonObject, root: JsonObject, indent: string): string {
  const properties = isObject(schema.properties) ? schema.properties : {};
  const required = new Set(Array.isArray(schema.required) ? schema.required : []);
  const inner = `${indent}  `;
  const lines: string[] = [];
  for (const name of Object.keys(properties).sort()) {
    const property = properties[name];
    const description = isObject(property) && typeof property.description === "string" ? property.description : null;
    if (description) {
      const text = description.replaceAll("*/", "* /").split("\n").join(`\n${inner} * `);
      lines.push(`${inner}/** ${text} */`);
    }
    const key = /^[A-Za-z_$][\w$]*$/.test(name) ? name : literal(name);
    lines.push(`${inner}${key}${required.has(name) ? "" : "?"}: ${schemaToType(property, root, inner)};`);
  }
  const extra = schema.additionalProperties;
  if (extra !== undefined && extra !== false) {
    // Named properties that may be absent must fit the index signature too.
    const optional = Object.keys(properties).some((name) => !required.has(name)) ? " | undefined" : "";
    lines.push(`${inner}[key: string]: ${extra === true ? "Json" : schemaToType(extra, root, inner)}${optional};`);
  }
  // An object the schema says nothing more about is any object; only `additionalProperties: false` closes it.
  if (lines.length === 0) return extra === false ? "Record<string, never>" : "{ [key: string]: Json }";
  return `{\n${lines.join("\n")}\n${indent}}`;
}

function resolveRef(ref: string, root: JsonObject): Json | undefined {
  if (!ref.startsWith("#/")) return undefined;
  let current: Json | undefined = root;
  for (const part of ref.slice(2).split("/")) {
    if (!isObject(current)) return undefined;
    current = current[part.replaceAll("~1", "/").replaceAll("~0", "~")];
  }
  return current;
}

/** Gives a `$ref` a type name instead of writing the referenced type out in place. */
export type RefNamer = (ref: string, root: JsonObject) => string | null;
let refNamer: RefNamer | null = null;

/** Runs `work` with references written as the names `namer` gives them. */
export function withNamedRefs<T>(namer: RefNamer, work: () => T): T {
  const previous = refNamer;
  refNamer = namer;
  try {
    return work();
  } finally {
    refNamer = previous;
  }
}

/** The TypeScript type of the values a schema accepts. */
export function schemaToType(schema: Json | undefined, root: JsonObject, indent: string): string {
  if (schema === undefined || schema === true) return "Json";
  if (schema === false) return "never";
  if (!isObject(schema)) return "Json";

  if (typeof schema.$ref === "string") {
    const name = refNamer?.(schema.$ref, root) ?? null;
    const referenced = name ?? schemaToType(resolveRef(schema.$ref, root), root, indent);
    // A reference with properties of its own beside it, such as the tag of a union member.
    return isObject(schema.properties) ? `${referenced} & ${objectType(schema, root, indent)}` : referenced;
  }
  if ("const" in schema && schema.const !== undefined) return literal(schema.const);
  if (Array.isArray(schema.enum)) return schema.enum.map(literal).join(" | ") || "never";
  for (const key of ["oneOf", "anyOf"] as const) {
    const options = schema[key];
    if (Array.isArray(options)) {
      const parts = [...new Set(options.map((option) => schemaToType(option, root, indent)))];
      return parts.join(" | ") || "never";
    }
  }
  if (Array.isArray(schema.type)) {
    const parts = schema.type.map((type) =>
      typeof type === "string" ? primitive(type, schema, root, indent) : "Json",
    );
    return [...new Set(parts)].join(" | ");
  }
  if (typeof schema.type === "string") return primitive(schema.type, schema, root, indent);
  if (isObject(schema.properties)) return objectType(schema, root, indent);
  return "Json";
}

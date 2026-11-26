// Reading the registry: what exists, and what the signed-in user may do with it.

import type { EntityName } from "@/api/generated";
import type {
  CommandMeta,
  DocTypeMeta,
  EntityMeta,
  FieldMeta,
  FieldType,
  Json,
  JsonObject,
  Meta,
  Reach,
} from "@/api/types";

// Names the UI itself relies on. Typed against the generated names, so the
// build fails if the server renames one of them.
export const COMPANY: EntityName = "company";
export const CURRENCY: EntityName = "currency";
export const ACCOUNT: EntityName = "account";
export const PRINT_TEMPLATE: EntityName = "print_template";
export const ROLE: EntityName = "role";
export const USER: EntityName = "user";

export function findEntity(meta: Meta, name: string | null | undefined): EntityMeta | undefined {
  return meta.entities.find((entity) => entity.name === name);
}

export function findDocType(meta: Meta, name: string | null | undefined): DocTypeMeta | undefined {
  return meta.doc_types.find((docType) => docType.name === name);
}

export function findCommand(meta: Meta, name: string | null | undefined): CommandMeta | undefined {
  return meta.commands.find((command) => command.name === name);
}

export function canEntity(meta: Meta, entity: string, action: "read" | "create" | "update" | "delete"): boolean {
  return meta.access.entities[entity]?.[action] === true;
}

export function canDoc(meta: Meta, docType: string | null | undefined, action: string): boolean {
  return docType ? meta.access.doc_types[docType]?.includes(action) === true : false;
}

export function canRun(meta: Meta, command: string): boolean {
  return meta.access.commands.includes(command);
}

/**
 * How far the user may use one field of an entity for an action: `all`, on
 * `some` records (on others the value is withheld and arrives blank), or
 * `none`. A field the registry does not list is free to use.
 */
export function fieldReach(meta: Meta, entity: string, field: string, action: "read" | "create" | "update"): Reach {
  return meta.access.entities[entity]?.fields[field]?.[action] ?? "all";
}

/** The fields of an entity the user may read at all. */
export function readableFields(meta: Meta, entity: EntityMeta): FieldMeta[] {
  return entity.fields.filter((field) => fieldReach(meta, entity.name, field.name, "read") !== "none");
}

/** Entities filtered by the current company carry it in a `company_id` field. */
export function isCompanyScoped(entity: EntityMeta): boolean {
  return entity.company_field === "company_id";
}

/** What the module is called in a menu. */
export function moduleLabel(meta: Meta, name: string): string {
  return meta.modules.find((module) => module.name === name)?.label ?? name;
}

// ------------------------------------------------------------------ schemas

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export interface InputProperty {
  name: string;
  /** What a form calls it. */
  label: string;
  description: string | null;
  /** `date` for a calendar date. */
  format: string | null;
  type: string | null;
  required: boolean;
  /** A secret: shown masked, never filled in by the browser, never read back. */
  secret: boolean;
  /** The value the command takes when the input is left out. */
  default: Json | undefined;
  /** The values it can take, where the schema lists them. */
  options: InputOption[] | null;
  /** What it is as a field (a link to an account, with filters), where the server says. */
  fieldType: FieldType | null;
  /** A decimal number written as a string (its pattern says so). */
  decimal: boolean;
}

export interface InputOption {
  value: string;
  label: string;
  description: string | null;
}

/** The inputs of a command, from its JSON Schema. */
/** A `$ref` within a schema, followed. */
function resolveRef(root: JsonObject, node: JsonObject): JsonObject {
  const ref = node.$ref;
  if (typeof ref !== "string" || !ref.startsWith("#/")) return node;
  let current: Json | undefined = root;
  for (const part of ref.slice(2).split("/")) current = isObject(current) ? current[part] : undefined;
  return isObject(current) ? { ...current, ...node, $ref: null } : node;
}

/** The values an input can take, where its schema lists them (`enum`, or `oneOf` of constants). */
function inputOptions(p: JsonObject): InputOption[] | null {
  if (Array.isArray(p.enum)) {
    return p.enum
      .filter((value): value is string => typeof value === "string")
      .map((value) => ({ value, label: value, description: null }));
  }
  const choices = Array.isArray(p.oneOf) ? p.oneOf : null;
  if (!choices?.every((choice) => isObject(choice) && typeof choice.const === "string")) return null;
  return choices.filter(isObject).map((choice) => ({
    value: String(choice.const),
    label: typeof choice.title === "string" ? choice.title : String(choice.const),
    description: typeof choice.description === "string" ? choice.description.replace(/\s*\n\s*/g, " ") : null,
  }));
}

/** The inputs of a command, from its JSON Schema. */
export function commandInputs(command: CommandMeta): InputProperty[] {
  const schema = isObject(command.input_schema) ? command.input_schema : {};
  const properties = isObject(schema.properties) ? schema.properties : {};
  const required = new Set(Array.isArray(schema.required) ? schema.required : []);
  return Object.entries(properties).map(([name, property]) => {
    let p = resolveRef(schema, isObject(property) ? property : {});
    // An optional choice is written `anyOf: [{$ref: the choices}, {type: "null"}]`: the choices it is.
    if (Array.isArray(p.anyOf)) {
      const choice = p.anyOf.find((branch) => isObject(branch) && branch.type !== "null");
      if (isObject(choice)) p = { ...resolveRef(schema, choice), ...p, anyOf: null };
    }
    const types = Array.isArray(p.type) ? p.type : [p.type];
    const type = types.find((candidate): candidate is string => typeof candidate === "string" && candidate !== "null");
    // What the input is, as a field, where the server says (`x-field-type`): a link with its filters.
    const declared = p["x-field-type"];
    return {
      name,
      label: typeof p.title === "string" ? p.title : name,
      // Schema descriptions are wrapped like source comments; a single line break is a space.
      description: typeof p.description === "string" ? p.description.replace(/(?<!\n)\n(?!\n)/g, " ") : null,
      format: typeof p.format === "string" ? p.format : null,
      type: type ?? null,
      required: required.has(name),
      secret: p.writeOnly === true || p.format === "password",
      default: p.default,
      options: inputOptions(p),
      fieldType: isFieldType(declared) ? declared : null,
      decimal: typeof p.pattern === "string" && p.pattern.startsWith("^-?\\d+(\\.\\d+)?"),
    };
  });
}

/**
 * Whether a dialog can ask for an input: a date, a yes or no, a number, a
 * choice, a link the server describes, or text. Not an id the schema says
 * nothing more about (which document, which line), nor a list or an object.
 */
export function askable(input: InputProperty): boolean {
  if (input.fieldType) return true;
  if (input.format === "uuid") return false;
  return input.type === "string" || input.type === "boolean" || input.type === "integer" || input.decimal;
}

/**
 * What a command's output schema calls a field: of the output itself, or of
 * one of its row types (`$defs`). The field's name where the schema gives no title.
 */
export function outputTitle(command: CommandMeta, field: string, def?: string): string {
  const schema = isObject(command.output_schema) ? command.output_schema : {};
  const defs = isObject(schema.$defs) ? schema.$defs : {};
  const node = def ? defs[def] : schema;
  const properties = isObject(node) && isObject(node.properties) ? node.properties : {};
  const property = properties[field];
  return isObject(property) && typeof property.title === "string" ? property.title : field;
}

function isFieldType(value: Json | undefined): value is FieldType {
  return isObject(value) && typeof value.kind === "string";
}

// ------------------------------------------------------------------ actions

export interface ResourceAction {
  /** "post" in "journal_entry.post". */
  action: string;
  label: string;
  command: CommandMeta;
  /** Inputs besides the record's id. */
  inputs: InputProperty[];
  destructive: boolean;
  /** The document type of the draft the action creates and returns, if it does. */
  creates: string | null;
}

export function toAction(command: CommandMeta): ResourceAction {
  return {
    action: command.action,
    label: command.label,
    command,
    inputs: commandInputs(command).filter((input) => input.name !== "id"),
    destructive: command.destructive,
    creates: command.creates,
  };
}

// An entity's record page saves and deletes through its own buttons; commands
// that do the same under the entity's name are not offered a second time,
// unless the entity is written only through its own commands (users).
const RECORD_PAGE_VERBS = new Set(["create", "update", "delete", "list", "get"]);

/**
 * The actions on one record that the user may run, as the registry describes
 * them: commands about this resource that take the record's id, and, for a
 * document, apply from its current state.
 */
export function recordActions(
  meta: Meta,
  resource: string,
  status: string | null,
  /** Commands whose other inputs a screen of the UI's own asks for (which document line). */
  picked: (command: string) => boolean = () => false,
): ResourceAction[] {
  return (
    meta.commands
      .filter(
        (command) =>
          command.resource === resource &&
          command.target &&
          !command.read_only &&
          canRun(meta, command.name) &&
          // Only where the generic layer writes the entity; one written by its own commands offers them.
          (command.resource_kind !== "entity" ||
            findEntity(meta, resource)?.read_only === true ||
            !RECORD_PAGE_VERBS.has(command.action)) &&
          (status === null || command.from_states.length === 0 || command.from_states.includes(status)),
      )
      .map(toAction)
      // A command that needs more than a dialog can ask (which line, which items) belongs to a screen of its own.
      .filter(
        (action) => picked(action.command.name) || action.inputs.every((input) => !input.required || askable(input)),
      )
  );
}

/**
 * The record's one primary action: the first whose `primary_when` the registry declares and the
 * record meets (its status, and which collections have lines), else the first that neither ends
 * nor undoes anything.
 */
export function primaryAction(
  actions: ResourceAction[],
  status: string,
  lineCount: (collection: string) => number,
): ResourceAction | null {
  const declared = actions.find((action) => {
    const when = action.command.primary_when;
    if (!when) return false;
    if (when.states.length > 0 && !when.states.includes(status)) return false;
    if (when.empty !== null && lineCount(when.empty) > 0) return false;
    if (when.not_empty !== null && lineCount(when.not_empty) === 0) return false;
    return true;
  });
  return declared ?? actions.find((action) => !action.destructive) ?? null;
}

// ------------------------------------------------------------------ reports

/**
 * The command that creates a record of an entity written through commands of
 * its own (users), where the generic layer does not: one that `creates` it,
 * or whose resource is the entity and whose action is `create`.
 */
export function createCommand(meta: Meta, entity: string): CommandMeta | undefined {
  return meta.commands.find(
    (command) =>
      !command.target &&
      !command.read_only &&
      canRun(meta, command.name) &&
      (command.creates === entity ||
        (command.resource === entity && command.resource_kind === "entity" && command.action === "create")),
  );
}

/** Commands the registry marks as reports the user may run, each shown as a page. */
export function reports(meta: Meta): CommandMeta[] {
  return meta.commands.filter((command) => command.report !== null && canRun(meta, command.name));
}

export function reportTitle(command: CommandMeta): string {
  return command.report?.title ?? command.label;
}

// --------------------------------------------------------------- navigation

export interface NavGroup {
  module: string;
  label: string;
  docTypes: DocTypeMeta[];
  entities: EntityMeta[];
  reports: CommandMeta[];
}

/**
 * The screens the user can open, grouped by module, in the order modules are
 * installed. Entities whose records live inside a parent record (lines of a
 * bill of materials) are edited on the parent's page and have no menu entry.
 */
export function navigation(meta: Meta): NavGroup[] {
  const allowed = reports(meta);
  return meta.modules
    .map((module) => ({
      module: module.name,
      label: module.label,
      docTypes: meta.doc_types.filter((d) => d.module === module.name && canDoc(meta, d.name, "read")),
      entities: meta.entities.filter(
        (e) => e.module === module.name && e.parent === null && canEntity(meta, e.name, "read"),
      ),
      reports: allowed.filter((report) => report.module === module.name),
    }))
    .filter((group) => group.docTypes.length + group.entities.length + group.reports.length > 0);
}

/** Entities whose records belong to a record of `parent`. */
export function childEntities(meta: Meta, parent: string): EntityMeta[] {
  return meta.entities.filter((entity) => entity.parent?.entity === parent && canEntity(meta, entity.name, "read"));
}

/** Fields worth a column in a list: not ids, not free-form JSON, not the company, not withheld. */
export function listFields(meta: Meta, entity: EntityMeta): FieldMeta[] {
  return readableFields(meta, entity).filter((field) => {
    if (field.type.kind === "uuid" || field.type.kind === "json") return false;
    if (isCompanyScoped(entity) && field.name === "company_id") return false;
    if (entity.parent && field.name === entity.parent.field) return false;
    return true;
  });
}

/** The value an enum shows for one of its values. */
export function enumLabel(field: FieldMeta, value: string): string {
  return field.type.kind === "enum" ? (field.type.labels[value] ?? value) : value;
}

/**
 * The document a document of this type is usually made from (an invoice's order), where the
 * type lets it be left out and then names the party itself instead: the header's optional
 * document link, when the header also has an optional party link the summary shows.
 */
export function withoutSource(docType: DocTypeMeta): FieldMeta | null {
  const source = docType.fields.find(
    (field) => field.type.kind === "document_link" && !field.required && !field.readonly,
  );
  const party = docType.fields.find(
    (field) =>
      field.type.kind === "link" &&
      !field.required &&
      !field.readonly &&
      docType.summary.some((shown) => shown.name === field.name),
  );
  return source && party ? source : null;
}

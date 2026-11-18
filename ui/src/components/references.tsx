// Inputs and values for the fields that refer to something other than a
// record of a fixed entity, each as the registry describes it:
//
//   document_link  a document of a type, in given states
//   line_link      a line of the document another field of this one names
//   lookup         a row of a query command, given inputs from this record
//   entity_link    a record of the entity another field names
//   posting        a posting, which is never chosen by hand

import { Link } from "react-router";
import { api } from "@/api/client";
import type { CommandMeta, FieldMeta, FieldType, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { documentLabel, readDocument, useDocumentBody, useDocumentHeader } from "@/lib/doc-labels";
import { quote, recordLabel, shortId } from "@/lib/link-labels";
import { canDoc, canEntity, findCommand, findDocType, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";
import { minFraction } from "./decimal-value";
import type { FieldValues } from "./field-input";
import { LinkLabel } from "./field-value";
import { Lookup } from "./lookup";
import { Picker, type PickerOption } from "./picker";
import { StatusBadge } from "./status-badge";

/**
 * What a field can read from the record it is part of: the values of the
 * header (with `company_id`) and, for a field of a line, the line's values.
 */
export interface FieldContext {
  header: FieldValues;
  line?: FieldValues;
  /** The fields of the header and of the line, to name a missing one. */
  fields?: FieldMeta[];
}

function labelOf(context: FieldContext | undefined, name: string): string {
  return context?.fields?.find((field) => field.name === name)?.label ?? name;
}

function text(value: Json | undefined): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** A value of the line the field is in, or else of the header. */
function contextValue(context: FieldContext | undefined, name: string): Json | undefined {
  const own = context?.line?.[name];
  return own !== undefined && own !== null ? own : context?.header[name];
}

export interface ReferenceInputProps {
  field: FieldMeta;
  value: string | null;
  onChange: (value: string | null) => void;
  context?: FieldContext;
  id?: string;
  disabled?: boolean;
  invalid?: boolean;
  "aria-label"?: string;
  "aria-describedby"?: string;
}

// ------------------------------------------------------------ document_link

type DocumentLinkType = Extract<FieldType, { kind: "document_link" }>;

function statesWhere(states: string[]): string | null {
  return states.length > 1 ? `record.status in [${states.map(quote).join(", ")}]` : null;
}

export function DocumentLinkInput(props: ReferenceInputProps & { type: DocumentLinkType }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const { type, value, onChange } = props;
  const docType = findDocType(meta, type.doc_type);
  const header = useDocumentHeader(type.doc_type, value);
  const companyId = text(props.context?.header.company_id) ?? company?.id ?? null;
  const readable = canDoc(meta, type.doc_type, "read");
  return (
    <Picker
      id={props.id}
      aria-label={props["aria-label"]}
      aria-describedby={props["aria-describedby"]}
      invalid={props.invalid}
      disabled={props.disabled || !readable}
      required={props.field.required}
      noun={docType?.plural ?? type.doc_type}
      value={value}
      selectedLabel={header === undefined ? undefined : header ? documentLabel(docType, header) : null}
      onChange={(next) => onChange(next)}
      href={value && readable ? paths.doc(code, type.doc_type, value) : null}
      load={{
        key: companyId ? `doc|${type.doc_type}|${companyId}|${type.states.join(",")}` : null,
        run: async (search, signal) => {
          const page = await api.run(
            "document.list",
            {
              doc_type: type.doc_type,
              company_id: companyId,
              q: search || null,
              status: type.states.length === 1 ? (type.states[0] ?? null) : null,
              where: statesWhere(type.states),
              sort: ["-doc_date", "-created_at"],
              limit: 20,
            },
            { signal },
          );
          return {
            total: page.total,
            options: page.items.map(
              (doc): PickerOption => ({
                key: doc.id,
                label: documentLabel(docType, doc),
                detail: (
                  <span className="flex items-center gap-2">
                    {formatCalendarDate(doc.doc_date)}
                    <StatusBadge status={doc.status} docType={docType} />
                  </span>
                ),
              }),
            ),
          };
        },
      }}
    />
  );
}

export function DocumentLinkValue({
  docType: name,
  id,
  linkify = false,
}: {
  docType: string;
  id: string;
  linkify?: boolean;
}) {
  const { meta } = useApp();
  const { code } = useCompany();
  const docType = findDocType(meta, name);
  const header = useDocumentHeader(name, id);
  if (header === undefined) return <span className="text-muted-foreground">{t("value.loading")}</span>;
  if (header === null) return <span title={id}>{shortId(id)}</span>;
  const label = documentLabel(docType, header);
  if (!linkify || !canDoc(meta, name, "read")) return <span className="whitespace-nowrap">{label}</span>;
  return (
    <Link to={paths.doc(code, name, id)} className="whitespace-nowrap underline underline-offset-2 hover:no-underline">
      {label}
    </Link>
  );
}

// ---------------------------------------------------------------- line_link

type LineLinkType = Extract<FieldType, { kind: "line_link" }>;

/** The lines of a collection of a document, as they are stored. */
function linesOf(body: JsonObject, collection: string): JsonObject[] {
  const lines = body[collection];
  return Array.isArray(lines) ? lines.filter(isObject) : [];
}

/** Labels of the records the link fields of some lines point to, by entity and id. */
async function linkLabels(fields: FieldMeta[], lines: JsonObject[], signal: AbortSignal): Promise<Map<string, string>> {
  const out = new Map<string, string>();
  await Promise.all(
    fields.map(async (field) => {
      if (field.type.kind !== "link") return;
      const { entity, key } = field.type;
      const values = [...new Set(lines.map((line) => text(line[field.name])).filter((v): v is string => v !== null))];
      if (values.length === 0) return;
      try {
        const page = await api.run(
          "entity.list",
          { entity, where: `record.${key} in [${values.map(quote).join(", ")}]`, limit: values.length },
          { signal },
        );
        for (const record of page.items) {
          const value = record[key];
          if (typeof value === "string") out.set(`${entity}|${value}`, recordLabel(entity, record));
        }
      } catch {
        // A line is still told apart by its number and quantities.
      }
    }),
  );
  return out;
}

function useLineDescriber(): (fields: FieldMeta[], line: JsonObject, labels: Map<string, string>) => PickerOption {
  const { currencyDecimals } = useApp();
  const { company } = useCompany();
  const decimals = company ? currencyDecimals[company.base_currency] : undefined;
  return (fields, line, labels) => {
    const names = fields.flatMap((field) => {
      if (field.type.kind !== "link") return [];
      const value = text(line[field.name]);
      return value ? [labels.get(`${field.type.entity}|${value}`) ?? shortId(value)] : [];
    });
    const figures = fields.flatMap((field) => {
      const value = text(line[field.name]);
      if (field.type.kind !== "decimal" || value === null) return [];
      return [`${field.label} ${formatDecimal(value, { minFraction: minFraction(field.decimal, decimals) })}`];
    });
    const number = typeof line.line_no === "number" ? line.line_no : null;
    return {
      key: text(line.id) ?? "",
      label: [number === null ? null : t("lines.line_n", { line: number }), ...names].filter(Boolean).join(" · "),
      detail: figures.join(" · "),
    };
  };
}

export function LineLinkInput(props: ReferenceInputProps & { type: LineLinkType }) {
  const { meta } = useApp();
  const { type, value, onChange } = props;
  const docType = findDocType(meta, type.doc_type);
  const collection = docType?.collections.find((candidate) => candidate.name === type.collection);
  const documentId = text(contextValue(props.context, type.document_field));
  const body = useDocumentBody(type.doc_type, documentId);
  const describe = useLineDescriber();
  const line = body && value ? linesOf(body.body, type.collection).find((candidate) => candidate.id === value) : null;
  return (
    <Picker
      id={props.id}
      aria-label={props["aria-label"]}
      aria-describedby={props["aria-describedby"]}
      invalid={props.invalid}
      disabled={props.disabled}
      required={props.field.required}
      searchable={false}
      noun={collection?.label ?? type.collection}
      value={value}
      selectedLabel={
        value === null
          ? null
          : body === undefined
            ? undefined
            : line
              ? describe(collection?.fields ?? [], line, new Map()).label
              : shortId(value)
      }
      onChange={(next) => onChange(next)}
      blocked={documentId ? null : t("reference.choose_document_first", { label: docType?.label ?? type.doc_type })}
      load={{
        key: documentId ? `line|${type.doc_type}|${documentId}|${type.collection}` : null,
        run: async (_search, signal) => {
          const doc = await readDocument(type.doc_type, documentId ?? "", signal);
          const lines = linesOf(doc.body, type.collection);
          const fields = collection?.fields ?? [];
          const labels = await linkLabels(fields, lines, signal);
          return { options: lines.map((each) => describe(fields, each, labels)) };
        },
      }}
    />
  );
}

export function LineLinkValue({ type, value, context }: { type: LineLinkType; value: string; context?: FieldContext }) {
  const { meta } = useApp();
  const docType = findDocType(meta, type.doc_type);
  const collection = docType?.collections.find((candidate) => candidate.name === type.collection);
  const documentId = text(contextValue(context, type.document_field));
  const body = useDocumentBody(type.doc_type, documentId);
  if (body === undefined) return <span className="text-muted-foreground">{t("value.loading")}</span>;
  const line = body ? linesOf(body.body, type.collection).find((candidate) => candidate.id === value) : undefined;
  if (!line) return <span title={value}>{shortId(value)}</span>;
  const number = typeof line.line_no === "number" ? line.line_no : null;
  const links = (collection?.fields ?? []).filter((field) => field.type.kind === "link");
  return (
    <span className="whitespace-nowrap">
      {number === null ? null : t("lines.line_n", { line: number })}
      {links.map((field) => {
        const linked = text(line[field.name]);
        return field.type.kind === "link" && linked ? (
          <span key={field.name}>
            {t("value.separator")}
            <LinkLabel entity={field.type.entity} keyField={field.type.key} value={linked} />
          </span>
        ) : null;
      })}
    </span>
  );
}

// ------------------------------------------------------------------- lookup

type LookupType = Extract<FieldType, { kind: "lookup" }>;

/** The inputs of a lookup's command, read from the record; `null` while one is missing. */
function lookupParams(type: LookupType, context: FieldContext | undefined): JsonObject | null {
  const input: JsonObject = {};
  for (const [name, param] of Object.entries(type.params)) {
    if (param.from === "value") {
      input[name] = param.value;
      continue;
    }
    const raw = contextValue(context, param.field);
    if (raw === undefined || raw === null || raw === "") return null;
    input[name] = param.map && typeof raw === "string" ? (param.map[raw] ?? raw) : raw;
  }
  return input;
}

type Format = "date" | "decimal" | "text";

/** How each field of a command's output rows is to be shown, from its output schema. */
function rowFormats(command: CommandMeta | undefined, rows: string): Record<string, Format> {
  const schema = command && isObject(command.output_schema) ? command.output_schema : {};
  const resolve = (node: Json | undefined): JsonObject => {
    if (!isObject(node)) return {};
    const ref = node.$ref;
    if (typeof ref === "string" && ref.startsWith("#/")) {
      let current: Json | undefined = schema;
      for (const part of ref.slice(2).split("/")) current = isObject(current) ? current[part] : undefined;
      return isObject(current) ? current : {};
    }
    return node;
  };
  const properties = resolve(schema.properties);
  const items = resolve(resolve(properties[rows]).items);
  const fields = resolve(items.properties);
  const out: Record<string, Format> = {};
  for (const [name, property] of Object.entries(fields)) {
    const p = resolve(property);
    out[name] = p.format === "date" ? "date" : typeof p.pattern === "string" ? "decimal" : "text";
  }
  return out;
}

function useRowDescriber(type: LookupType): (row: JsonObject) => PickerOption {
  const { meta, currencyDecimals } = useApp();
  const { company } = useCompany();
  const decimals = company ? currencyDecimals[company.base_currency] : undefined;
  const formats = rowFormats(findCommand(meta, type.command), type.rows);
  return (row) => {
    const parts = type.display.flatMap((name) => {
      const value = text(row[name]) ?? (typeof row[name] === "number" ? String(row[name]) : null);
      if (value === null) return [];
      if (formats[name] === "date") return [formatCalendarDate(value)];
      if (formats[name] === "decimal") return [formatDecimal(value, { minFraction: decimals ?? 2 })];
      return [value];
    });
    const [first, ...rest] = parts;
    return { key: text(row[type.key]) ?? "", label: first ?? "", detail: rest.join(" · ") };
  };
}

/** `limit` for a command that takes one; a command that lists everything (`party.get`) refuses it. */
function withLimit(command: CommandMeta | undefined, input: JsonObject, limit: number): JsonObject {
  const schema = command && isObject(command.input_schema) ? command.input_schema : {};
  const properties = isObject(schema.properties) ? schema.properties : {};
  return "limit" in properties ? { ...input, limit } : input;
}

async function lookupRows(
  type: LookupType,
  input: JsonObject,
  signal: AbortSignal,
): Promise<{ rows: JsonObject[]; total?: number }> {
  const output = await api.runNamed(type.command, input, { signal });
  if (!isObject(output)) return { rows: [] };
  const rows = output[type.rows];
  return {
    rows: Array.isArray(rows) ? rows.filter(isObject) : [],
    total: typeof output.total === "number" ? output.total : undefined,
  };
}

export function CommandLookupInput(props: ReferenceInputProps & { type: LookupType }) {
  const { meta } = useApp();
  const { type, value, onChange } = props;
  const command = findCommand(meta, type.command);
  const input = lookupParams(type, props.context);
  const describe = useRowDescriber(type);
  const inputKey = input ? JSON.stringify(input) : null;
  const selected = useLookupLabel(type, value, input);
  const missing = Object.values(type.params).flatMap((param) =>
    param.from === "field" && contextValue(props.context, param.field) == null
      ? [labelOf(props.context, param.field)]
      : [],
  );
  const missingLabel = missing.join(", ");
  return (
    <Picker
      id={props.id}
      aria-label={props["aria-label"]}
      aria-describedby={props["aria-describedby"]}
      invalid={props.invalid}
      disabled={props.disabled}
      required={props.field.required}
      searchable={false}
      noun={props.field.label}
      value={value}
      selectedLabel={selected}
      onChange={(next) => onChange(next)}
      blocked={input ? null : t("reference.fill_first", { fields: missingLabel })}
      load={{
        key: inputKey && command ? `lookup|${type.command}|${inputKey}` : null,
        run: async (_search, signal) => {
          const { rows, total } = await lookupRows(type, withLimit(command, input ?? {}, 100), signal);
          return { total, options: rows.map(describe) };
        },
      }}
    />
  );
}

/**
 * What a lookup value shows: the row of the command it was chosen from. A row
 * no longer listed (an open item since settled) is shown by its id.
 */
function useLookupLabel(type: LookupType, value: string | null, input: JsonObject | null): string | null | undefined {
  const { meta } = useApp();
  const command = findCommand(meta, type.command);
  const describe = useRowDescriber(type);
  // The lookup's command reads its rows by key where the lookup says how
  // (`by_keys`), whether or not the row could still be chosen.
  const byKeys = type.by_keys;
  const { data: found } = useAsync(
    value && input ? `${type.command}|${JSON.stringify(input)}|${value}` : null,
    async (signal) => {
      const query: JsonObject = byKeys
        ? { ...(input ?? {}), [byKeys]: [value ?? ""] }
        : withLimit(command, input ?? {}, 500);
      const { rows } = await lookupRows(type, query, signal);
      const row = rows.find((candidate) => candidate[type.key] === value);
      return row ? describe(row).label : null;
    },
  );
  if (value === null) return null;
  if (!input) return shortId(value);
  if (found === undefined) return undefined;
  return found ?? shortId(value);
}

export function CommandLookupValue({
  type,
  value,
  context,
}: {
  type: LookupType;
  value: string;
  context?: FieldContext;
}) {
  const label = useLookupLabel(type, value, lookupParams(type, context));
  if (label === undefined) return <span className="text-muted-foreground">{t("value.loading")}</span>;
  return <span title={value}>{label}</span>;
}

// -------------------------------------------------------------- entity_link

type EntityLinkType = Extract<FieldType, { kind: "entity_link" }>;

/** The entity an entity link points into, from the field that names it. */
export function useLinkedEntity(type: EntityLinkType, context: FieldContext | undefined): string | null {
  const { meta } = useApp();
  const name = text(contextValue(context, type.entity_field));
  return name && findEntity(meta, name) && canEntity(meta, name, "read") ? name : null;
}

export function EntityLinkInput(props: ReferenceInputProps & { type: EntityLinkType; companyId?: string | null }) {
  const entity = useLinkedEntity(props.type, props.context);
  const named = contextValue(props.context, props.type.entity_field);
  if (!entity) {
    return (
      <Picker
        id={props.id}
        aria-label={props["aria-label"]}
        disabled
        noun={props.field.label}
        value={props.value}
        selectedLabel={props.value ? shortId(props.value) : null}
        onChange={() => {}}
        blocked={
          named
            ? t("reference.unknown_entity", { name: String(named) })
            : t("reference.fill_first", { fields: labelOf(props.context, props.type.entity_field) })
        }
        load={{ key: null, run: async () => ({ options: [] }) }}
      />
    );
  }
  return (
    <Lookup
      id={props.id}
      entity={entity}
      keyField="id"
      value={props.value}
      onChange={(next) => props.onChange(next)}
      companyId={props.companyId ?? text(props.context?.header.company_id)}
      required={props.field.required}
      disabled={props.disabled}
      invalid={props.invalid}
      aria-label={props["aria-label"]}
    />
  );
}

export function EntityLinkValue({
  type,
  value,
  context,
  linkify,
}: {
  type: EntityLinkType;
  value: string;
  context?: FieldContext;
  linkify?: boolean;
}) {
  const entity = useLinkedEntity(type, context);
  if (!entity) return <span title={value}>{shortId(value)}</span>;
  return <LinkLabel entity={entity} keyField="id" value={value} linkify={linkify} />;
}

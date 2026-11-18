import { Link } from "react-router";
import type { FieldMeta, Json } from "@/api/types";
import { useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { shortId, useLinkInfo } from "@/lib/link-labels";
import { enumLabel } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { DecimalValue, type RowContext } from "./decimal-value";
import { CommandLookupValue, DocumentLinkValue, EntityLinkValue, type FieldContext, LineLinkValue } from "./references";

export function isNumeric(field: FieldMeta): boolean {
  return field.type.kind === "decimal" || field.type.kind === "int";
}

/** A decimal string for display: right-aligned in tabular digits by its cell, never rounded. */
export function Amount({ value, decimals = 2 }: { value: string | null | undefined; decimals?: number }) {
  if (value === null || value === undefined || value === "") return <Empty />;
  return <span className="whitespace-nowrap tabular-nums">{formatDecimal(value, { minFraction: decimals })}</span>;
}

/** A value the user's field rules withhold on this record. */
export function Withheld() {
  return (
    <span className="text-muted-foreground" title={t("value.withheld_title")}>
      {t("value.withheld")}
    </span>
  );
}

export function Empty() {
  return (
    <span className="text-muted-foreground" title={t("value.none_title")}>
      {t("value.none")}
    </span>
  );
}

/** The name of the record a link points to, optionally as a link to it. */
export function LinkLabel({
  entity,
  keyField,
  value,
  linkify = false,
}: {
  entity: string;
  keyField: string;
  value: string;
  linkify?: boolean;
}) {
  const { code } = useCompany();
  const info = useLinkInfo(entity, keyField, value);
  if (info === undefined) return <span className="text-muted-foreground">{t("value.loading")}</span>;
  if (info === null) return <span title={value}>{keyField === "id" ? shortId(value) : value}</span>;
  if (!linkify) return <span>{info.label}</span>;
  return (
    <Link to={paths.entityRecord(code, entity, info.id)} className="underline underline-offset-2 hover:no-underline">
      {info.label}
    </Link>
  );
}

/** One field of a record, for reading. */
export function FieldValue({
  field,
  value,
  linkify = false,
  context,
  bare = false,
}: {
  field: FieldMeta;
  value: Json | undefined;
  linkify?: boolean;
  /** The record or line the value is part of, for references and units that depend on other fields. */
  context?: FieldContext;
  /** A decimal without its currency or unit, where a column header names them. */
  bare?: boolean;
}) {
  if (value === null || value === undefined || value === "") return <Empty />;
  const type = field.type;
  const asText = typeof value === "string" ? value : null;
  switch (type.kind) {
    case "bool":
      return <span>{value === true ? t("value.yes") : t("value.no")}</span>;
    case "decimal":
      return asText !== null ? (
        <DecimalValue field={field} value={asText} bare={bare} row={toRowContext(context)} />
      ) : (
        <span>{String(value)}</span>
      );
    case "int":
      return <span className="tabular-nums">{String(value)}</span>;
    case "date":
      return <span className="whitespace-nowrap">{asText !== null ? formatCalendarDate(asText) : String(value)}</span>;
    case "timestamp":
      return <span className="whitespace-nowrap">{asText !== null ? formatTimestamp(asText) : String(value)}</span>;
    case "enum":
      return <span>{asText !== null ? enumLabel(field, asText) : String(value)}</span>;
    case "link":
      return asText !== null ? (
        <LinkLabel entity={type.entity} keyField={type.key} value={asText} linkify={linkify} />
      ) : (
        <span>{String(value)}</span>
      );
    case "document_link":
      return asText !== null ? <DocumentLinkValue docType={type.doc_type} id={asText} linkify={linkify} /> : null;
    case "line_link":
      return asText !== null ? <LineLinkValue type={type} value={asText} context={context} /> : null;
    case "lookup":
      return asText !== null ? <CommandLookupValue type={type} value={asText} context={context} /> : null;
    case "entity_link":
      return asText !== null ? (
        <EntityLinkValue type={type} value={asText} context={context} linkify={linkify} />
      ) : null;
    case "uuid":
    case "posting":
      return asText !== null ? (
        <span className="font-mono text-xs" title={asText}>
          {shortId(asText)}
        </span>
      ) : (
        <span>{String(value)}</span>
      );
    case "text_array":
      return <span>{Array.isArray(value) ? value.join(", ") : String(value)}</span>;
    case "json":
      return <span className="block max-w-64 truncate font-mono text-xs">{JSON.stringify(value)}</span>;
    default:
      return <span>{asText ?? JSON.stringify(value)}</span>;
  }
}

/** The record or line a field is part of, as a decimal reads its currency or unit from it. */
export function toRowContext(context: FieldContext | undefined): RowContext | undefined {
  return context
    ? {
        values: { ...context.header, ...context.line },
        fields: context.fields ?? [],
        document: context.line ? context.header : undefined,
      }
    : undefined;
}

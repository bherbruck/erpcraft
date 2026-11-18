import { Link } from "@astryxdesign/core/Link";
import { Text } from "@astryxdesign/core/Text";
import type { FieldMeta, Json } from "@/api/types";
import { useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { shortId, useLinkInfo } from "@/lib/link-labels";
import { enumLabel } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { DecimalValue, type RowContext } from "./decimal-value";
import { CommandLookupValue, DocumentLinkValue, EntityLinkValue, type FieldContext, LineLinkValue } from "./references";

export function isNumeric(field: FieldMeta): boolean {
  return field.type.kind === "decimal" || field.type.kind === "int";
}

/** A value the user's field rules withhold on this record. */
export function Withheld() {
  return (
    <span title={t("value.withheld_title")}>
      <Text color="secondary">{t("value.withheld")}</Text>
    </span>
  );
}

export function Empty() {
  return (
    <span title={t("value.none_title")}>
      <Text color="secondary">{t("value.none")}</Text>
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
  if (info === undefined) return <Text color="secondary">{t("value.loading")}</Text>;
  if (info === null) return <span title={value}>{keyField === "id" ? shortId(value) : value}</span>;
  if (!linkify) return <span>{info.label}</span>;
  return <Link href={paths.entityRecord(code, entity, info.id)}>{info.label}</Link>;
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
  context?: FieldContext;
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
      return <Text hasTabularNumbers>{String(value)}</Text>;
    case "date":
      return <span className="nowrap">{asText !== null ? formatCalendarDate(asText) : String(value)}</span>;
    case "timestamp":
      return <span className="nowrap">{asText !== null ? formatTimestamp(asText) : String(value)}</span>;
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
        <span title={asText}>
          <Text type="code">{shortId(asText)}</Text>
        </span>
      ) : (
        <span>{String(value)}</span>
      );
    case "text_array":
      return <span>{Array.isArray(value) ? value.join(", ") : String(value)}</span>;
    case "json":
      return (
        <Text type="code" maxLines={1}>
          {JSON.stringify(value)}
        </Text>
      );
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

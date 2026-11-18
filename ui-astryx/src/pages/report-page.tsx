// Every report is a command the registry marks with `report`. The spike gives every one the
// generic page (as ../ui does for any report a module adds): its inputs as a filter row, its
// rows as a table, both read from the command's JSON Schemas. ../ui's hand-laid-out pages
// (statements, trial balance, aging, …) are not ported; they render generically here.

import { Card } from "@astryxdesign/core/Card";
import { CheckboxInput } from "@astryxdesign/core/CheckboxInput";
import { DateInput } from "@astryxdesign/core/DateInput";
import { HStack } from "@astryxdesign/core/HStack";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { Selector } from "@astryxdesign/core/Selector";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import type { ISODateString } from "@astryxdesign/core/utils";
import { VStack } from "@astryxdesign/core/VStack";
import { type ReactNode, useMemo } from "react";
import { AgingPage } from "./reports/aging";
import { BankReconciliationPage } from "./reports/bank-reconciliation";
import { BalanceSheetPage, IncomeStatementPage } from "./reports/statements";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { CommandMeta, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { Empty } from "@/components/field-value";
import { FieldInput } from "@/components/field-input";
import { Page } from "@/components/page";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { formatDecimal } from "@/lib/decimal";
import { canRun, commandInputs, findCommand, type InputProperty, reportTitle } from "@/lib/meta";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

// Reports with a page laid out for what they show, by command name; every other report is generic.
const CUSTOM: Record<string, () => ReactNode> = {
  "gl.balance_sheet": BalanceSheetPage,
  "gl.income_statement": IncomeStatementPage,
  "open_item.aging": AgingPage,
  "bank.reconciliation": BankReconciliationPage,
};

export function ReportPage() {
  const { meta } = useApp();
  const params = useParams();
  const command = findCommand(meta, params.report);
  if (!command || command.report === null || !canRun(meta, command.name)) return <NotFoundPage />;
  const Custom = CUSTOM[command.name];
  return Custom ? <Custom /> : <GenericReport key={command.name} command={command} />;
}

export default ReportPage;

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

type Format = "date" | "timestamp" | "decimal" | "integer" | "boolean" | "text";

interface OutputColumn {
  name: string;
  label: string;
  format: Format;
}

function resolver(root: JsonObject): (node: Json | undefined) => JsonObject {
  const resolve = (node: Json | undefined): JsonObject => {
    if (!isObject(node)) return {};
    if (typeof node.$ref === "string" && node.$ref.startsWith("#/")) {
      let current: Json | undefined = root;
      for (const part of node.$ref.slice(2).split("/")) current = isObject(current) ? current[part] : undefined;
      return resolve(current);
    }
    const options = node.anyOf ?? node.oneOf;
    if (Array.isArray(options)) {
      const first = options.map(resolve).find((option) => option.type !== "null" && Object.keys(option).length > 0);
      if (first) return first;
    }
    return node;
  };
  return resolve;
}

const rank = (format: Format): number => (format === "decimal" || format === "integer" ? 1 : 0);

function formatOf(property: JsonObject): Format {
  const types = Array.isArray(property.type) ? property.type : [property.type];
  if (property.format === "date") return "date";
  if (property.format === "date-time") return "timestamp";
  if (typeof property.pattern === "string") return "decimal";
  if (types.includes("integer") || types.includes("number")) return "integer";
  if (types.includes("boolean")) return "boolean";
  return "text";
}

const columnLabel = (name: string, property: JsonObject): string =>
  typeof property.title === "string" ? property.title : name;

/** The rows of a report's output: its first array of objects, and how to show each field. */
function outputShape(command: CommandMeta): { rows: string | null; columns: OutputColumn[]; totals: OutputColumn[] } {
  const root = isObject(command.output_schema) ? command.output_schema : {};
  const resolve = resolver(root);
  const properties = resolve(root).properties;
  const top = isObject(properties) ? properties : {};
  let rows: string | null = null;
  let columns: OutputColumn[] = [];
  const totals: OutputColumn[] = [];
  for (const [name, raw] of Object.entries(top)) {
    const property = resolve(raw);
    const items = resolve(property.items);
    if (rows === null && property.type === "array" && isObject(items.properties)) {
      rows = name;
      columns = Object.entries(items.properties)
        .filter(([, inner]) => resolve(inner).format !== "uuid")
        .map(([column, inner]) => ({ name: column, label: columnLabel(column, resolve(inner)), format: formatOf(resolve(inner)) }))
        .sort((x, y) => rank(x.format) - rank(y.format));
    } else if (formatOf(property) === "decimal" || (formatOf(property) === "integer" && name !== "total")) {
      totals.push({ name, label: columnLabel(name, property), format: formatOf(property) });
    }
  }
  return { rows, columns, totals };
}

function Cell({ value, format, decimals }: { value: Json | undefined; format: Format; decimals: number }) {
  if (value === null || value === undefined || value === "") return <Empty />;
  if (typeof value === "string") {
    if (format === "date") return <span className="nowrap">{formatCalendarDate(value)}</span>;
    if (format === "timestamp") return <span className="nowrap">{formatTimestamp(value)}</span>;
    if (format === "decimal")
      return (
        <Text hasTabularNumbers>
          <span className="nowrap">{formatDecimal(value, { minFraction: decimals })}</span>
        </Text>
      );
    return <span>{value}</span>;
  }
  if (typeof value === "boolean") return <span>{value ? t("value.yes") : t("value.no")}</span>;
  if (typeof value === "number") return <Text hasTabularNumbers>{String(value)}</Text>;
  return (
    <Text type="code" maxLines={1}>
      {JSON.stringify(value)}
    </Text>
  );
}

const PAGE_SIZE = 50;
const PAGING = new Set(["limit", "offset"]);

function GenericReport({ command }: { command: CommandMeta }) {
  const { currencyDecimals } = useApp();
  const { company } = useCompany();
  const url = useUrlState();
  const inputs = useMemo(() => commandInputs(command), [command]);
  const shape = useMemo(() => outputShape(command), [command]);
  const paged = inputs.some((input) => input.name === "limit") && inputs.some((input) => input.name === "offset");
  const page = pageFromUrl(url.get("page"));
  const needsCompany = inputs.some((input) => input.name === "company_id" && input.required);
  const filters = inputs.filter((input) => !PAGING.has(input.name) && input.name !== "company_id");

  const input: JsonObject = {};
  for (const property of filters) {
    const value = url.get(property.name);
    if (value === "") continue;
    input[property.name] = property.type === "boolean" ? value === "1" : property.type === "integer" ? parseInt(value, 10) : value;
  }
  if (inputs.some((each) => each.name === "company_id") && company) input.company_id = company.id;
  if (paged) {
    input.limit = PAGE_SIZE;
    input.offset = (page - 1) * PAGE_SIZE;
  }
  const missing = filters.some((property) => property.required && !(property.name in input));
  const ready = !missing && (!needsCompany || company !== null);
  const report = useAsync(ready ? JSON.stringify(input) : null, (signal) => api.runNamed(command.name, input, { signal }));

  const data = isObject(report.data) ? report.data : undefined;
  const listed = shape.rows && data ? data[shape.rows] : undefined;
  const rows = Array.isArray(listed) ? listed : undefined;
  const total = data && typeof data.total === "number" ? data.total : undefined;
  const decimals = company ? (currencyDecimals[company.base_currency] ?? 2) : 2;
  const columns = shape.columns.map(
    (column): Column<Json> => ({
      id: column.name,
      header: column.label,
      // The grid formats, sorts and copies by type: decimals stay exact strings.
      type: column.format === "decimal" ? "money" : column.format === "integer" ? "number" : column.format === "date" ? "date" : column.format === "timestamp" ? "datetime" : column.format === "boolean" ? "boolean" : "text",
      minFraction: column.format === "decimal" ? decimals : undefined,
      value: (row) => {
        const value = isObject(row) ? row[column.name] : undefined;
        return typeof value === "number" ? String(value) : value === undefined ? null : typeof value === "object" && value !== null ? JSON.stringify(value) : value;
      },
      text:
        column.format === "date"
          ? (row) => (isObject(row) && typeof row[column.name] === "string" ? formatCalendarDate(row[column.name] as string) : "")
          : column.format === "timestamp"
            ? (row) => (isObject(row) && typeof row[column.name] === "string" ? formatTimestamp(row[column.name] as string) : "")
            : undefined,
    }),
  );
  const title = reportTitle(command);

  /** One input of the report as a filter, kept in the URL. A link the server describes is a lookup. */
  function filterInput(property: InputProperty) {
    const value = url.get(property.name);
    const set = (next: string | null): void => url.set({ [property.name]: next ?? null, page: null }, { replace: true });
    if (property.fieldType) {
      const field: FieldMeta = {
        name: property.name,
        label: property.label,
        type: property.fieldType,
        required: property.required,
        readonly: false,
        immutable: false,
        decimal: null,
        default: null,
        help: property.description,
      };
      return (
        <div key={property.name} className="report-filter">
          <FieldInput field={field} value={value || null} onChange={(next) => set(typeof next === "string" ? next : null)} companyId={company?.id} />
        </div>
      );
    }
    if (property.format === "date")
      return (
        <DateInput
          key={property.name}
          label={property.label}
          labelTooltip={property.description ?? undefined}
          isRequired={property.required}
          format="date"
          hasClear
          value={value ? (value as ISODateString) : undefined}
          onChange={(next) => set(next ?? null)}
        />
      );
    if (property.options)
      return (
        <Selector
          key={property.name}
          label={property.label}
          labelTooltip={property.description ?? undefined}
          isRequired={property.required}
          value={value}
          hasClear
          placeholder={t("report.any")}
          onChange={(next: string | null) => set(next)}
          options={property.options.map((o) => ({ value: o.value, label: o.label }))}
        />
      );
    if (property.type === "boolean")
      return <CheckboxInput key={property.name} label={property.label} value={value === "1"} onChange={(on) => set(on ? "1" : null)} />;
    return (
      <TextInput
        key={property.name}
        label={property.label}
        labelTooltip={property.description ?? undefined}
        isRequired={property.required}
        value={value}
        onChange={(next) => set(next)}
      />
    );
  }

  return (
    <Page
      title={title}
      padded={false}
      toolbar={
        <VStack gap={3}>
          {command.description ? <Text type="supporting">{command.description}</Text> : null}
          {filters.length > 0 ? (
            <HStack gap={3} wrap="wrap" vAlign="end">
              {filters.map(filterInput)}
            </HStack>
          ) : null}
          {shape.totals.length > 0 && data ? (
            <Card padding={2} variant="muted">
              <MetadataList orientation="horizontal">
                {shape.totals.map((column) => (
                  <MetadataListItem key={column.name} label={column.label}>
                    <Cell value={data[column.name]} format={column.format} decimals={decimals} />
                  </MetadataListItem>
                ))}
              </MetadataList>
            </Card>
          ) : null}
        </VStack>
      }
    >
      <DataTable
        id={`report.${command.name}`}
        fill
        caption={title}
        columns={columns}
        rows={ready ? rows : []}
        rowKey={(row) => (isObject(row) && typeof row.id === "string" ? row.id : JSON.stringify(row))}
        loading={report.loading}
        error={report.error}
        onRetry={report.reload}
        page={paged && total !== undefined ? page : undefined}
        pageSize={paged ? PAGE_SIZE : undefined}
        total={total}
        onPage={paged ? (next) => url.set({ page: next === 1 ? null : String(next) }) : undefined}
        empty={{
          title: needsCompany && !company ? t("report.needs_company") : missing ? t("report.fill_inputs") : t("report.empty"),
        }}
      />
    </Page>
  );
}

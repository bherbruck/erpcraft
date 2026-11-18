// Every report is a command the registry marks with `report`. A few have a page
// of their own, laid out for what they show; any other report, such as one a
// module adds, gets the generic page: its inputs as a filter bar, its rows as
// a table, both read from the command's JSON Schemas.

import { type ReactNode, useMemo } from "react";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { CommandMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Amount, Empty } from "@/components/field-value";
import { PageHeader } from "@/components/page";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { canRun, commandInputs, findCommand, type InputProperty, reportTitle } from "@/lib/meta";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { AccountLedgerPage } from "./account-ledger";
import { AgingPage } from "./aging";
import { BankReconciliationPage } from "./bank-reconciliation";
import { AssetRegisterPage, AssetSchedulePage } from "./fixed-assets";
import { NotFoundPage } from "./not-found";
import { OpenItemsPage } from "./open-items";
import { BalanceSheetPage, IncomeStatementPage } from "./statements";
import { StockOnHandPage } from "./stock-on-hand";
import { TrialBalancePage } from "./trial-balance";

// Reports with a page of their own, by command name.
const CUSTOM: Record<string, () => ReactNode> = {
  "gl.trial_balance": TrialBalancePage,
  "gl.balance_sheet": BalanceSheetPage,
  "gl.income_statement": IncomeStatementPage,
  "gl.account_ledger": AccountLedgerPage,
  "open_item.list": OpenItemsPage,
  "stock.on_hand": StockOnHandPage,
  "bank.reconciliation": BankReconciliationPage,
  "open_item.aging": AgingPage,
  "asset.register": AssetRegisterPage,
  "asset.schedule": AssetSchedulePage,
};

export function ReportPage() {
  const { meta } = useApp();
  const params = useParams();
  const command = findCommand(meta, params.report);
  if (!command || command.report === null || !canRun(meta, command.name)) return <NotFoundPage />;
  const Custom = CUSTOM[command.name];
  return Custom ? <Custom /> : <GenericReport key={command.name} command={command} />;
}

// ------------------------------------------------------------ the schemas

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

function rank(format: Format): number {
  return format === "decimal" || format === "integer" ? 1 : 0;
}

function formatOf(property: JsonObject): Format {
  const types = Array.isArray(property.type) ? property.type : [property.type];
  if (property.format === "date") return "date";
  if (property.format === "date-time") return "timestamp";
  if (typeof property.pattern === "string") return "decimal";
  if (types.includes("integer") || types.includes("number")) return "integer";
  if (types.includes("boolean")) return "boolean";
  return "text";
}

/** What an output column is called: its title in the schema, or else its name. */
function columnLabel(name: string, property: JsonObject): string {
  return typeof property.title === "string" ? property.title : name;
}

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
        .map(([column, inner]) => ({
          name: column,
          label: columnLabel(column, resolve(inner)),
          format: formatOf(resolve(inner)),
        }))
        // What a row is (codes, names, dates) before what it comes to; the schema lists fields alphabetically.
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
    if (format === "date") return <span className="whitespace-nowrap">{formatCalendarDate(value)}</span>;
    if (format === "timestamp") return <span className="whitespace-nowrap">{formatTimestamp(value)}</span>;
    if (format === "decimal") return <Amount value={value} decimals={decimals} />;
    return <span>{value}</span>;
  }
  if (typeof value === "boolean") return <span>{value ? t("value.yes") : t("value.no")}</span>;
  if (typeof value === "number") return <span className="tabular-nums">{String(value)}</span>;
  return <span className="block max-w-64 truncate font-mono text-xs">{JSON.stringify(value)}</span>;
}

// ------------------------------------------------------------ the page

const PAGE_SIZE = 50;
const PAGING = new Set(["limit", "offset"]);

function enumValues(command: CommandMeta, name: string): string[] | null {
  const root = isObject(command.input_schema) ? command.input_schema : {};
  const resolve = resolver(root);
  const properties = isObject(root.properties) ? root.properties : {};
  const property = resolve(properties[name]);
  return Array.isArray(property.enum) ? property.enum.filter((v): v is string => typeof v === "string") : null;
}

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
    input[property.name] =
      property.type === "boolean" ? value === "1" : property.type === "integer" ? parseInt(value, 10) : value;
  }
  if (inputs.some((each) => each.name === "company_id") && company) input.company_id = company.id;
  if (paged) {
    input.limit = PAGE_SIZE;
    input.offset = (page - 1) * PAGE_SIZE;
  }
  const missing = filters.some((property) => property.required && !(property.name in input));
  const ready = !missing && (!needsCompany || company !== null);
  const key = ready ? JSON.stringify(input) : null;
  const report = useAsync(key, (signal) => api.runNamed(command.name, input, { signal }));

  const data = isObject(report.data) ? report.data : undefined;
  const listed = shape.rows && data ? data[shape.rows] : undefined;
  const rows = Array.isArray(listed) ? listed : undefined;
  const total = data && typeof data.total === "number" ? data.total : undefined;
  const decimals = company ? (currencyDecimals[company.base_currency] ?? 2) : 2;
  const columns = shape.columns.map(
    (column): Column<Json> => ({
      id: column.name,
      header: column.label,
      align: column.format === "decimal" || column.format === "integer" ? "end" : "start",
      cell: (row) => (
        <Cell value={isObject(row) ? row[column.name] : undefined} format={column.format} decimals={decimals} />
      ),
    }),
  );
  const title = reportTitle(command);

  function filterInput(property: InputProperty) {
    const id = `report-${property.name}`;
    const value = url.get(property.name);
    const set = (next: string): void => url.set({ [property.name]: next, page: null }, { replace: true });
    const options = enumValues(command, property.name);
    return (
      <div key={property.name} className="space-y-1">
        <Label htmlFor={id} title={property.description ?? undefined}>
          {property.label}
        </Label>
        {property.format === "date" ? (
          <DateInput id={id} className="w-44" value={value} onChange={set} />
        ) : options ? (
          <NativeSelect id={id} className="w-44" value={value} onChange={(event) => set(event.target.value)}>
            <option value="">{t("report.any")}</option>
            {options.map((option) => (
              <option key={option} value={option}>
                {option}
              </option>
            ))}
          </NativeSelect>
        ) : property.type === "boolean" ? (
          <span className="flex h-8 items-center">
            <Checkbox id={id} checked={value === "1"} onCheckedChange={(checked) => set(checked === true ? "1" : "")} />
          </span>
        ) : (
          <Input id={id} className="w-56" value={value} onChange={(event) => set(event.target.value)} />
        )}
      </div>
    );
  }

  return (
    <>
      <PageHeader title={title} />
      <p className="mb-3 text-sm text-muted-foreground">{command.description}</p>
      {filters.length > 0 ? (
        <div className="mb-3 flex flex-wrap items-end gap-3">{filters.map(filterInput)}</div>
      ) : null}
      {shape.totals.length > 0 && data ? (
        <dl className="mb-3 flex flex-wrap gap-x-6 gap-y-1 text-sm">
          {shape.totals.map((column) => (
            <div key={column.name} className="flex gap-2">
              <dt className="text-muted-foreground">{column.label}</dt>
              <dd className="font-medium">
                <Cell value={data[column.name]} format={column.format} decimals={decimals} />
              </dd>
            </div>
          ))}
        </dl>
      ) : null}
      <DataTable
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
        empty={
          <p>
            {needsCompany && !company
              ? t("report.needs_company")
              : missing
                ? t("report.fill_inputs")
                : t("report.empty")}
          </p>
        }
      />
    </>
  );
}

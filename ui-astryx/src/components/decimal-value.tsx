// Decimals shown by what they mean. The registry says of each decimal field
// whether it is money, a unit price, a quantity, a rate or a percent, how many
// places it can have, and where its currency or unit of measure is.

import { Text } from "@astryxdesign/core/Text";
import type { DecimalMeta, FieldMeta, Json, ValueSource } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatDecimal } from "@/lib/decimal";
import { useLinkInfo } from "@/lib/link-labels";
import type { FieldValues } from "./field-input";

/** The record or line a value is part of: where a currency or unit is read from. */
export interface RowContext {
  values: FieldValues;
  fields: FieldMeta[];
  /** The header of the document a line belongs to, for a value whose source is the document. */
  document?: FieldValues;
}

/** Decimals always shown for a value of this kind, given its currency's decimals. */
export function minFraction(decimal: DecimalMeta | null, currencyDecimals: number | undefined): number {
  if (!decimal) return 0;
  if (decimal.role === "money" || decimal.role === "unit_price") return currencyDecimals ?? 2;
  return 0;
}

function text(value: Json | undefined): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/**
 * Where a value source points, as far as it can be known without reading
 * anything: the company's currency, another field of the row, or a field of
 * the header of the document the row is a line of. `undefined` for a source
 * that names a linked record, which takes a read.
 */
export function plainSource(
  source: ValueSource | null,
  row: RowContext | undefined,
  companyCurrency: string | null,
): string | null | undefined {
  if (!source) return null;
  // Read as a string: sources a newer server declares are recognized here before the types know them.
  const from: string = source.from;
  const field = "field" in source ? source.field : null;
  if (from === "company") return companyCurrency;
  if (from === "field" && field) return text(row?.values[field]);
  if (from === "document" && field) return text(row?.document?.[field] ?? row?.values[field]);
  if (from === "linked") return undefined;
  return null;
}

/**
 * Where a value source points: the company's currency, another field of the
 * row, the document's header, or a field of the record another field of the
 * row links to.
 */
function useSource(source: ValueSource | null, row: RowContext | undefined): string | null {
  const { company } = useCompany();
  const linkField =
    source?.from === "linked" ? row?.fields.find((candidate) => candidate.name === source.field) : undefined;
  const linkType = linkField?.type.kind === "link" ? linkField.type : null;
  const linked = useLinkInfo(
    linkType?.entity ?? null,
    linkType?.key ?? "id",
    source?.from === "linked" ? text(row?.values[source.field]) : null,
  );
  const plain = plainSource(source, row, company?.base_currency || null);
  if (plain !== undefined) return plain;
  return linked && source?.from === "linked" ? text(linked.record[source.key]) : null;
}

/** The currency an amount is in, where the registry says. */
export function useCurrency(decimal: DecimalMeta | null, row?: RowContext): string | null {
  return useSource(decimal?.currency ?? null, row);
}

/** Decimals always shown for a value of this field, as read-only values show it. */
export function useMinFraction(decimal: DecimalMeta | null, row?: RowContext): number {
  const { currencyDecimals } = useApp();
  const currency = useCurrency(decimal, row);
  return minFraction(decimal, currency ? currencyDecimals[currency] : undefined);
}

/**
 * The header of a column of decimals. Money in the company's currency names
 * the currency once in the header, rather than on every row.
 */
export function useDecimalHeader(field: FieldMeta): string {
  const { company } = useCompany();
  const currency = field.decimal?.currency?.from === "company" ? company?.base_currency : null;
  return currency ? t("value.in_currency", { label: field.label, currency }) : field.label;
}

/** Whether the currency of this field is shown in its column header instead of beside each value. */
export function currencyInHeader(field: FieldMeta): boolean {
  return field.decimal?.currency?.from === "company";
}

/** A decimal string, formatted for its role, with its currency or unit where they vary by row. */
export function DecimalValue({
  field,
  value,
  row,
  bare = false,
}: {
  field: FieldMeta;
  value: string;
  row?: RowContext;
  /** Leave out the currency and unit, as where the header names them. */
  bare?: boolean;
}) {
  const { currencyDecimals } = useApp();
  const decimal = field.decimal;
  const currency = useCurrency(decimal, row);
  const unit = useSource(decimal?.unit ?? null, row);
  const formatted = formatDecimal(value, {
    minFraction: minFraction(decimal, currency ? currencyDecimals[currency] : undefined),
  });
  const suffix =
    bare || !decimal
      ? null
      : decimal.role === "percent"
        ? t("value.percent_sign")
        : decimal.role === "quantity"
          ? unit
          : (decimal.role === "money" || decimal.role === "unit_price") && !currencyInHeader(field)
            ? currency
            : null;
  return (
    <Text hasTabularNumbers>
      <span className="nowrap">
        {formatted}
        {suffix ? <Text type="supporting"> {suffix}</Text> : null}
      </span>
    </Text>
  );
}

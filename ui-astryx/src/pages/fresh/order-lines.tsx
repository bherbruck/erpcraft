// The lines of a sales order as an Astryx Table: rows, compact density, one small control per
// cell. Astryx's layout guidance renders a columnar collection as a Table; its renderCell takes
// any component, so the cells hold the same inputs the header uses.

import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { IconButton } from "@astryxdesign/core/IconButton";
import { pixel, proportional } from "@astryxdesign/core/Table";
import { Table } from "@astryxdesign/core/Table";
import type { TableColumn } from "@astryxdesign/core/Table";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { Plus, Trash2 } from "lucide-react";
import type { CollectionMeta, FieldMeta } from "@/api/types";
import { useEffect } from "react";
import { t } from "@/i18n/t";
import { formatDecimal, isDecimalDraft } from "@/lib/decimal";
import { useLinkInfo } from "@/lib/link-labels";
import { LineWhatLookup, RecordLookup } from "./lookups";
import { KIND_FIELD, type LineField, type LineKind, lineKind, type OrderLine, type Problems } from "./order-model";

interface Row extends Record<string, unknown> {
  line: OrderLine;
  number: number;
  net: string | null;
}

export interface OrderLinesProps {
  collection: CollectionMeta;
  lines: OrderLine[];
  /** Net amounts as the server worked them out, by line number. */
  nets: (string | null)[];
  editable: boolean;
  companyId: string | null;
  decimals: number;
  problems: Problems["lines"];
  /** The key of a line just added from the keyboard, whose first cell takes focus. */
  focusKey: string | null;
  onChange: (key: string, change: Partial<Record<LineField, string>>) => void;
  onAdd: (fromKeyboard: boolean) => void;
  onRemove: (key: string) => void;
}

function fieldOf(collection: CollectionMeta, name: string): FieldMeta | undefined {
  return collection.fields.find((field) => field.name === name);
}

/** A decimal in a cell: digits only, right-aligned, Enter on the last line adds one. */
function DecimalCell({
  label,
  value,
  error,
  onChange,
  onEnter,
}: {
  label: string;
  value: string;
  error?: string;
  onChange: (value: string) => void;
  onEnter?: () => void;
}) {
  return (
    <TextInput
      label={label}
      isLabelHidden
      size="sm"
      width="100%"
      inputMode="decimal"
      value={value}
      onChange={(next) => {
        if (isDecimalDraft(next)) onChange(next);
      }}
      onEnter={onEnter}
      status={error ? { type: "error", message: error } : undefined}
      statusVariant="tooltip"
      // Astryx inputs have no alignment prop and the native input is not reachable through
      // controlStyle; the one CSS rule of this page (globals.css) right-aligns amounts.
      controlClassName="fresh-amount"
    />
  );
}

/** What a read-only line sells, by name. */
function WhatText({ kind, value }: { kind: LineKind | null; value: string }) {
  const info = useLinkInfo(kind, "id", value || null);
  return <Text maxLines={1}>{info?.label ?? ""}</Text>;
}

function LinkText({ entity, value }: { entity: string; value: string }) {
  const info = useLinkInfo(entity, "id", value || null);
  return <Text maxLines={1}>{info?.label ?? ""}</Text>;
}

function Amount({ value, decimals }: { value: string | null; decimals: number }) {
  return value ? (
    <Text hasTabularNumbers>{formatDecimal(value, { minFraction: decimals })}</Text>
  ) : (
    <Text color="secondary">{t("fresh.not_worked_out")}</Text>
  );
}

export function OrderLines({
  collection,
  lines,
  nets,
  editable,
  companyId,
  decimals,
  problems,
  focusKey,
  onChange,
  onAdd,
  onRemove,
}: OrderLinesProps) {
  const label = (name: string): string => fieldOf(collection, name)?.label ?? name;
  const kinds = [label("item_id"), label("charge_id"), label("account_id")].join(t("lines.kind_or"));
  // A line added from the keyboard takes focus in its first cell. The Table gives no ref per
  // row, so the last row's first input is found once it has rendered.
  useEffect(() => {
    if (!focusKey) return;
    const rows = document.querySelectorAll<HTMLElement>(".astryx-table-body .astryx-table-row");
    rows[rows.length - 1]?.querySelector<HTMLInputElement>("input")?.focus();
  }, [focusKey]);
  const rows: Row[] = lines.map((line, index) => ({ line, number: index + 1, net: nets[index] ?? null }));
  const last = lines.length;
  const cellLabel = (name: string, row: Row) => t("lines.cell", { field: label(name), line: row.number });
  const error = (row: Row, name: string) => problems[row.number]?.fields[name];
  const enterOnLast = (row: Row) => (row.number === last ? () => onAdd(true) : undefined);

  const decimalColumn = (name: LineField, width: number): TableColumn<Row> => ({
    key: name,
    header: label(name),
    width: pixel(width),
    align: "end",
    renderCell: (row) =>
      editable ? (
        <DecimalCell
          label={cellLabel(name, row)}
          value={row.line.values[name]}
          error={error(row, name)}
          onChange={(value) => onChange(row.line.key, { [name]: value })}
          onEnter={enterOnLast(row)}
        />
      ) : (
        <Amount value={row.line.values[name] || null} decimals={name === "discount_amount" ? decimals : 0} />
      ),
  });

  const columns: TableColumn<Row>[] = [
    {
      key: "number",
      header: t("lines.number"),
      width: pixel(32),
      renderCell: (row) => (
        <Text color="secondary" hasTabularNumbers>
          {row.number}
        </Text>
      ),
    },
    {
      key: "what",
      header: kinds,
      width: proportional(2, { minWidth: 172 }),
      renderCell: (row) => {
        const kind = lineKind(row.line);
        const value = kind ? row.line.values[KIND_FIELD[kind]] : "";
        if (!editable) return <WhatText kind={kind} value={value} />;
        return (
          <LineWhatLookup
            kind={kind}
            value={value}
            label={t("lines.kind", { line: row.number })}
            companyId={companyId}
            error={error(row, "item_id") ?? error(row, "charge_id") ?? error(row, "account_id") ?? problems[row.number]?.line}
            onChange={(next, id) =>
              onChange(row.line.key, {
                item_id: next === "item" ? id : "",
                charge_id: next === "charge" ? id : "",
                account_id: next === "account" ? id : "",
                warehouse_id: next === "item" ? row.line.values.warehouse_id : "",
              })
            }
          />
        );
      },
    },
    {
      key: "description",
      header: label("description"),
      width: proportional(2, { minWidth: 120 }),
      renderCell: (row) =>
        editable ? (
          <TextInput
            label={cellLabel("description", row)}
            isLabelHidden
            size="sm"
            width="100%"
            value={row.line.values.description}
            onChange={(value) => onChange(row.line.key, { description: value })}
            onEnter={enterOnLast(row)}
            status={error(row, "description") ? { type: "error", message: error(row, "description") } : undefined}
            statusVariant="tooltip"
          />
        ) : (
          <Text maxLines={2}>{row.line.values.description}</Text>
        ),
    },
    {
      key: "warehouse_id",
      header: label("warehouse_id"),
      width: pixel(132),
      // Only a stock line leaves from a warehouse.
      renderCell: (row) =>
        lineKind(row.line) !== "item" ? null : editable ? (
          <RecordLookup
            entity="warehouse"
            label={cellLabel("warehouse_id", row)}
            isLabelHidden
            size="sm"
            companyId={companyId}
            value={row.line.values.warehouse_id}
            error={error(row, "warehouse_id")}
            onChange={(value) => onChange(row.line.key, { warehouse_id: value })}
          />
        ) : (
          <LinkText entity="warehouse" value={row.line.values.warehouse_id} />
        ),
    },
    decimalColumn("qty", 80),
    decimalColumn("unit_price", 92),
    decimalColumn("discount_percent", 92),
    decimalColumn("discount_amount", 84),
    {
      key: "net",
      header: label("net_amount"),
      width: pixel(92),
      align: "end",
      renderCell: (row) => <Amount value={row.net} decimals={decimals} />,
    },
  ];
  if (editable)
    columns.push({
      key: "remove",
      header: "",
      width: pixel(40),
      align: "end",
      renderCell: (row) => (
        <IconButton
          label={t("lines.remove", { line: row.number })}
          tooltip={t("lines.remove", { line: row.number })}
          icon={<Icon icon={Trash2} />}
          variant="ghost"
          size="sm"
          onClick={() => onRemove(row.line.key)}
        />
      ),
    });

  return (
    <>
      <Table<Row>
        data={rows}
        columns={columns}
        idKey={(row) => row.line.key}
        density="compact"
        dividers="rows"
        verticalAlign="middle"
      />
      {editable ? (
        <HStack gap={2} vAlign="center">
          <Button label={t("lines.add")} variant="ghost" size="sm" icon={<Icon icon={Plus} />} onClick={() => onAdd(false)} />
          <Text type="supporting">{t("fresh.lines_hint")}</Text>
        </HStack>
      ) : null}
    </>
  );
}

// The lines of a document in @erpcraft/grid with the Astryx skin: Excel keys, range selection,
// copy and paste with Excel, undo, and line editing (Enter or ArrowDown on the last row adds a
// line, Ctrl+Enter inserts, Ctrl+- deletes). Columns come from the registry, as before.

import { Banner } from "@astryxdesign/core/Banner";
import { Heading } from "@astryxdesign/core/Heading";
import { InfoTip } from "@astryxdesign/core/InfoTip";
import { HStack } from "@astryxdesign/core/HStack";
import { VStack } from "@astryxdesign/core/VStack";
import {
  applyChanges,
  type ColumnType,
  type EditReport,
  Grid,
  type GridColumn,
  localStorageLayout,
  type Option,
  type ParseResult,
} from "@erpcraft/grid";
import { useEffect, useMemo, useRef, useState } from "react";
import { ReferenceLabels, type ReferenceSource, referenceSource } from "./grid-references";
import { fitWidths } from "./grid-widths";
import { api } from "@/api/client";
import type { CollectionMeta, DocTypeMeta, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp } from "@/app/context";
import { t } from "@/i18n/t";
import { peekLinkLabel, recordLabel, rememberRecord, shortId, useLinkLabelsVersion } from "@/lib/link-labels";
import { enumLabel, findEntity, isCompanyScoped } from "@/lib/meta";
import type { FieldValues } from "./field-input";
import { astryxGridSlots } from "./grid-skin";

export interface Line {
  key: string;
  stored: JsonObject;
  values: FieldValues;
}

const isBlank = (value: Json | undefined): boolean => value === undefined || value === null || value === "";
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const ROW_HEIGHT = 36;

function gridType(field: FieldMeta): ColumnType {
  switch (field.type.kind) {
    case "decimal":
      return field.decimal?.role === "money" || field.decimal?.role === "unit_price" ? "money" : "number";
    case "int":
      return "number";
    case "date":
      return "date";
    case "timestamp":
      return "datetime";
    case "bool":
      return "boolean";
    case "enum":
      return "enum";
    case "link":
      return "reference";
    default:
      return "text";
  }
}

export function LinesGrid({
  docType,
  collection,
  fields,
  lines,
  editable,
  problems,
  computed,
  companyId,
  currency,
  header,
  newLine,
  onChange,
}: {
  docType: DocTypeMeta;
  collection: CollectionMeta;
  /** The fields shown, in order (the page leaves out ids and empty read-only ones). */
  fields: FieldMeta[];
  lines: Line[];
  editable: boolean;
  problems: Record<number, { line?: string; fields: Record<string, string> }> | undefined;
  /** The server's preview of the sent lines, for what it works out (net amounts). */
  computed?: JsonObject[];
  companyId: string | null;
  currency: string | null;
  /** The document's header (with company_id): what link-like fields of a line depend on. */
  header: FieldValues;
  newLine: () => Line;
  /** Changes the lines from what they are by then: an edit and a new line can come in one key. */
  onChange: (update: (lines: Line[]) => Line[]) => void;
}) {
  const { meta, currencyDecimals } = useApp();
  const labelsVersion = useLinkLabelsVersion();
  const [report, setReport] = useState<EditReport | null>(null);
  // Labels of line links, lookups and document links, filled as their options load.
  const [, setRefVersion] = useState(0);
  const labels = useRef<ReferenceLabels | null>(null);
  if (!labels.current) labels.current = new ReferenceLabels(() => setRefVersion((v) => v + 1));
  const contextFields = useMemo(() => [...docType.fields, ...collection.fields], [docType, collection]);
  const sources = useMemo(() => {
    const out = new Map<string, ReferenceSource>();
    for (const field of fields) {
      const source = referenceSource(meta, field, contextFields, labels.current as ReferenceLabels, { companyId, decimals: (currency ? currencyDecimals[currency] : undefined) ?? 2 });
      if (source) out.set(field.name, source);
    }
    return out;
  }, [fields, contextFields, meta, companyId, currency, currencyDecimals]);
  const headerRef = useRef(header);
  headerRef.current = header;
  // The labels of the values the lines hold, read when lines or the header they depend on change.
  const prefetchKey = JSON.stringify([...sources.keys()].map((name) => [name, lines.map((line) => line.values[name] ?? null)])) + JSON.stringify(header);
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key names what the labels depend on
  useEffect(() => {
    if (sources.size === 0) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      for (const source of sources.values()) void source.prefetch(lines.map((line) => line.values), headerRef.current, controller.signal).catch(() => undefined);
    }, 150);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [prefetchKey]);
  // One set of slot components for the grid's life: new ones would remount every cell, editor included.
  const sourcesRef = useRef(sources);
  sourcesRef.current = sources;
  const slots = useMemo(
    () =>
      astryxGridSlots<Line>({
        pickers: {
          options: (columnId, row) => {
            const source = sourcesRef.current.get(columnId);
            return source ? (search, signal) => source.options(row.values, headerRef.current, search, signal) : undefined;
          },
          blocked: (columnId, row) => sourcesRef.current.get(columnId)?.blocked(row.values, headerRef.current) ?? null,
        },
      }),
    [],
  );
  const kinds = docType.alternatives.find((alternative) => alternative.collection === collection.name)?.one_of ?? [];
  const decimals = (currency ? currencyDecimals[currency] : undefined) ?? 2;

  // Which preview line each line is: blank new lines are not sent, so they have none.
  const fresh = useMemo(() => newLine().values, [newLine]);
  const sentIndex = new Map<string, number>();
  for (const line of lines) {
    const sent =
      Object.keys(line.stored).length > 0 ||
      collection.fields.some((field) => !isBlank(line.values[field.name]) && line.values[field.name] !== fresh[field.name]);
    if (sent) sentIndex.set(line.key, sentIndex.size);
  }

  const firstComputedMoney = fields.find((field) => field.readonly && gridType(field) === "money")?.name;
  // The width there is, measured, so the columns can be fitted to it.
  const [available, setAvailable] = useState(0);
  const boxRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const observer = new ResizeObserver(([entry]) => setAvailable(Math.floor((entry?.contentRect.width ?? 0) - 4)));
    observer.observe(box);
    return () => observer.disconnect();
  }, []);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `labelsVersion` re-reads labels that arrived
  const rawColumns = useMemo<GridColumn<Line>[]>(
    () =>
      fields.map((field): GridColumn<Line> => {
        const type = gridType(field);
        const name = field.name;
        const kind = kinds.find((candidate) => candidate.fields.includes(name));
        const base: GridColumn<Line> = {
          id: name,
          header: field.label,
          type,
          sortable: false,
          filterable: false,
          editable: editable && !field.readonly,
          value: (row) => {
            const index = sentIndex.get(row.key);
            const worked = field.readonly && computed && index !== undefined ? computed[index]?.[name] : undefined;
            const value = worked ?? row.values[name];
            return field.type.kind === "int" && typeof value === "number" ? String(value) : (value ?? null);
          },
          setValue: (row, value) => {
            const values: FieldValues = { ...row.values, [name]: (value ?? null) as Json };
            if (field.type.kind === "int" && typeof value === "string" && /^-?\d+$/.test(value)) values[name] = parseInt(value, 10);
            // A line is one kind (stock item, charge, account): a value in one kind's field empties the others.
            if (kind && !isBlank(values[name])) for (const other of kinds) if (other !== kind) for (const n of other.fields) values[n] = null;
            return { ...row, values };
          },
          minFraction: type === "money" ? decimals : undefined,
          currency: type === "money" ? (currency ?? undefined) : undefined,
          // The document's totals are the server's, under the lines; no second sum in the grid.
          total: false,
          // What the server works out for the line (net) stays in view at the right edge.
          pin: field.readonly && type === "money" && name === firstComputedMoney ? "right" : name === fields[0]?.name ? "left" : undefined,
          hidden: field.type.kind === "json" ? true : undefined,
          searchable: false,
          width: type === "text" ? 240 : type === "reference" ? 200 : undefined,
        };
        if (field.type.kind === "enum") {
          const enumType = field.type;
          return { ...base, options: enumType.values.map((v) => ({ value: v, label: enumLabel(field, v) })) };
        }
        if (field.type.kind === "link") {
          const { entity, key } = field.type;
          const target = findEntity(meta, entity);
          const filters: Record<string, string> = { ...field.type.filters, ...(target && isCompanyScoped(target) && companyId ? { company_id: companyId } : {}) };
          const seen = new Map<string, string>();
          return {
            ...base,
            header: field.label,
            format: (value) => {
              if (typeof value !== "string" || value === "") return "";
              const label = peekLinkLabel(entity, key, value);
              return label ?? (label === null ? shortId(value) : t("value.loading"));
            },
            // Typed or pasted text: a record already offered, by its name or code, or an id.
            parse: (text): ParseResult => {
              const trimmed = text.trim();
              if (trimmed === "") return { ok: true, value: null };
              const found = seen.get(trimmed.toLowerCase());
              if (found) return { ok: true, value: found };
              if (UUID.test(trimmed) || key !== "id") return { ok: true, value: trimmed };
              return { ok: false, error: t("grid.pick_from_list", { label: target?.label ?? entity }) };
            },
            loadOptions: async (search, signal): Promise<Option[]> => {
              const page = await api.run("entity.list", { entity, q: search || null, filters, limit: 20, active_only: true }, { signal });
              return page.items.map((record) => {
                rememberRecord(entity, key, record);
                const value = String(record[key] ?? record.id);
                const label = recordLabel(entity, record);
                seen.set(label.toLowerCase(), value);
                if (typeof record.code === "string") seen.set(record.code.toLowerCase(), value);
                return { value, label };
              });
            },
          };
        }
        const source = sources.get(name);
        if (source) {
          return {
            ...base,
            type: "reference",
            width: 240,
            format: (value, row) => (typeof value === "string" && value ? source.label(value, row.values) : ""),
            // Typed or pasted: a choice already offered, by its label, or an id.
            parse: (text): ParseResult => {
              const trimmed = text.trim();
              if (trimmed === "") return { ok: true, value: null };
              const found = labels.current?.find(name, trimmed);
              if (found) return { ok: true, value: found };
              if (UUID.test(trimmed)) return { ok: true, value: trimmed };
              return { ok: false, error: t("grid.pick_from_list", { label: field.label }) };
            },
          };
        }
        if (["document_link", "line_link", "lookup", "entity_link", "uuid", "json", "posting"].includes(field.type.kind)) {
          return { ...base, editable: false, format: (value) => (typeof value === "string" ? shortId(value) : "") };
        }
        return base;
      }),
    [fields, kinds, editable, computed, decimals, currency, companyId, meta, labelsVersion, lines, sources, labels.current?.version],
  );
  const widths = useMemo(() => (available > 0 ? fitWidths(rawColumns, lines, available) : null), [rawColumns, lines, available]);
  const columns = useMemo(() => rawColumns.map((column) => ({ ...column, width: widths?.get(column.id) ?? column.width })), [rawColumns, widths]);

  const problemList = Object.entries(problems ?? {}).flatMap(([line, problem]) => [
    ...(problem.line ? [`${t("lines.line_n", { line })}: ${problem.line}`] : []),
    ...Object.entries(problem.fields).map(([name, message]) => {
      const label = collection.fields.find((field) => field.name === name)?.label ?? name;
      return `${t("lines.line_n", { line })}, ${label}: ${message}`;
    }),
  ]);
  const height = Math.min(ROW_HEIGHT * (Math.max(lines.length, 1) + 1) + 8, 560);

  return (
    <VStack gap={2}>
      {problemList.length > 0 ? (
        <Banner status="error" title={problemList[0] ?? ""} description={problemList.slice(1).join(" · ") || undefined} />
      ) : null}
      {report && report.rejected.length > 0 ? (
        <Banner
          status="warning"
          title={t("grid.rejected", { count: report.rejected.length })}
          description={report.rejected
            .slice(0, 5)
            .map((cell) => `${t("lines.line_n", { line: cell.rowIndex + 1 })}, ${cell.columnId}: ${cell.reason}`)
            .join(" · ")}
          isDismissable
          onDismiss={() => setReport(null)}
        />
      ) : null}
      <div ref={boxRef}>
      <Grid<Line>
        id={`lines.${docType.name}.${collection.name}`}
        aria-label={collection.label}
        columns={columns}
        rows={lines}
        rowId={(line) => line.key}
        slots={slots}
        toolbar
        // The section's heading shares the toolbar row with the columns menu; the keys are behind its info tip.
        toolbarExtra={
          <HStack gap={1} vAlign="center">
            <Heading level={2}>{collection.label}</Heading>
            {editable ? <InfoTip content={t("grid.keys")} label={t("grid.keys_label")} /> : null}
          </HStack>
        }
        height={height}
        rowHeight={ROW_HEIGHT}
        layoutStorage={localStorageLayout("astryx-spike.grid.")}
        onEdit={editable ? (changes) => onChange((current) => applyChanges(current, changes, columns, (line) => line.key)) : undefined}
        onReport={setReport}
        lineEditing={
          editable
            ? {
                onAppend: () => onChange((current) => [...current, newLine()]),
                onInsert: (at) => onChange((current) => [...current.slice(0, at), newLine(), ...current.slice(at)]),
                onDelete: (ids) => onChange((current) => current.filter((line) => !ids.includes(line.key))),
              }
            : undefined
        }
      />
      </div>
    </VStack>
  );
}

// The lines of a document in @erpcraft/grid with the shadcn skin: Excel keys, range selection,
// copy and paste with Excel, undo, and line editing (Enter or ArrowDown on the last row adds a
// line, Ctrl+Enter inserts, Ctrl+- deletes). Columns come from the registry. Takes the same
// props as the table it replaces in document-page.tsx. Ported from ui-astryx's lines-grid.tsx.

import {
  applyChanges,
  type CellChange,
  type ColumnType,
  type EditReport,
  Grid,
  type GridColumn,
  localStorageLayout,
  type Option,
  type ParseResult,
} from "@erpcraft/grid";
import { Info } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "@/api/client";
import type { CollectionMeta, FieldMeta, Json } from "@/api/types";
import { useApp } from "@/app/context";
import { t } from "@/i18n/t";
import { peekLinkLabel, recordLabel, rememberRecord, shortId, useLinkLabelsVersion } from "@/lib/link-labels";
import { enumLabel, findEntity, isCompanyScoped } from "@/lib/meta";
import type { FieldValues } from "./field-input";
import { ReferenceLabels, type ReferenceSource, referenceSource } from "./grid-references";
import { shadcnGridSlots } from "./grid-skin";
import { fitWidths } from "./grid-widths";
import { Alert, AlertDescription, AlertTitle } from "./ui/alert";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

export interface Line {
  key: string;
  stored: { [key: string]: Json };
  values: FieldValues;
}

/** One kind a line can be, and the fields that make it that kind. */
export interface LineKind {
  label: string;
  fields: string[];
}

/** By line (from 1): a message for the line, and one per field of it. */
export type LineProblems = Record<number, { line?: string; fields: Record<string, string> }>;

const isBlank = (value: Json | undefined): boolean => value === undefined || value === null || value === "";
const isZero = (value: Json | undefined): boolean =>
  isBlank(value) || value === 0 || (typeof value === "string" && /^-?0*(\.0*)?$/.test(value));
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

/** The fields a line shows: what a person enters or reads, not ids the server keeps. */
function shownFields(collection: CollectionMeta, lines: Line[], editable: boolean): FieldMeta[] {
  return collection.fields.filter((field) => {
    if (field.name === "id" || field.type.kind === "json") return false;
    // A read-only column with no value on any line is left out of a locked document.
    if (!editable && field.readonly && lines.every((line) => isBlank(line.values[field.name]))) return false;
    return true;
  });
}

export function LinesGrid({
  collection,
  lines,
  editable,
  kinds,
  companyId,
  currency,
  header,
  headerFields,
  problems,
  newLine,
  onChange,
}: {
  collection: CollectionMeta;
  lines: Line[];
  editable: boolean;
  /** What a line is, one of (stock, a charge, an account), as the document type declares. */
  kinds: LineKind[] | null;
  companyId: string | null;
  /** The document's currency, for money columns. */
  currency?: string | null;
  /** The document's header values, which references on a line can depend on. */
  header: FieldValues;
  headerFields: FieldMeta[];
  problems: LineProblems | undefined;
  /** A blank line, as the page makes them. */
  newLine: () => Line;
  /** The lines as they are now, and the cell an edit touched (its problem is cleared). */
  onChange: (lines: Line[], touched?: { line: number; field: string }) => void;
}) {
  const { meta, currencyDecimals } = useApp();
  const labelsVersion = useLinkLabelsVersion();
  const [report, setReport] = useState<EditReport | null>(null);

  // One key can change the lines twice (an edit, then a new line): each change starts from the
  // lines as the one before left them, not from the props of this render.
  const latest = useRef(lines);
  latest.current = lines;
  const change = (next: Line[], touched?: { line: number; field: string }) => {
    latest.current = next;
    onChange(next, touched);
  };

  // Labels of line links, lookups and document links, filled as their options load.
  const [, setRefVersion] = useState(0);
  const labels = useRef<ReferenceLabels | null>(null);
  if (!labels.current) labels.current = new ReferenceLabels(() => setRefVersion((v) => v + 1));
  const contextFields = useMemo(() => [...headerFields, ...collection.fields], [headerFields, collection]);
  const decimals = (currency ? currencyDecimals[currency] : undefined) ?? 2;
  const fields = useMemo(() => shownFields(collection, lines, editable), [collection, lines, editable]);
  const sources = useMemo(() => {
    const out = new Map<string, ReferenceSource>();
    for (const field of fields) {
      const source = referenceSource(meta, field, contextFields, labels.current as ReferenceLabels, {
        companyId,
        decimals,
      });
      if (source) out.set(field.name, source);
    }
    return out;
  }, [fields, contextFields, meta, companyId, decimals]);
  const headerRef = useRef(header);
  headerRef.current = header;
  // The labels of the values the lines hold, read when lines or the header they depend on change.
  const prefetchKey =
    JSON.stringify([...sources.keys()].map((name) => [name, lines.map((line) => line.values[name] ?? null)])) +
    JSON.stringify(header);
  // biome-ignore lint/correctness/useExhaustiveDependencies: the key names what the labels depend on
  useEffect(() => {
    if (sources.size === 0) return undefined;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      for (const source of sources.values())
        void source
          .prefetch(
            lines.map((line) => line.values),
            headerRef.current,
            controller.signal,
          )
          .catch(() => undefined);
    }, 150);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [prefetchKey]);

  // One set of slot components for the grid's life: new ones would remount every cell, editor included.
  const sourcesRef = useRef(sources);
  sourcesRef.current = sources;
  const problemsRef = useRef(problems);
  problemsRef.current = problems;
  const slots = useMemo(
    () =>
      shadcnGridSlots<Line>({
        problem: (columnId, row) => {
          const index = latest.current.findIndex((line) => line.key === row.key);
          return index >= 0 ? problemsRef.current?.[index + 1]?.fields[columnId] : undefined;
        },
        pickers: {
          options: (columnId, row) => {
            const source = sourcesRef.current.get(columnId);
            return source
              ? (search, signal) => source.options(row.values, headerRef.current, search, signal)
              : undefined;
          },
          blocked: (columnId, row) => sourcesRef.current.get(columnId)?.blocked(row.values, headerRef.current) ?? null,
        },
      }),
    [],
  );

  // The width there is, measured, so the columns can be fitted to it.
  const [available, setAvailable] = useState(0);
  const boxRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return undefined;
    const observer = new ResizeObserver(([entry]) => setAvailable(Math.floor((entry?.contentRect.width ?? 0) - 4)));
    observer.observe(box);
    return () => observer.disconnect();
  }, []);

  const lineKinds = kinds ?? [];
  // While a document is edited, a quantity the server keeps (delivered, invoiced, returned) that is
  // still nothing on every line starts hidden, so the lines fit; Columns shows it.
  const quietKey = editable
    ? fields
        .filter(
          (field) =>
            field.readonly && gridType(field) === "number" && lines.every((line) => isZero(line.values[field.name])),
        )
        .map((field) => field.name)
        .join()
    : "";
  const quiet = useMemo(() => new Set(quietKey.split(",").filter(Boolean)), [quietKey]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `labelsVersion` re-reads labels that arrived
  const rawColumns = useMemo<GridColumn<Line>[]>(
    () =>
      fields.map((field): GridColumn<Line> => {
        const type = gridType(field);
        const name = field.name;
        const kind = lineKinds.find((candidate) => candidate.fields.includes(name));
        const base: GridColumn<Line> = {
          id: name,
          header: field.label,
          type,
          sortable: false,
          filterable: false,
          editable: editable && !field.readonly,
          value: (row) => {
            const value = row.values[name];
            return field.type.kind === "int" && typeof value === "number" ? String(value) : (value ?? null);
          },
          setValue: (row, value) => {
            const values: FieldValues = { ...row.values, [name]: (value ?? null) as Json };
            if (field.type.kind === "int" && typeof value === "string" && /^-?\d+$/.test(value))
              values[name] = Number.parseInt(value, 10);
            // A line is one kind (stock item, charge, account): a value in one kind's field empties the others.
            if (kind && !isBlank(values[name]))
              for (const other of lineKinds) if (other !== kind) for (const n of other.fields) values[n] = null;
            return { ...row, values };
          },
          minFraction: type === "money" ? decimals : undefined,
          currency: type === "money" ? (currency ?? undefined) : undefined,
          // The document's totals are the server's, under the lines; no second sum in the grid.
          total: false,
          // The first field stays in view while the others scroll, and so does the line's net amount.
          pin: name === fields[0]?.name ? "left" : name === "net_amount" ? "right" : undefined,
          searchable: false,
          width: type === "text" ? 240 : type === "reference" ? 200 : undefined,
          // While a document is edited, a quantity the server keeps (delivered, invoiced, returned)
          // that is still nothing on every line starts hidden, so the lines fit; Columns shows it.
          hidden: quiet.has(name) ? true : undefined,
        };
        if (field.type.kind === "enum") {
          const enumType = field.type;
          return { ...base, options: enumType.values.map((v) => ({ value: v, label: enumLabel(field, v) })) };
        }
        if (field.type.kind === "link") {
          const { entity, key } = field.type;
          const target = findEntity(meta, entity);
          const filters: Record<string, string> = {
            ...field.type.filters,
            ...(target && isCompanyScoped(target) && companyId ? { company_id: companyId } : {}),
          };
          const seen = new Map<string, string>();
          return {
            ...base,
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
              const page = await api.run(
                "entity.list",
                { entity, q: search || null, filters, limit: 20, active_only: true },
                { signal },
              );
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
        if (["uuid", "posting"].includes(field.type.kind)) {
          return { ...base, editable: false, format: (value) => (typeof value === "string" ? shortId(value) : "") };
        }
        return base;
      }),
    [
      fields,
      lineKinds,
      editable,
      decimals,
      currency,
      companyId,
      meta,
      labelsVersion,
      sources,
      labels.current?.version,
      quiet,
    ],
  );
  const widths = useMemo(
    () => (available > 0 ? fitWidths(rawColumns, lines, available) : null),
    [rawColumns, lines, available],
  );
  const columns = useMemo(
    () => rawColumns.map((column) => ({ ...column, width: widths?.get(column.id) ?? column.width })),
    [rawColumns, widths],
  );

  const onEdit = (changes: CellChange<Line>[]) => {
    const next = applyChanges(latest.current, changes, columns, (line) => line.key);
    const first = changes[0];
    const index = first ? next.findIndex((line) => line.key === first.rowId) : -1;
    change(next, first && index >= 0 ? { line: index + 1, field: first.columnId } : undefined);
  };

  const problemList = Object.entries(problems ?? {}).flatMap(([line, problem]) => [
    ...(problem.line ? [`${t("lines.line_n", { line })}: ${problem.line}`] : []),
    ...Object.entries(problem.fields).map(([name, message]) => {
      const label = collection.fields.find((field) => field.name === name)?.label ?? name;
      return `${t("lines.line_n", { line })}, ${label}: ${message}`;
    }),
  ]);
  const height = Math.min(ROW_HEIGHT * (Math.max(lines.length, 1) + 1) + 8, 560);

  return (
    <div className="flex flex-col gap-2">
      {problemList.length > 0 ? (
        <Alert variant="destructive">
          <AlertTitle>{problemList[0]}</AlertTitle>
          {problemList.length > 1 ? <AlertDescription>{problemList.slice(1).join(" · ")}</AlertDescription> : null}
        </Alert>
      ) : null}
      {report && report.rejected.length > 0 ? (
        <Alert>
          <AlertTitle>{t("grid.rejected", { count: report.rejected.length })}</AlertTitle>
          <AlertDescription>
            {report.rejected
              .slice(0, 5)
              .map((cell) => `${t("lines.line_n", { line: cell.rowIndex + 1 })}, ${cell.columnId}: ${cell.reason}`)
              .join(" · ")}
          </AlertDescription>
        </Alert>
      ) : null}
      <div ref={boxRef}>
        <Grid<Line>
          id={`lines.${collection.name}`}
          aria-label={collection.label}
          columns={columns}
          rows={lines}
          rowId={(line) => line.key}
          slots={slots}
          toolbar
          // The page's section names the lines; the toolbar holds the columns menu and the keys, behind a tip.
          toolbarExtra={
            <div className="flex items-center gap-1">
              {editable ? (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <button type="button" aria-label={t("grid.keys_label")} className="text-muted-foreground">
                      <Info aria-hidden="true" className="size-4" />
                    </button>
                  </TooltipTrigger>
                  <TooltipContent className="max-w-sm">{t("grid.keys")}</TooltipContent>
                </Tooltip>
              ) : null}
            </div>
          }
          height={height}
          rowHeight={ROW_HEIGHT}
          layoutStorage={localStorageLayout("erpcraft.grid.")}
          onEdit={editable ? onEdit : undefined}
          onReport={setReport}
          lineEditing={
            editable
              ? {
                  onAppend: () => change([...latest.current, newLine()]),
                  onInsert: (at) => change([...latest.current.slice(0, at), newLine(), ...latest.current.slice(at)]),
                  onDelete: (ids) => change(latest.current.filter((line) => !ids.includes(line.key))),
                }
              : undefined
          }
        />
      </div>
    </div>
  );
}

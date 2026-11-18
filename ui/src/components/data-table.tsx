import { ArrowDown, ArrowUp, ChevronLeft, ChevronRight } from "lucide-react";
import type { ReactNode } from "react";
import { Link } from "react-router";
import type { ApiError } from "@/api/client";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableFooter, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { cn } from "@/lib/utils";

export interface Column<Row> {
  id: string;
  /** What the column is called, also in the column chooser. */
  header: string;
  /** The header as shown, where it is more than the words (a currency that is read). */
  headerNode?: ReactNode;
  cell: (row: Row) => ReactNode;
  /** Numbers are aligned to the end so their digits line up. */
  align?: "start" | "end";
  /** The field the server sorts by when the header is clicked. */
  sortField?: string;
  className?: string;
  /** Not shown until the user asks for it in the column chooser. */
  defaultHidden?: boolean;
}

export interface DataTableProps<Row> {
  columns: Column<Row>[];
  /** `undefined` while the first page loads. */
  rows: Row[] | undefined;
  rowKey: (row: Row) => string;
  /** Makes every row a link to its record. A row without a destination stays plain. */
  rowHref?: (row: Row) => string | undefined;
  loading?: boolean;
  error?: ApiError | null;
  onRetry?: () => void;
  /** What to say, and offer, when there are no rows. */
  empty: ReactNode;
  /** Current sort as the server takes it: a field name, `-` first for descending. */
  sort?: string;
  onSort?: (sort: string) => void;
  /** Paging. Leave out for a table that shows everything, such as a report. */
  page?: number;
  pageSize?: number;
  total?: number;
  onPage?: (page: number) => void;
  /** Extra rows at the foot, such as totals. */
  footer?: ReactNode;
  caption: string;
  /** Rows can be chosen with a checkbox, for an action on several at once. */
  selection?: {
    selected: ReadonlySet<string>;
    onToggle: (row: Row, selected: boolean) => void;
    /** Why a row cannot be chosen right now, or null if it can. */
    blocked?: (row: Row) => string | null;
    label: (row: Row) => string;
  };
}

const SKELETON_ROWS = ["a", "b", "c", "d", "e", "f"];

/**
 * The one table for every list. Sorting and paging are requests to the server:
 * the table shows the page it is given and reports what the user asked for.
 */
export function DataTable<Row>({
  columns,
  rows,
  rowKey,
  rowHref,
  loading = false,
  error = null,
  onRetry,
  empty,
  sort = "",
  onSort,
  page,
  pageSize,
  total,
  onPage,
  footer,
  caption,
  selection,
}: DataTableProps<Row>) {
  const descending = sort.startsWith("-");
  const sortField = descending ? sort.slice(1) : sort;
  const paged = page !== undefined && pageSize !== undefined && total !== undefined && onPage !== undefined;
  const first = paged ? (page - 1) * pageSize + 1 : 0;
  const last = paged ? Math.min(page * pageSize, total) : 0;

  return (
    <div className="overflow-hidden rounded-lg border bg-card">
      {error ? (
        <div className="p-3">
          <Alert
            title={t("list.load_failed")}
            actions={
              onRetry ? (
                <Button variant="outline" size="sm" onClick={onRetry}>
                  {t("action.retry")}
                </Button>
              ) : undefined
            }
          >
            {error.message}
          </Alert>
        </div>
      ) : null}
      <Table aria-label={caption} aria-busy={loading || undefined}>
        <TableHeader>
          <TableRow>
            {selection ? (
              <TableHead className="w-10 px-3">
                <span className="sr-only">{t("list.select")}</span>
              </TableHead>
            ) : null}
            {columns.map((column) => {
              const sorted = column.sortField !== undefined && column.sortField === sortField;
              const end = column.align === "end";
              return (
                <TableHead
                  key={column.id}
                  className={cn("px-3", end && "text-end", column.className)}
                  aria-sort={sorted ? (descending ? "descending" : "ascending") : undefined}
                >
                  {column.sortField && onSort ? (
                    <button
                      type="button"
                      className={cn(
                        "inline-flex items-center gap-1 rounded-sm hover:text-foreground",
                        end && "flex-row-reverse",
                      )}
                      onClick={() => {
                        const field = column.sortField ?? "";
                        onSort(sorted && !descending ? `-${field}` : field);
                      }}
                      title={t("list.sort_by", { column: column.header })}
                    >
                      {column.headerNode ?? column.header}
                      {sorted ? (
                        descending ? (
                          <ArrowDown className="size-3" aria-hidden="true" />
                        ) : (
                          <ArrowUp className="size-3" aria-hidden="true" />
                        )
                      ) : (
                        <span className="size-3" aria-hidden="true" />
                      )}
                    </button>
                  ) : (
                    (column.headerNode ?? column.header)
                  )}
                </TableHead>
              );
            })}
          </TableRow>
        </TableHeader>
        <TableBody className={cn(loading && rows !== undefined && "opacity-60")}>
          {rows === undefined && !error
            ? SKELETON_ROWS.map((key) => (
                <TableRow key={key}>
                  {selection ? <TableCell /> : null}
                  {columns.map((column) => (
                    <TableCell key={column.id} className="px-3 py-2.5">
                      <Skeleton className={cn("my-0.5 w-2/3", column.align === "end" && "ms-auto")} />
                    </TableCell>
                  ))}
                </TableRow>
              ))
            : null}
          {rows?.map((row) => {
            const href = rowHref?.(row);
            return (
              <TableRow key={rowKey(row)} data-state={selection?.selected.has(rowKey(row)) ? "selected" : undefined}>
                {selection ? (
                  <TableCell className="w-10 px-3 py-2.5">
                    <Checkbox
                      aria-label={selection.label(row)}
                      title={selection.blocked?.(row) ?? undefined}
                      checked={selection.selected.has(rowKey(row))}
                      disabled={!selection.selected.has(rowKey(row)) && Boolean(selection.blocked?.(row))}
                      onCheckedChange={(checked) => selection.onToggle(row, checked === true)}
                    />
                  </TableCell>
                ) : null}
                {columns.map((column, index) => (
                  <TableCell
                    key={column.id}
                    className={cn(
                      "px-3 py-2.5",
                      column.align === "end" && "text-end tabular-nums",
                      href && "p-0",
                      column.className,
                    )}
                  >
                    {href ? (
                      // Every cell is part of the row's link, so the whole row opens the
                      // record, also in a new tab. Only the first cell is a tab stop.
                      <Link to={href} tabIndex={index === 0 ? undefined : -1} className="block px-3 py-2.5">
                        {column.cell(row)}
                      </Link>
                    ) : (
                      column.cell(row)
                    )}
                  </TableCell>
                ))}
              </TableRow>
            );
          })}
        </TableBody>
        {footer && rows !== undefined && rows.length > 0 ? <TableFooter>{footer}</TableFooter> : null}
      </Table>
      {rows !== undefined && rows.length === 0 && !error ? (
        <div className="flex flex-col items-center gap-3 px-4 py-10 text-center text-sm text-muted-foreground">
          {empty}
        </div>
      ) : null}
      {paged ? (
        <div className="flex items-center justify-between gap-3 border-t px-3 py-2.5 text-sm text-muted-foreground">
          <span aria-live="polite">
            {rows === undefined ? (
              <Skeleton className="w-24" />
            ) : total === 0 ? (
              t("list.count_none")
            ) : (
              t("list.count", { first, last, total: total.toLocaleString() })
            )}
          </span>
          <span className="flex items-center gap-1">
            <Button variant="outline" size="sm" disabled={page <= 1} onClick={() => onPage(page - 1)}>
              <ChevronLeft className="rtl:rotate-180" aria-hidden="true" />
              {t("list.previous")}
            </Button>
            <Button variant="outline" size="sm" disabled={last >= total} onClick={() => onPage(page + 1)}>
              {t("list.next")}
              <ChevronRight className="rtl:rotate-180" aria-hidden="true" />
            </Button>
          </span>
        </div>
      ) : null}
    </div>
  );
}

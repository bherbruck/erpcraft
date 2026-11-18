import { api } from "@/api/client";
import type { HistoryEntry } from "@/api/types";
import { Button } from "@/components/button";
import { t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { useAsync } from "@/lib/use-async";
import { Badge } from "./badge";
import { type Column, DataTable } from "./data-table";

const LIMIT = 100;

const columns: Column<HistoryEntry>[] = [
  {
    id: "at",
    header: t("history.when"),
    cell: (entry) => <span className="whitespace-nowrap">{formatTimestamp(entry.at)}</span>,
  },
  { id: "user", header: t("history.who"), cell: (entry) => entry.user_name ?? entry.user_id },
  {
    id: "command",
    header: t("history.command"),
    cell: (entry) => <code className="font-mono text-xs">{entry.command}</code>,
  },
  {
    id: "outcome",
    header: t("history.outcome"),
    // The audit log says `ok` or `error`.
    cell: (entry) =>
      entry.outcome === "ok" ? (
        <Badge tone="success">{t("history.ok")}</Badge>
      ) : (
        <Badge tone="destructive">{t("history.error")}</Badge>
      ),
  },
  {
    id: "detail",
    header: t("history.detail"),
    cell: (entry) => (
      <div className="space-y-1">
        {entry.error_message ? <p>{entry.error_message}</p> : null}
        {entry.input !== undefined && entry.input !== null ? (
          <details>
            <summary className="cursor-pointer text-sm text-muted-foreground">{t("history.input")}</summary>
            <pre className="mt-1 max-w-form overflow-x-auto rounded-sm bg-muted p-2 font-mono text-xs">
              {JSON.stringify(entry.input, null, 2)}
            </pre>
          </details>
        ) : null}
      </div>
    ),
  },
];

/** Who ran which command on a record, and when, failed attempts included. */
export function History({ resource, id, version }: { resource: string; id: string; version?: string | number }) {
  const history = useAsync(`${resource}|${id}|${version ?? ""}`, (signal) =>
    api.run("audit.history", { resource, id, limit: LIMIT }, { signal }),
  );
  return (
    <div className="space-y-2">
      <DataTable
        caption={t("history.title")}
        columns={columns}
        rows={history.data?.items}
        rowKey={(entry) => entry.id}
        loading={history.loading}
        error={history.error}
        onRetry={history.reload}
        empty={<p>{t("history.empty")}</p>}
      />
      {history.data && history.data.total > history.data.items.length ? (
        <p className="text-sm text-muted-foreground">
          {t("history.truncated", { shown: history.data.items.length, total: history.data.total })}
        </p>
      ) : null}
      <Button variant="ghost" size="sm" onClick={history.reload}>
        {t("action.refresh")}
      </Button>
    </div>
  );
}

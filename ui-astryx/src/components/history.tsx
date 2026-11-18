import { Badge } from "@astryxdesign/core/Badge";
import { Button } from "@astryxdesign/core/Button";
import { Code } from "@astryxdesign/core/Code";
import { Collapsible } from "@astryxdesign/core/Collapsible";
import { CodeBlock } from "@astryxdesign/core/CodeBlock";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { api } from "@/api/client";
import type { HistoryEntry } from "@/api/types";
import { t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { useAsync } from "@/lib/use-async";
import { type Column, DataTable } from "./data-table";

const LIMIT = 100;

const columns: Column<HistoryEntry>[] = [
  { id: "at", header: t("history.when"), width: 180, cell: (entry) => formatTimestamp(entry.at) },
  { id: "user", header: t("history.who"), width: 160, cell: (entry) => entry.user_name ?? entry.user_id },
  { id: "command", header: t("history.command"), width: 200, cell: (entry) => <Code>{entry.command}</Code> },
  {
    id: "outcome",
    header: t("history.outcome"),
    width: 100,
    cell: (entry) =>
      entry.outcome === "ok" ? <Badge variant="success" label={t("history.ok")} /> : <Badge variant="error" label={t("history.error")} />,
  },
  {
    id: "detail",
    header: t("history.detail"),
    cell: (entry) => (
      <VStack gap={1}>
        {entry.error_message ? <Text>{entry.error_message}</Text> : null}
        {entry.input !== undefined && entry.input !== null ? (
          <Collapsible trigger={t("history.input")} defaultIsOpen={false}>
            <CodeBlock code={JSON.stringify(entry.input, null, 2)} language="json" />
          </Collapsible>
        ) : null}
      </VStack>
    ),
  },
];

/** Who ran which command on a record, and when, failed attempts included. */
export function History({ resource, id, version }: { resource: string; id: string; version?: string | number }) {
  const history = useAsync(`${resource}|${id}|${version ?? ""}`, (signal) =>
    api.run("audit.history", { resource, id, limit: LIMIT }, { signal }),
  );
  return (
    <VStack gap={2}>
      <DataTable
        caption={t("history.title")}
        columns={columns}
        rows={history.data?.items}
        rowKey={(entry) => entry.id}
        loading={history.loading}
        error={history.error}
        onRetry={history.reload}
        empty={{ title: t("history.empty") }}
      />
      {history.data && history.data.total > history.data.items.length ? (
        <Text type="supporting">{t("history.truncated", { shown: history.data.items.length, total: history.data.total })}</Text>
      ) : null}
      <div>
        <Button label={t("action.refresh")} variant="ghost" size="sm" onClick={history.reload} />
      </div>
    </VStack>
  );
}

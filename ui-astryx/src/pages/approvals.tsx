// The approvals inbox (`approval.list`) and one request: what was asked, on which facts, who
// decided, and approving or rejecting it. Nobody decides their own request.

import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { CodeBlock } from "@astryxdesign/core/CodeBlock";
import { Collapsible } from "@astryxdesign/core/Collapsible";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Link } from "@astryxdesign/core/Link";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { Selector } from "@astryxdesign/core/Selector";
import { Tab, TabList } from "@astryxdesign/core/TabList";
import { Text } from "@astryxdesign/core/Text";
import { TextArea } from "@astryxdesign/core/TextArea";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo, useState } from "react";
import { useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { ApprovalRequest } from "@/api/generated";
import type { Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { Empty } from "@/components/field-value";
import { Page } from "@/components/page";
import { type MessageKey, t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { forgetDocuments } from "@/lib/doc-labels";
import { canRun, commandInputs, findCommand, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

type Tone = "neutral" | "warning" | "success" | "error";
const STATUSES: Record<string, { tone: Tone; label: MessageKey }> = {
  pending: { tone: "warning", label: "approval.status.pending" },
  executed: { tone: "success", label: "approval.status.executed" },
  failed: { tone: "error", label: "approval.status.failed" },
  stale: { tone: "neutral", label: "approval.status.stale" },
  rejected: { tone: "error", label: "approval.status.rejected" },
  expired: { tone: "neutral", label: "approval.status.expired" },
};
const SCOPES: Record<string, MessageKey> = {
  to_approve: "approval.scope.to_approve",
  mine: "approval.scope.mine",
  all: "approval.scope.all",
};

function StatusOf({ status }: { status: string }) {
  const known = STATUSES[status];
  return <Badge variant={known?.tone ?? "neutral"} label={known ? t(known.label) : status} />;
}

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** "Purchase order: Approve". */
function useCommandLabel(): (name: string) => string {
  const { meta } = useApp();
  return (name) => {
    const command = findCommand(meta, name);
    if (!command) return name;
    const resource = findDocType(meta, command.resource)?.label ?? meta.entities.find((e) => e.name === command.resource)?.label;
    return resource ? t("approval.command", { resource, action: command.label }) : command.label;
  };
}

/** The facts a rule decided on, as the request shows them. */
function Facts({ facts }: { facts: JsonObject }) {
  return (
    <MetadataList>
      {Object.entries(facts).map(([name, value]) => (
        <MetadataListItem key={name} label={name.replaceAll("_", " ")}>
          {typeof value === "string" || typeof value === "number" || typeof value === "boolean" ? String(value) : JSON.stringify(value)}
        </MetadataListItem>
      ))}
    </MetadataList>
  );
}

/** The record or document the request is about, as a link where it has a page. */
function Subject({ command: name, input }: { command: string; input: Json }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const command = findCommand(meta, name);
  const id = isObject(input) && typeof input.id === "string" ? input.id : null;
  if (!command || !id) return <Empty />;
  const docType = findDocType(meta, command.resource);
  if (docType) return <Link href={paths.doc(code, docType.name, id)}>{docType.label}</Link>;
  return <Link href={paths.entityRecord(code, command.resource, id)}>{command.resource}</Link>;
}

export function ApprovalsPage() {
  const { meta } = useApp();
  const { code } = useCompany();
  const url = useUrlState();
  const list = findCommand(meta, "approval.list");
  const scopes = useMemo(() => (list ? (commandInputs(list).find((input) => input.name === "scope")?.options ?? []) : []), [list]);
  const scope = url.get("scope") || scopes[0]?.value || "to_approve";
  const status = url.get("status");
  const label = useCommandLabel();
  const requests = useAsync(list && canRun(meta, list.name) ? `approvals|${scope}|${status}` : null, (signal) =>
    api.runNamed("approval.list", { scope, status: status || null, limit: 200 }, { signal }),
  );
  const rows = isObject(requests.data) && Array.isArray(requests.data.items) ? (requests.data.items as unknown as ApprovalRequest[]) : undefined;
  const columns = useMemo<Column<ApprovalRequest>[]>(
    () => [
      { id: "requested", header: t("approval.requested"), type: "datetime", width: 170, value: (r) => r.requested_at, text: (r) => formatTimestamp(r.requested_at) },
      { id: "command", header: t("approval.what"), width: 220, value: (r) => label(r.command) },
      { id: "subject", header: t("approval.on"), width: 160, value: (r) => findCommand(meta, r.command)?.resource ?? "", cell: (r) => <Subject command={r.command} input={r.input} /> },
      { id: "message", header: t("approval.why"), width: 320, value: (r) => r.message ?? "" },
      { id: "by", header: t("approval.asked_by"), width: 160, value: (r) => r.requested_by_name ?? "" },
      {
        id: "status",
        header: t("document.status"),
        width: 120,
        value: (r) => r.status,
        text: (r) => (STATUSES[r.status] ? t((STATUSES[r.status] as { label: MessageKey }).label) : r.status),
        cell: (r) => <StatusOf status={r.status} />,
      },
    ],
    [label, meta],
  );
  if (!list || !canRun(meta, list.name)) return <NotFoundPage />;
  return (
    <Page
      title={list.label}
      padded={false}
      toolbar={
        <HStack gap={3} vAlign="center" hAlign="between" wrap="wrap">
          <TabList value={scope} onChange={(next) => url.set({ scope: next })} size="sm">
            {scopes.map((option) => (
              <Tab key={option.value} value={option.value} label={SCOPES[option.value] ? t(SCOPES[option.value] as MessageKey) : option.label} />
            ))}
          </TabList>
          <Selector
            label={t("document.filter_status")}
            isLabelHidden
            size="sm"
            value={status}
            hasClear={status !== ""}
            placeholder={t("document.any_status")}
            onChange={(next: string | null) => url.set({ status: next ?? null })}
            options={Object.entries(STATUSES).map(([name, known]) => ({ value: name, label: t(known.label) }))}
          />
        </HStack>
      }
    >
      <DataTable
        id="approvals"
        fill
        caption={list.label}
        columns={columns}
        rows={rows}
        rowKey={(r) => r.id}
        rowHref={(r) => paths.approval(code, r.id)}
        loading={requests.loading}
        error={requests.error}
        onRetry={requests.reload}
        empty={{ title: scope === "to_approve" ? t("approval.nothing_to_approve") : t("approval.none") }}
      />
    </Page>
  );
}

export function ApprovalPage() {
  const { meta, me } = useApp();
  const params = useParams();
  const label = useCommandLabel();
  const request = useAsync(params.id && canRun(meta, "approval.get") ? `approval|${params.id}` : null, (signal) =>
    api.runNamed("approval.get", { id: params.id ?? "" }, { signal }),
  );
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const data = isObject(request.data) && typeof request.data.status === "string" ? (request.data as unknown as ApprovalRequest) : undefined;

  async function decide(command: "approval.approve" | "approval.reject"): Promise<void> {
    if (!data) return;
    setBusy(command);
    setError(null);
    try {
      await api.runNamed(command, { id: data.id, reason: reason.trim() || null }, { idempotencyKey: newKey() });
      forgetDocuments();
      setReason("");
      request.reload();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause.message : String(cause));
    }
    setBusy(null);
  }

  if (request.error?.status === 404 || !canRun(meta, "approval.get")) return <NotFoundPage />;
  const approve = findCommand(meta, "approval.approve");
  const reject = findCommand(meta, "approval.reject");
  const pending = data?.status === "pending";
  const mine = data?.requested_by === me.user_id;
  const facts = data && isObject(data.shown_facts) ? data.shown_facts : null;

  return (
    <Page
      back
      kicker={findCommand(meta, "approval.list")?.label}
      title={data ? label(data.command) : t("value.loading")}
      badge={data ? <StatusOf status={data.status} /> : null}
      actions={
        data && pending && !mine ? (
          <>
            {reject && canRun(meta, reject.name) ? (
              <Button
                label={reject.label}
                variant="destructive"
                isLoading={busy === reject.name}
                isDisabled={reason.trim() === "" || busy !== null}
                tooltip={reason.trim() === "" ? t("approval.reason_needed") : undefined}
                onClick={() => void decide("approval.reject")}
              />
            ) : null}
            {approve && canRun(meta, approve.name) ? (
              <Button label={approve.label} variant="primary" isLoading={busy === approve.name} isDisabled={busy !== null} onClick={() => void decide("approval.approve")} />
            ) : null}
          </>
        ) : null
      }
    >
      {data ? (
        <div className="two-columns">
          <VStack gap={4}>
            {error ? <Banner status="error" title={t("approval.decision_failed")} description={error} /> : null}
            <Card padding={4}>
              <MetadataList>
                <MetadataListItem label={t("approval.why")}>{data.message ?? <Empty />}</MetadataListItem>
                <MetadataListItem label={t("approval.asked_by")}>
                  {t("approval.asked_at", { name: data.requested_by_name ?? "", at: formatTimestamp(data.requested_at) })}
                </MetadataListItem>
                <MetadataListItem label={t("approval.expires_at")}>{formatTimestamp(data.expires_at)}</MetadataListItem>
                <MetadataListItem label={t("approval.on")}>
                  <Subject command={data.command} input={data.input} />
                </MetadataListItem>
                {data.error ? <MetadataListItem label={t("approval.outcome")}>{data.error}</MetadataListItem> : null}
              </MetadataList>
            </Card>
            {pending && mine ? <Text type="supporting">{t("approval.own_request")}</Text> : null}
            {pending && !mine && reject && canRun(meta, reject.name) ? (
              <TextArea label={t("approval.reason")} description={t("approval.reason_hint")} value={reason} onChange={setReason} />
            ) : null}
            {data.decisions.length > 0 ? (
              <VStack gap={2}>
                <Heading level={2}>{t("approval.decisions")}</Heading>
                <MetadataList>
                  {data.decisions.map((decision) => (
                    <MetadataListItem key={`${decision.user_id}|${decision.decided_at}`} label={decision.name}>
                      {`${decision.decision === "approve" ? t("approval.approved") : t("approval.rejected")} · ${formatTimestamp(decision.decided_at)}${decision.reason ? ` · ${decision.reason}` : ""}`}
                    </MetadataListItem>
                  ))}
                </MetadataList>
              </VStack>
            ) : null}
          </VStack>
          <VStack gap={4}>
            {facts ? (
              <VStack gap={2}>
                <Heading level={2}>{t("approval.facts")}</Heading>
                <Facts facts={facts} />
              </VStack>
            ) : null}
            <Collapsible trigger={t("approval.input")} defaultIsOpen>
              <CodeBlock code={JSON.stringify(data.input, null, 2)} language="json" />
            </Collapsible>
          </VStack>
        </div>
      ) : null}
    </Page>
  );
}

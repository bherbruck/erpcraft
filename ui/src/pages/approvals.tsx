// The approvals inbox (`approval.list`: what the user may approve, what they
// asked for, or everything) and one request: what was asked, on which facts,
// who decided, and approving or rejecting it. Nobody decides their own request.

import { useMemo, useState } from "react";
import { useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { ApprovalRequest } from "@/api/generated";
import type { Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Approvers } from "@/components/approval-notice";
import { ApprovalFacts, ApprovalInput, ApprovalSubject } from "@/components/approval-parts";
import { type Column, DataTable } from "@/components/data-table";
import { Empty } from "@/components/field-value";
import { PageHeader, Section, Tabs } from "@/components/page";
import { Alert } from "@/components/ui/alert";
import { Badge, type BadgeTone } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { type MessageKey, t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { forgetDocuments } from "@/lib/doc-labels";
import { canRun, commandInputs, findCommand, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

// The statuses a request goes through, as `approval.list` names them. Its schema
// lists them in prose only, so they are listed here with how each looks.
const STATUSES: Record<string, { tone: BadgeTone; label: MessageKey }> = {
  pending: { tone: "warning", label: "approval.status.pending" },
  executed: { tone: "success", label: "approval.status.executed" },
  failed: { tone: "destructive", label: "approval.status.failed" },
  stale: { tone: "neutral", label: "approval.status.stale" },
  rejected: { tone: "destructive", label: "approval.status.rejected" },
  expired: { tone: "neutral", label: "approval.status.expired" },
};

// The views of the inbox, as `approval.list` takes them in `scope`.
const SCOPES: Record<string, MessageKey> = {
  to_approve: "approval.scope.to_approve",
  mine: "approval.scope.mine",
  all: "approval.scope.all",
};

function scopeLabel(scope: string): string {
  const key = SCOPES[scope];
  return key ? t(key) : scope;
}

function StatusOf({ status }: { status: string }) {
  const known = STATUSES[status];
  return <Badge tone={known?.tone ?? "neutral"}>{known ? t(known.label) : status}</Badge>;
}

/** "Purchase order: Approve", from the command's own description. */
function useCommandLabel(): (name: string) => string {
  const { meta } = useApp();
  return (name) => {
    const command = findCommand(meta, name);
    if (!command) return name;
    const resource =
      findDocType(meta, command.resource)?.label ?? meta.entities.find((e) => e.name === command.resource)?.label;
    return resource ? t("approval.command", { resource, action: command.label }) : command.label;
  };
}

export function ApprovalsPage() {
  const { meta } = useApp();
  const { code } = useCompany();
  const url = useUrlState();
  const list = findCommand(meta, "approval.list");
  const scopes = useMemo(
    () => (list ? (commandInputs(list).find((input) => input.name === "scope")?.options ?? []) : []),
    [list],
  );
  const scope = url.get("scope") || scopes[0]?.value || "to_approve";
  const status = url.get("status");
  const label = useCommandLabel();
  const requests = useAsync(list && canRun(meta, list.name) ? `approvals|${scope}|${status}` : null, (signal) =>
    api.runNamed("approval.list", { scope, status: status || null, limit: 200 }, { signal }),
  );
  const rows = isList(requests.data) ? requests.data.items : undefined;

  const columns = useMemo<Column<ApprovalRequest>[]>(
    () => [
      {
        id: "requested",
        header: t("approval.requested"),
        cell: (r) => <span className="whitespace-nowrap">{formatTimestamp(r.requested_at)}</span>,
      },
      {
        id: "command",
        header: t("approval.what"),
        cell: (r) => <span className="whitespace-nowrap font-medium">{label(r.command)}</span>,
      },
      {
        id: "subject",
        header: t("approval.on"),
        cell: (r) => <ApprovalSubject command={r.command} input={r.input} compact />,
      },
      {
        id: "facts",
        header: t("approval.facts_short"),
        cell: (r) =>
          r.shown_facts && typeof r.shown_facts === "object" && !Array.isArray(r.shown_facts) ? (
            <ApprovalFacts point={r.point} facts={r.shown_facts} compact />
          ) : (
            <Empty />
          ),
      },
      { id: "message", header: t("approval.why"), className: "w-full", cell: (r) => r.message ?? <Empty /> },
      {
        id: "by",
        header: t("approval.asked_by"),
        cell: (r) => <span className="whitespace-nowrap">{r.requested_by_name ?? <Empty />}</span>,
      },
      { id: "status", header: t("document.status"), cell: (r) => <StatusOf status={r.status} /> },
    ],
    [label],
  );

  if (!list || !canRun(meta, list.name)) return <NotFoundPage />;
  return (
    <>
      <PageHeader title={list.label} />
      <Tabs
        current={scope}
        tabs={scopes.map((option) => ({
          id: option.value,
          label: scopeLabel(option.value),
          to: `?${new URLSearchParams({ scope: option.value, ...(status ? { status } : {}) })}`,
        }))}
      />
      <div className="mb-3 flex items-center gap-2">
        <Select
          aria-label={t("document.filter_status")}
          className="w-44"
          value={status}
          onChange={(event) => url.set({ status: event.target.value })}
        >
          <option value="">{t("document.any_status")}</option>
          {Object.entries(STATUSES).map(([name, known]) => (
            <option key={name} value={name}>
              {t(known.label)}
            </option>
          ))}
        </Select>
      </div>
      <DataTable
        caption={list.label}
        columns={columns}
        rows={rows}
        rowKey={(r) => r.id}
        rowHref={(r) => paths.approval(code, r.id)}
        loading={requests.loading}
        error={requests.error}
        onRetry={requests.reload}
        empty={<p>{scope === "to_approve" ? t("approval.nothing_to_approve") : t("approval.none")}</p>}
      />
    </>
  );
}

function isList(value: Json | undefined): value is { items: ApprovalRequest[]; total: number } & JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value) && Array.isArray(value.items);
}

function isRequest(value: Json | undefined): value is ApprovalRequest & JsonObject {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    typeof value.status === "string" &&
    typeof value.command === "string"
  );
}

export function ApprovalPage() {
  const { meta, me } = useApp();
  const { code } = useCompany();
  const params = useParams();
  const label = useCommandLabel();
  const request = useAsync(params.id && canRun(meta, "approval.get") ? `approval|${params.id}` : null, (signal) =>
    api.runNamed("approval.get", { id: params.id ?? "" }, { signal }),
  );
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const data = isRequest(request.data) ? request.data : undefined;

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
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(null);
  }

  if (request.error?.status === 404 || !canRun(meta, "approval.get")) return <NotFoundPage />;
  const approve = findCommand(meta, "approval.approve");
  const reject = findCommand(meta, "approval.reject");
  const pending = data?.status === "pending";
  // Nobody decides their own request; the server refuses it regardless.
  const mine = data?.requested_by === me.user_id;
  const command = data ? findCommand(meta, data.command) : undefined;
  const input =
    data && typeof data.input === "object" && data.input !== null && !Array.isArray(data.input) ? data.input : null;
  const target = command?.target && input && typeof input.id === "string" ? input.id : null;
  const facts =
    data && typeof data.shown_facts === "object" && data.shown_facts !== null && !Array.isArray(data.shown_facts)
      ? data.shown_facts
      : null;

  return (
    <>
      <PageHeader
        crumbs={[{ label: findCommand(meta, "approval.list")?.label ?? "", to: paths.approvals(code) }]}
        title={data ? label(data.command) : ""}
        badges={data ? <StatusOf status={data.status} /> : null}
        actions={
          data && pending && !mine ? (
            <>
              {reject && canRun(meta, reject.name) ? (
                <Button
                  variant="destructive-outline"
                  busy={busy === reject.name}
                  disabled={reason.trim() === "" || busy !== null}
                  title={reason.trim() === "" ? t("approval.reason_needed") : undefined}
                  onClick={() => void decide("approval.reject")}
                >
                  {reject.label}
                </Button>
              ) : null}
              {approve && canRun(meta, approve.name) ? (
                <Button
                  busy={busy === approve.name}
                  disabled={busy !== null}
                  onClick={() => void decide("approval.approve")}
                >
                  {approve.label}
                </Button>
              ) : null}
            </>
          ) : null
        }
      />
      {error ? <Alert title={t("approval.decision_failed")}>{error.message}</Alert> : null}
      {data ? (
        <div className="grid max-w-5xl gap-5 lg:grid-cols-2">
          <div className="space-y-4">
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-md border bg-card p-4 text-sm">
              <dt className="text-muted-foreground">{t("approval.why")}</dt>
              <dd>{data.message ?? <Empty />}</dd>
              <dt className="text-muted-foreground">{t("approval.asked_by")}</dt>
              <dd>
                {t("approval.asked_at", { name: data.requested_by_name ?? "", at: formatTimestamp(data.requested_at) })}
              </dd>
              <dt className="text-muted-foreground">{t("approval.decided_by")}</dt>
              <dd>
                <Approvers
                  approval={{
                    approvers: {
                      role_id: data.approver_role_id,
                      permission: data.approver_permission,
                      approvals_needed: data.approvals_needed,
                    },
                  }}
                />
              </dd>
              <dt className="text-muted-foreground">{t("approval.expires_at")}</dt>
              <dd>{formatTimestamp(data.expires_at)}</dd>
              {target ? (
                <>
                  <dt className="text-muted-foreground">{t("approval.on")}</dt>
                  <dd>
                    <ApprovalSubject command={data.command} input={data.input} />
                  </dd>
                </>
              ) : null}
              {data.error ? (
                <>
                  <dt className="text-muted-foreground">{t("approval.outcome")}</dt>
                  <dd className="text-destructive">{data.error}</dd>
                </>
              ) : null}
            </dl>
            {pending && mine ? <p className="text-sm text-muted-foreground">{t("approval.own_request")}</p> : null}
            {pending && !mine && reject && canRun(meta, reject.name) ? (
              <div className="space-y-1">
                <Label htmlFor="approval-reason">{t("approval.reason")}</Label>
                <Textarea
                  id="approval-reason"
                  rows={3}
                  value={reason}
                  onChange={(event) => setReason(event.target.value)}
                />
                <p className="text-xs text-muted-foreground">{t("approval.reason_hint")}</p>
              </div>
            ) : null}
            {data.decisions.length > 0 ? (
              <Section title={t("approval.decisions")}>
                <ul className="divide-y rounded-md border bg-card text-sm">
                  {data.decisions.map((decision) => (
                    <li key={`${decision.user_id}|${decision.decided_at}`} className="px-3 py-2">
                      <span className="font-medium">{decision.name}</span>{" "}
                      {decision.decision === "approve" ? t("approval.approved") : t("approval.rejected")}{" "}
                      <span className="text-muted-foreground">{formatTimestamp(decision.decided_at)}</span>
                      {decision.reason ? <p className="text-muted-foreground">{decision.reason}</p> : null}
                    </li>
                  ))}
                </ul>
              </Section>
            ) : null}
          </div>
          <div className="space-y-4">
            {facts ? (
              <Section title={t("approval.facts")}>
                <ApprovalFacts point={data.point} facts={facts} />
              </Section>
            ) : null}
            <Section title={t("approval.input")}>
              <ApprovalInput command={data.command} input={data.input} />
              <details className="text-sm">
                <summary className="cursor-pointer text-muted-foreground">{t("approval.details")}</summary>
                <pre className="mt-2 overflow-x-auto rounded-md border bg-muted p-3 font-mono text-xs">
                  {JSON.stringify(data.input, null, 2)}
                </pre>
              </details>
            </Section>
          </div>
        </div>
      ) : request.loading ? (
        <p className="text-sm text-muted-foreground">{t("value.loading")}</p>
      ) : null}
    </>
  );
}

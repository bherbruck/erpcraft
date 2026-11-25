// Delegating approvals (`approval.delegate`): while away, someone else decides the requests the
// user could decide. Lists what the user delegated and what was delegated to them
// (`approval.delegations`), and ends a delegation (`approval.end_delegation`). Shown only to
// those who may delegate (approval:delegate).

import { Plus } from "lucide-react";
import { useState } from "react";
import { ApiError, api } from "@/api/client";
import type { DelegationView } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge } from "@/components/badge";
import { Button } from "@/components/button";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { type Column, DataTable } from "@/components/data-table";
import { DateInput } from "@/components/date-input";
import { Dialog } from "@/components/dialog";
import { Empty } from "@/components/field-value";
import { FormField } from "@/components/form-field";
import { Lookup } from "@/components/lookup";
import { Textarea } from "@/components/ui/textarea";
import { t } from "@/i18n/t";
import { formatCalendarDate, todayCalendarDate } from "@/lib/dates";
import { canRun, commandInputs, findCommand } from "@/lib/meta";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";

export const DELEGATE = "approval.delegate";

/** Whether the user may hand on their approvals, which is what the screen is for. */
export function canDelegate(meta: Parameters<typeof canRun>[0]): boolean {
  return canRun(meta, DELEGATE) && canRun(meta, "approval.delegations");
}

export function DelegationsPanel() {
  const { meta, me } = useApp();
  const list = useAsync("delegations", (signal) => api.run("approval.delegations", {}, { signal }));
  const [creating, setCreating] = useState(false);
  const [ending, setEnding] = useState<DelegationView | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const today = todayCalendarDate();

  async function end(): Promise<void> {
    if (!ending) return;
    setBusy(true);
    setError(null);
    try {
      await api.run("approval.end_delegation", { id: ending.id }, { idempotencyKey: newKey() });
      setEnding(null);
      list.reload();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  const columns: Column<DelegationView>[] = [
    {
      id: "user",
      header: t("delegation.from"),
      cell: (d) => <span className="whitespace-nowrap">{d.user_name}</span>,
    },
    {
      id: "delegate",
      header: t("delegation.to"),
      cell: (d) => <span className="whitespace-nowrap font-medium">{d.delegate_name}</span>,
    },
    {
      id: "dates",
      header: t("delegation.when"),
      cell: (d) => (
        <span className="whitespace-nowrap">
          {t("delegation.range", { from: formatCalendarDate(d.starts_on), to: formatCalendarDate(d.ends_on) })}
        </span>
      ),
    },
    {
      id: "state",
      header: t("document.status"),
      cell: (d) =>
        d.ended_at ? (
          <Badge tone="neutral">{t("delegation.ended")}</Badge>
        ) : d.starts_on > today ? (
          <Badge tone="info">{t("delegation.planned")}</Badge>
        ) : (
          <Badge tone="success">{t("delegation.active")}</Badge>
        ),
    },
    { id: "reason", header: t("delegation.reason"), className: "w-full", cell: (d) => d.reason ?? <Empty /> },
    {
      id: "end",
      header: t("delegation.end"),
      headerNode: <span className="sr-only">{t("delegation.end")}</span>,
      cell: (d) =>
        // Only who gave a delegation ends it.
        d.user_id === me.user_id && !d.ended_at && canRun(meta, "approval.end_delegation") ? (
          <Button variant="outline" size="sm" onClick={() => setEnding(d)}>
            {t("delegation.end")}
          </Button>
        ) : null,
    },
  ];

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="max-w-3xl text-sm text-muted-foreground">{t("delegation.intro")}</p>
        <Button onClick={() => setCreating(true)}>
          <Plus aria-hidden="true" />
          {findCommand(meta, DELEGATE)?.label}
        </Button>
      </div>
      <DataTable
        caption={t("delegation.title")}
        columns={columns}
        rows={list.data?.items}
        rowKey={(d) => d.id}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        empty={<p>{t("delegation.none")}</p>}
      />
      {creating ? (
        <DelegateDialog
          onClose={() => setCreating(false)}
          onDone={() => {
            setCreating(false);
            list.reload();
          }}
        />
      ) : null}
      <ConfirmDialog
        open={ending !== null}
        title={t("delegation.end_title", { name: ending?.delegate_name ?? "" })}
        description={t("delegation.end_description")}
        confirmLabel={t("delegation.end")}
        busy={busy}
        error={error}
        onConfirm={() => void end()}
        onClose={() => {
          setEnding(null);
          setError(null);
        }}
      />
    </div>
  );
}

function DelegateDialog({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { meta } = useApp();
  const { company } = useCompany();
  const command = findCommand(meta, DELEGATE);
  const inputs = command ? commandInputs(command) : [];
  const labelOf = (name: string): string => inputs.find((input) => input.name === name)?.label ?? name;
  const hintOf = (name: string): string | null => inputs.find((input) => input.name === name)?.description ?? null;
  const [delegate, setDelegate] = useState<string | null>(null);
  const [startsOn, setStartsOn] = useState(todayCalendarDate());
  const [endsOn, setEndsOn] = useState("");
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [key] = useState(newKey);
  const dirty = delegate !== null || endsOn !== "" || reason !== "";

  async function save(): Promise<void> {
    if (!delegate || !endsOn) return;
    setBusy(true);
    setError(null);
    try {
      await api.run(
        DELEGATE,
        { delegate_id: delegate, starts_on: startsOn || null, ends_on: endsOn, reason: reason.trim() || null },
        { idempotencyKey: key },
      );
      onDone();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setBusy(false);
    }
  }

  const field = (name: string): string | undefined => (error?.field === name ? error.message : undefined);

  return (
    <Dialog
      open
      dirty={dirty}
      onClose={onClose}
      title={command?.label ?? DELEGATE}
      description={command?.description}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {t("action.cancel")}
          </Button>
          <Button busy={busy} disabled={!delegate || !endsOn} onClick={() => void save()}>
            {command?.label}
          </Button>
        </>
      }
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField
          id="delegation-delegate_id"
          label={labelOf("delegate_id")}
          hint={hintOf("delegate_id")}
          required
          error={field("delegate_id")}
          labelFor={null}
          className="sm:col-span-2"
        >
          <Lookup
            id="delegation-delegate_id"
            aria-label={labelOf("delegate_id")}
            entity="user"
            keyField="id"
            value={delegate}
            companyId={company?.id}
            allowCreate={false}
            recordLink={false}
            onChange={(next) => setDelegate(next)}
          />
        </FormField>
        <FormField
          id="delegation-starts_on"
          label={labelOf("starts_on")}
          hint={hintOf("starts_on")}
          error={field("starts_on")}
        >
          <DateInput id="delegation-starts_on" value={startsOn} onChange={setStartsOn} />
        </FormField>
        <FormField
          id="delegation-ends_on"
          label={labelOf("ends_on")}
          hint={hintOf("ends_on")}
          required
          error={field("ends_on")}
        >
          <DateInput id="delegation-ends_on" value={endsOn} onChange={setEndsOn} />
        </FormField>
        <FormField
          id="delegation-reason"
          label={labelOf("reason")}
          hint={hintOf("reason")}
          error={field("reason")}
          className="sm:col-span-2"
        >
          <Textarea id="delegation-reason" value={reason} onChange={(event) => setReason(event.target.value)} />
        </FormField>
      </div>
      {error && !error.field ? <Alert title={t("delegation.refused")}>{error.message}</Alert> : null}
    </Dialog>
  );
}

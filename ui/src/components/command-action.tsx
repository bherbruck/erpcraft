import { ArrowRight } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { ApiError, api } from "@/api/client";
import type { FieldMeta, Json, JsonObject, Posting } from "@/api/types";
import { useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Dialog } from "@/components/dialog";
import { FormField } from "@/components/form-field";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { type PendingApproval, pendingApproval } from "@/lib/approval";
import { formatCalendarDate } from "@/lib/dates";
import type { ResourceAction } from "@/lib/meta";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { ApprovalNotice } from "./approval-notice";
import { CommandInput, InputLabelText } from "./command-input";
import { FieldValue } from "./field-value";
import { PostingEntries } from "./postings";
import { StatusBadge } from "./status-badge";

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Buttons for the actions the registry offers on a record as it is now. An
 * action that ends or undoes something looks different. An action that drafts
 * a follow-on document runs at once and opens the draft, since nothing it does
 * is final; every other action opens its preview first.
 */
export function ActionButtons({
  actions,
  disabledReason,
  running,
  onChoose,
}: {
  actions: ResourceAction[];
  /** Why the actions cannot be used right now, such as unsaved changes. */
  disabledReason?: string | null;
  /** The follow-on action being run, if any. */
  running?: string | null;
  onChoose: (action: ResourceAction) => void;
}) {
  return (
    <>
      {actions.map((action) => (
        <Button
          key={action.action}
          variant={action.destructive ? "destructive" : "outline"}
          disabled={Boolean(disabledReason) || (running != null && running !== action.action)}
          busy={running === action.action}
          title={disabledReason ?? action.command.description}
          onClick={() => onChoose(action)}
        >
          {action.label}
          {action.creates ? <ArrowRight className="rtl:rotate-180" aria-hidden="true" /> : null}
        </Button>
      ))}
    </>
  );
}

/**
 * Runs an action that drafts a follow-on document, once per click: a double
 * click or a retry sends the same idempotency key and gets the same draft.
 */
export function useFollowOn(onCreated: (docType: string, id: string) => void): {
  running: string | null;
  error: ApiError | null;
  /** The approval the last action waits for, instead of having run. */
  pending: PendingApproval | null;
  run: (action: ResourceAction, id: string) => Promise<void>;
  clear: () => void;
} {
  const [running, setRunning] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [pending, setPending] = useState<PendingApproval | null>(null);
  const keys = useRef(new Map<string, string>());
  async function run(action: ResourceAction, id: string): Promise<void> {
    if (!action.creates || running) return;
    const slot = `${action.command.name}|${id}`;
    const key = keys.current.get(slot) ?? newKey();
    keys.current.set(slot, key);
    setRunning(action.action);
    setError(null);
    try {
      const result = await api.runNamed(action.command.name, { id }, { idempotencyKey: key });
      const waiting = pendingApproval(result);
      if (waiting) {
        setPending(waiting);
        setRunning(null);
        return;
      }
      const created = isObject(result) && typeof result.id === "string" ? result.id : null;
      setRunning(null);
      if (created) onCreated(action.creates, created);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setRunning(null);
    }
  }
  return {
    running,
    error,
    pending,
    run,
    clear: () => {
      setError(null);
      setPending(null);
    },
  };
}

// Bookkeeping of the record itself, which says nothing a person needs before confirming.
const HIDDEN_CHANGES = new Set(["version", "_version", "updated_at", "created_at", "locked", "body", "summary"]);

/** A posting as the server returns it with a document, checked for what this screen reads. */
const OUTCOME = new Set(["number", "status", "doc_date"]);

function isPosting(value: Json): value is Posting & JsonObject {
  return (
    isObject(value) &&
    typeof value.id === "string" &&
    typeof value.posting_date === "string" &&
    typeof value.created_at === "string" &&
    isObject(value.entries) &&
    Object.values(value.entries).every(Array.isArray)
  );
}

function postingsOf(value: Json | undefined): Posting[] {
  return Array.isArray(value) ? value.filter(isPosting) : [];
}

/** What a previewed command would change, compared with the record as it is. */
function PreviewSummary({
  before,
  after,
  fields,
  states,
}: {
  before: JsonObject;
  after: Json;
  /** The record's top-level fields, to name and show what changes. */
  fields: FieldMeta[];
  states?: Parameters<typeof StatusBadge>[0]["docType"];
}) {
  const { company } = useCompany();
  if (!isObject(after)) return <p>{t("preview.no_change")}</p>;
  // A record the command creates is told by its outcome only; the rest is bookkeeping.
  const created = Object.keys(before).length === 0;
  const changed = fields.filter((field) => {
    if (HIDDEN_CHANGES.has(field.name)) return false;
    if (created && !OUTCOME.has(field.name)) return false;
    const [a, b] = [before[field.name] ?? null, after[field.name] ?? null];
    return (typeof b !== "object" || b === null) && a !== b;
  });
  const known = new Set(postingsOf(before.postings).map((posting) => posting.id));
  const postings = postingsOf(after.postings).filter((posting) => !known.has(posting.id));
  const value = (field: FieldMeta, v: Json | undefined) =>
    field.name === "status" && typeof v === "string" && states ? (
      <StatusBadge status={v} docType={states} />
    ) : (
      <FieldValue field={field} value={v} />
    );

  return (
    <div className="space-y-3 text-sm">
      {changed.length === 0 && postings.length === 0 ? <p>{t("preview.no_change")}</p> : null}
      {changed.length > 0 ? (
        <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-1.5">
          {changed.map((field) => (
            <div key={field.name} className="contents">
              <dt className="text-muted-foreground">{field.label}</dt>
              <dd className="flex flex-wrap items-center gap-2">
                {value(field, before[field.name])}
                <span aria-hidden="true" className="text-muted-foreground rtl:rotate-180">
                  {t("preview.arrow")}
                </span>
                <span className="sr-only">{t("preview.becomes")}</span>
                {value(field, after[field.name])}
              </dd>
            </div>
          ))}
        </dl>
      ) : null}
      {postings.map((posting) => (
        <section key={posting.id} className="rounded-md border">
          <h4 className="border-b px-3 py-1.5 font-medium">
            {t(posting.reverses_id ? "preview.posting_reversal" : "preview.posting", {
              date: formatCalendarDate(posting.posting_date),
            })}
          </h4>
          <PostingEntries posting={posting} currency={company?.base_currency ?? ""} />
        </section>
      ))}
    </div>
  );
}

/**
 * Runs one action on one record: first as a preview, showing what the server
 * says will happen and which ledger entries it will make, then for real once
 * the user confirms.
 */
export function ActionDialog({
  action,
  input: given,
  subject,
  current,
  fields,
  docType,
  prefill = false,
  onClose,
  onDone,
}: {
  action: ResourceAction;
  /** What the command is given besides what the dialog asks for: the record's `id`, or the items to settle. */
  input: JsonObject;
  /** What the action is run on, in words: "Journal entry JE-2026-00001". */
  subject: string;
  /** The record as the page shows it, to compare the preview with. */
  current: object;
  /** The record's top-level fields. */
  fields: FieldMeta[];
  docType?: Parameters<typeof StatusBadge>[0]["docType"];
  /** Start the action's inputs from the record's fields of the same names. */
  prefill?: boolean;
  onClose: () => void;
  onDone: (result: Json) => void;
}) {
  const { company } = useCompany();
  // An action that edits the record (Edit user) starts from what the record holds.
  const [initial] = useState<Record<string, Json>>(() => {
    const out: Record<string, Json> = {};
    if (!prefill) return out;
    for (const [name, value] of Object.entries(current)) {
      if (
        action.inputs.some((input) => input.name === name) &&
        (typeof value === "string" || typeof value === "boolean")
      )
        out[name] = value;
    }
    return out;
  });
  const [extra, setExtra] = useState<Record<string, Json>>(initial);
  const [running, setRunning] = useState(false);
  const [failure, setFailure] = useState<ApiError | null>(null);
  const [requested, setRequested] = useState<PendingApproval | null>(null);
  const [key, setKey] = useState(newKey);

  const input = useMemo(() => {
    const out: JsonObject = { ...given };
    for (const [name, value] of Object.entries(extra)) if (value !== "" && value !== null) out[name] = value;
    return out;
  }, [given, extra]);
  const inputKey = JSON.stringify(input);
  // The record as JSON, which is what it came from the server as.
  const before = useMemo(() => JSON.parse(JSON.stringify(current)) as JsonObject, [current]);
  // What the caller gives is not asked again.
  const asked = action.inputs.filter((property) => !(property.name in given));

  // A changed input is a new request, so it gets a new idempotency key.
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs when the input changes
  useEffect(() => {
    setKey(newKey());
    setFailure(null);
  }, [inputKey]);

  const [settled, setSettled] = useState(inputKey);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(inputKey), 300);
    return () => clearTimeout(timer);
  }, [inputKey]);

  const preview = useAsync(`${action.command.name}|${settled}`, (signal) =>
    api.preview(action.command.name, JSON.parse(settled) as JsonObject, { signal }),
  );
  const ready = preview.data !== undefined && !preview.loading && !preview.error && settled === inputKey;
  const previewApproval = pendingApproval(preview.data);
  const needsApproval = previewApproval !== null;

  async function confirm(): Promise<void> {
    setRunning(true);
    setFailure(null);
    try {
      const result = await api.runNamed(action.command.name, input, { idempotencyKey: key });
      // Not run: it waits for approval. Say so here, and leave closing to the user.
      const waiting = pendingApproval(result);
      if (waiting) {
        setRequested(waiting);
        setRunning(false);
        return;
      }
      toast.success(t("toast.done", { action: action.label }));
      onDone(result);
    } catch (cause) {
      setFailure(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setRunning(false);
    }
  }

  return (
    <Dialog
      open
      wide
      onClose={onClose}
      dirty={JSON.stringify(extra) !== JSON.stringify(initial) || running}
      title={t("preview.title", { action: action.label, subject })}
      description={action.command.description}
      footer={
        <>
          <Button variant="outline" onClick={requested ? () => onDone(null) : onClose} disabled={running}>
            {requested ? t("action.close") : t("action.cancel")}
          </Button>
          {requested ? null : (
            <Button
              variant={action.destructive ? "destructive" : "default"}
              disabled={!ready}
              busy={running}
              onClick={() => void confirm()}
            >
              {needsApproval
                ? t("approval.ask", { action: action.label })
                : t("preview.confirm", { action: action.label })}
            </Button>
          )}
        </>
      }
    >
      <div className="space-y-4">
        {asked.length > 0 ? (
          <div className="grid gap-4 sm:grid-cols-2">
            {asked.map((property) => {
              const fieldId = `action-${property.name}`;
              const held = extra[property.name];
              const change = (next: Json): void => setExtra((previous) => ({ ...previous, [property.name]: next }));
              return (
                <FormField
                  key={property.name}
                  id={fieldId}
                  label={<InputLabelText property={property} />}
                  hint={property.description}
                >
                  <CommandInput
                    property={property}
                    id={fieldId}
                    value={held}
                    onChange={change}
                    disabled={running}
                    companyId={company?.id ?? null}
                  />
                </FormField>
              );
            })}
          </div>
        ) : null}
        <section aria-live="polite" className="space-y-2">
          <h3 className="text-sm font-semibold">{t("preview.heading")}</h3>
          {requested ? (
            <ApprovalNotice approval={requested} preview={false} />
          ) : preview.error ? (
            <Alert title={t("preview.refused")}>{preview.error.message}</Alert>
          ) : previewApproval ? (
            <ApprovalNotice approval={previewApproval} preview />
          ) : preview.data === undefined ? (
            <div className="space-y-2">
              <Skeleton className="w-2/3" />
              <Skeleton className="w-1/2" />
            </div>
          ) : (
            <PreviewSummary before={before} after={preview.data} fields={fields} states={docType} />
          )}
        </section>
        {failure ? <Alert title={t("preview.failed", { action: action.label })}>{failure.message}</Alert> : null}
      </div>
    </Dialog>
  );
}

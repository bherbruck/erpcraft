// One action on several documents. The registry says from which states each
// action applies, so only actions every chosen document's state allows are
// offered. Nothing is previewed: the confirmation says what will run on how
// many. Each document is its own request with its own idempotency key, and the
// result is reported per document, as the guidelines require.

import { CheckCircle2, CircleX } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { ApiError, api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import { t } from "@/i18n/t";
import { pendingApproval } from "@/lib/approval";
import { forgetDocuments } from "@/lib/doc-labels";
import { type ResourceAction, recordActions } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";

export interface BulkResult {
  doc: DocumentSummary;
  ok: boolean;
  message: string | null;
  /** The number the action gave the document, such as when it was posted. */
  number: string | null;
  /** The approval request it waits for, where it did not run for that. */
  approval?: string | null;
}

/** Actions that apply to every one of the documents and need nothing but the document. */
export function bulkActions(
  meta: Parameters<typeof recordActions>[0],
  docType: string,
  docs: DocumentSummary[],
): ResourceAction[] {
  if (docs.length === 0) return [];
  const sets = docs.map((doc) => recordActions(meta, docType, doc.status));
  const first = sets[0] ?? [];
  return first.filter(
    (action) =>
      action.creates === null &&
      action.inputs.every((input) => !input.required) &&
      sets.every((set) => set.some((other) => other.command.name === action.command.name)),
  );
}

export function BulkBar({
  docType,
  chosen,
  onClear,
  onDone,
}: {
  docType: DocTypeMeta;
  chosen: DocumentSummary[];
  onClear: () => void;
  onDone: (results: BulkResult[], action: string) => void;
}) {
  const { meta } = useApp();
  const actions = bulkActions(meta, docType.name, chosen);
  const [confirming, setConfirming] = useState<ResourceAction | null>(null);
  const [running, setRunning] = useState<{ done: number; total: number } | null>(null);

  async function run(action: ResourceAction): Promise<void> {
    const docs = [...chosen];
    setRunning({ done: 0, total: docs.length });
    const results: BulkResult[] = [];
    // One after the other: a posting takes locks, and the order of the list is the order of the work.
    for (const doc of docs) {
      try {
        const after = await api.runNamed(action.command.name, { id: doc.id }, { idempotencyKey: newKey() });
        const waiting = pendingApproval(after);
        if (waiting) {
          results.push({
            doc,
            ok: false,
            message: waiting.message ?? t("approval.waiting"),
            number: doc.number,
            approval: waiting.id,
          });
          setRunning({ done: results.length, total: docs.length });
          continue;
        }
        const number = after && typeof after === "object" && !Array.isArray(after) ? after.number : null;
        results.push({ doc, ok: true, message: null, number: typeof number === "string" ? number : doc.number });
      } catch (cause) {
        results.push({
          doc,
          ok: false,
          message: cause instanceof ApiError ? cause.message : t("error.unexpected"),
          number: doc.number,
        });
      }
      setRunning({ done: results.length, total: docs.length });
    }
    forgetDocuments();
    setRunning(null);
    setConfirming(null);
    onDone(results, action.label);
  }

  return (
    <div className="mb-3 flex flex-wrap items-center gap-2 rounded-md border bg-card p-2 text-sm">
      <span className="px-1 font-medium">{t("bulk.chosen", { count: chosen.length })}</span>
      {actions.length === 0 ? (
        <span className="text-muted-foreground">{t("bulk.none_common")}</span>
      ) : (
        actions.map((action) => (
          <Button
            key={action.command.name}
            size="sm"
            variant={action.destructive ? "destructive-outline" : "outline"}
            onClick={() => setConfirming(action)}
          >
            {action.label}
          </Button>
        ))
      )}
      <Button size="sm" variant="ghost" className="ms-auto" onClick={onClear}>
        {t("bulk.clear")}
      </Button>
      {confirming ? (
        <Dialog
          open
          onClose={() => setConfirming(null)}
          dirty={running !== null}
          title={t("bulk.confirm_title", { action: confirming.label, count: chosen.length, label: docType.plural })}
          description={confirming.command.description}
          footer={
            <>
              <Button variant="outline" onClick={() => setConfirming(null)} disabled={running !== null}>
                {t("action.cancel")}
              </Button>
              <Button
                variant={confirming.destructive ? "destructive" : "default"}
                busy={running !== null}
                onClick={() => void run(confirming)}
              >
                {t("bulk.confirm", { action: confirming.label, count: chosen.length })}
              </Button>
            </>
          }
        >
          <p className="text-sm">
            {running
              ? t("bulk.progress", { done: running.done, total: running.total })
              : t("bulk.no_preview", { action: confirming.label, count: chosen.length })}
          </p>
        </Dialog>
      ) : null}
    </div>
  );
}

/** What happened to each document of the last bulk action. */
export function BulkResults({
  docType,
  action,
  results,
  onClose,
}: {
  docType: DocTypeMeta;
  action: string;
  results: BulkResult[];
  onClose: () => void;
}) {
  const { code } = useCompany();
  const failed = results.filter((result) => !result.ok).length;
  return (
    <Alert
      tone={failed > 0 ? "warning" : "info"}
      className="mb-3"
      title={t("bulk.result_title", { action, ok: results.length - failed, failed })}
      actions={
        <Button variant="outline" size="sm" onClick={onClose}>
          {t("action.close")}
        </Button>
      }
    >
      <ul className="mt-1 space-y-1">
        {results.map((result) => (
          <li key={result.doc.id} className="flex items-start gap-2">
            {result.ok ? (
              <CheckCircle2 className="mt-0.5 size-4 shrink-0 text-success" aria-label={t("bulk.succeeded")} />
            ) : (
              <CircleX className="mt-0.5 size-4 shrink-0 text-destructive" aria-label={t("bulk.refused")} />
            )}
            <Link
              to={paths.doc(code, docType.name, result.doc.id)}
              className="shrink-0 font-medium underline underline-offset-2 hover:no-underline"
            >
              {result.number ?? t("document.unnumbered")}
            </Link>
            {result.doc.memo ? <span className="text-muted-foreground">{result.doc.memo}</span> : null}
            <span className={result.ok ? "text-muted-foreground" : ""}>
              {result.ok ? t("bulk.succeeded") : result.message}
            </span>
            {result.approval ? (
              <Link
                to={paths.approval(code, result.approval)}
                className="underline underline-offset-2 hover:no-underline"
              >
                {t("approval.open_request")}
              </Link>
            ) : null}
          </li>
        ))}
      </ul>
    </Alert>
  );
}

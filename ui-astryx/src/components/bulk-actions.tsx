// One action on several documents. Only actions every chosen document's state allows are
// offered. Each document is its own request with its own idempotency key, and the result is
// reported per document.

import { AlertDialog } from "@astryxdesign/core/AlertDialog";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { Link } from "@astryxdesign/core/Link";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { CheckCircle2, CircleX } from "lucide-react";
import { useState } from "react";
import { ApiError, api } from "@/api/client";
import type { DocTypeMeta, DocumentSummary } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { pendingApproval } from "@/lib/approval";
import { forgetDocuments } from "@/lib/doc-labels";
import { recordActions, type ResourceAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";

export interface BulkResult {
  doc: DocumentSummary;
  ok: boolean;
  message: string | null;
  number: string | null;
  approval?: string | null;
}

/** Actions that apply to every one of the documents and need nothing but the document. */
export function bulkActions(meta: Parameters<typeof recordActions>[0], docType: string, docs: DocumentSummary[]): ResourceAction[] {
  if (docs.length === 0) return [];
  const sets = docs.map((doc) => recordActions(meta, docType, doc.status));
  return (sets[0] ?? []).filter(
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
        if (waiting) results.push({ doc, ok: false, message: waiting.message ?? t("approval.waiting"), number: doc.number, approval: waiting.id });
        else {
          const number = after && typeof after === "object" && !Array.isArray(after) ? after.number : null;
          results.push({ doc, ok: true, message: null, number: typeof number === "string" ? number : doc.number });
        }
      } catch (cause) {
        results.push({ doc, ok: false, message: cause instanceof ApiError ? cause.message : t("error.unexpected"), number: doc.number });
      }
      setRunning({ done: results.length, total: docs.length });
    }
    forgetDocuments();
    setRunning(null);
    setConfirming(null);
    onDone(results, action.label);
  }

  return (
    <Card padding={2} variant="muted">
      <HStack gap={2} vAlign="center" wrap="wrap">
        <Text weight="medium">{t("bulk.chosen", { count: chosen.length })}</Text>
        {actions.length === 0 ? (
          <Text type="supporting">{t("bulk.none_common")}</Text>
        ) : (
          actions.map((action) => (
            <Button key={action.command.name} label={action.label} size="sm" variant={action.destructive ? "destructive" : "secondary"} onClick={() => setConfirming(action)} />
          ))
        )}
        <span className="push-end">
          <Button label={t("bulk.clear")} size="sm" variant="ghost" onClick={onClear} />
        </span>
      </HStack>
      {confirming ? (
        <AlertDialog
          isOpen
          onOpenChange={(open) => (open || running ? null : setConfirming(null))}
          title={t("bulk.confirm_title", { action: confirming.label, count: chosen.length, label: docType.plural })}
          description={
            running ? t("bulk.progress", { done: running.done, total: running.total }) : t("bulk.no_preview", { action: confirming.label, count: chosen.length })
          }
          actionLabel={t("bulk.confirm", { action: confirming.label, count: chosen.length })}
          actionVariant={confirming.destructive ? "destructive" : "primary"}
          isActionLoading={running !== null}
          onAction={() => void run(confirming)}
        />
      ) : null}
    </Card>
  );
}

/** What happened to each document of the last bulk action. */
export function BulkResults({ docType, action, results, onClose }: { docType: DocTypeMeta; action: string; results: BulkResult[]; onClose: () => void }) {
  const { code } = useCompany();
  const failed = results.filter((result) => !result.ok).length;
  return (
    <Banner
      status={failed > 0 ? "warning" : "success"}
      title={t("bulk.result_title", { action, ok: results.length - failed, failed })}
      endContent={<Button label={t("action.close")} size="sm" onClick={onClose} />}
      collapsible={false}
    >
      <VStack gap={1}>
        {results.map((result) => (
          <HStack key={result.doc.id} gap={2} vAlign="center" wrap="wrap">
            <Icon icon={result.ok ? CheckCircle2 : CircleX} color={result.ok ? "success" : "error"} label={result.ok ? t("bulk.succeeded") : t("bulk.refused")} />
            <Link href={paths.doc(code, docType.name, result.doc.id)}>{result.number ?? t("document.unnumbered")}</Link>
            {result.doc.memo ? <Text type="supporting">{result.doc.memo}</Text> : null}
            <Text>{result.ok ? t("bulk.succeeded") : (result.message ?? "")}</Text>
            {result.approval ? <Link href={paths.approval(code, result.approval)}>{t("approval.open_request")}</Link> : null}
          </HStack>
        ))}
      </VStack>
    </Banner>
  );
}

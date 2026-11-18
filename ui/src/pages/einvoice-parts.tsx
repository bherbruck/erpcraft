// E-invoicing on the screens that already exist: a posted sales invoice or
// credit note exports itself as an EN 16931 invoice (`einvoice.export`), and
// documents are named by their number and party wherever the module refers to one.

import { FileCode } from "lucide-react";
import { useState } from "react";
import { Link } from "react-router";
import { ApiError, api } from "@/api/client";
import type { EinvoiceExportInput, EinvoiceExportOutput } from "@/api/generated";
import type { JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Dialog } from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { t } from "@/i18n/t";
import { documentLabel, useDocumentHeader } from "@/lib/doc-labels";
import { canDoc, canRun, commandInputs, findCommand, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";

const EXPORT = "einvoice.export";

/** A document by its type, number and party, linked where the user may read it. */
export function DocumentLinkLabel({ docType, id }: { docType: string; id: string }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const type = findDocType(meta, docType);
  const header = useDocumentHeader(docType, id);
  const label = `${type?.label ?? docType} ${header ? documentLabel(type, header) : ""}`.trim();
  return canDoc(meta, docType, "read") ? (
    <Link to={paths.doc(code, docType, id)} className="underline underline-offset-2 hover:no-underline">
      {label}
    </Link>
  ) : (
    <span>{label}</span>
  );
}

/** The document types `einvoice.export` takes, from its schema. */
function useExportable(): string[] {
  const { meta } = useApp();
  const command = findCommand(meta, EXPORT);
  if (!command || !canRun(meta, EXPORT)) return [];
  return (
    commandInputs(command)
      .find((input) => input.name === "doc_type")
      ?.options?.map((option) => option.value) ?? []
  );
}

/** "Export e-invoice" on a posted document the module can send. */
export function EinvoiceExportButton({ docType, id, locked }: { docType: string; id: string; locked: boolean }) {
  const exportable = useExportable();
  const [open, setOpen] = useState(false);
  if (!locked || !exportable.includes(docType)) return null;
  return (
    <>
      <Button variant="outline" onClick={() => setOpen(true)}>
        <FileCode aria-hidden="true" />
        {t("einvoice.export")}
      </Button>
      {open ? <ExportDialog docType={docType} id={id} onClose={() => setOpen(false)} /> : null}
    </>
  );
}

/** Saves text as a file the browser downloads. */
function download(name: string, mediaType: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: mediaType }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function ExportDialog({ docType, id, onClose }: { docType: string; id: string; onClose: () => void }) {
  const { meta } = useApp();
  const command = findCommand(meta, EXPORT);
  const inputs = command ? commandInputs(command) : [];
  const syntaxes = inputs.find((input) => input.name === "syntax")?.options ?? [];
  const profiles = inputs.find((input) => input.name === "profile")?.options ?? [];
  const [syntax, setSyntax] = useState(syntaxes[0]?.value ?? "");
  const [profile, setProfile] = useState(profiles[0]?.value ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [done, setDone] = useState<EinvoiceExportOutput | null>(null);

  async function run(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      // The schema's own options are what the dialog offers; they are the values the types name.
      const input = { doc_type: docType, id, syntax: syntax || null, profile: profile || null } as JsonObject;
      const answer = await api.run("einvoice.export", input as EinvoiceExportInput);
      setDone(answer);
      download(answer.filename, answer.media_type, answer.xml);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={t("einvoice.export")}
      description={command?.description}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {done ? t("action.close") : t("action.cancel")}
          </Button>
          <Button busy={busy} onClick={() => void run()}>
            {done ? t("einvoice.download_again") : t("einvoice.download")}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="einvoice-syntax">{inputs.find((input) => input.name === "syntax")?.label}</Label>
            <Select id="einvoice-syntax" value={syntax} onChange={(event) => setSyntax(event.target.value)}>
              {syntaxes.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </Select>
          </div>
          <div className="space-y-1">
            <Label htmlFor="einvoice-profile">{inputs.find((input) => input.name === "profile")?.label}</Label>
            <Select id="einvoice-profile" value={profile} onChange={(event) => setProfile(event.target.value)}>
              {profiles.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </Select>
          </div>
        </div>
        {error ? <Alert title={t("einvoice.export_refused")}>{error.message}</Alert> : null}
        {done ? (
          <Alert
            tone={done.valid ? "info" : "warning"}
            title={
              done.valid
                ? t("einvoice.exported", { file: done.filename })
                : t("einvoice.exported_invalid", { file: done.filename })
            }
          >
            {done.findings.length > 0 ? (
              <ul className="list-disc ps-4">
                {done.findings.map((finding) => (
                  <li key={`${finding.rule}|${finding.message}`}>
                    <code className="font-mono text-xs">{finding.rule}</code> {finding.message}
                  </li>
                ))}
              </ul>
            ) : (
              <p>{done.specification}</p>
            )}
          </Alert>
        ) : null}
      </div>
    </Dialog>
  );
}

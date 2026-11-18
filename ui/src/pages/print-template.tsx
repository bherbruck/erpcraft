// The record screen of a print template: the generic entity form, with the
// document type chosen from the registry, the body edited as source, a way to
// start from the template built into the server, and the template rendered
// against a document of the type beside the form.

import { FileCode } from "lucide-react";
import { useEffect, useState } from "react";
import { ApiError, api } from "@/api/client";
import type { FieldMeta, Json } from "@/api/types";
import { useApp } from "@/app/context";
import { PrintFrame } from "@/components/print-frame";
import { DocumentLinkInput } from "@/components/references";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { canRun, findDocType } from "@/lib/meta";
import { useAsync } from "@/lib/use-async";
import type { FormCustomization } from "./entity-record";

const text = (value: Json | undefined): string => (typeof value === "string" ? value : "");

function DocTypeSelect({
  id,
  value,
  invalid,
  describedBy,
  onChange,
}: {
  id: string;
  value: string;
  invalid: boolean;
  describedBy: string | undefined;
  onChange: (value: string | null) => void;
}) {
  const { meta } = useApp();
  return (
    <Select
      id={id}
      value={value}
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      aria-required="true"
      onChange={(event) => onChange(event.target.value || null)}
    >
      {value === "" ? <option value="">{t("field.choose")}</option> : null}
      {meta.doc_types.map((docType) => (
        <option key={docType.name} value={docType.name}>
          {docType.label}
        </option>
      ))}
    </Select>
  );
}

function BodyInput({
  id,
  value,
  docType,
  invalid,
  describedBy,
  onChange,
}: {
  id: string;
  value: string;
  docType: string;
  invalid: boolean;
  describedBy: string | undefined;
  onChange: (value: string | null) => void;
}) {
  const { meta } = useApp();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const allowed = canRun(meta, "print_template.builtin");

  async function startFromBuiltin(): Promise<void> {
    setLoading(true);
    setError(null);
    try {
      const builtin = await api.run("print_template.builtin", { doc_type: docType });
      onChange(builtin.body);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause.message : t("error.unexpected"));
    }
    setLoading(false);
  }

  return (
    <div className="space-y-2">
      {allowed ? (
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            busy={loading}
            disabled={docType === "" || (value !== "" && loading)}
            title={value !== "" ? t("print_template.replaces") : undefined}
            onClick={() => void startFromBuiltin()}
          >
            <FileCode aria-hidden="true" />
            {t("print_template.start_from_builtin")}
          </Button>
          <span className="text-xs text-muted-foreground">
            {docType === ""
              ? t("print_template.choose_type_first")
              : value !== ""
                ? t("print_template.replaces")
                : null}
          </span>
        </div>
      ) : null}
      {error ? <Alert title={t("print_template.builtin_failed")}>{error}</Alert> : null}
      <Textarea
        id={id}
        value={value}
        rows={24}
        spellCheck={false}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        className="font-mono text-xs"
        onChange={(event) => onChange(event.target.value === "" ? null : event.target.value)}
      />
    </div>
  );
}

/** The template as it is being written, rendered against a document the user picks. Nothing is saved. */
function Preview({ body, docType }: { body: string; docType: string }) {
  const { meta } = useApp();
  const type = findDocType(meta, docType);
  const [documentId, setDocumentId] = useState<string | null>(null);
  // Rendered again once typing pauses, not on every key.
  const [settled, setSettled] = useState(body);
  useEffect(() => {
    const timer = setTimeout(() => setSettled(body), 600);
    return () => clearTimeout(timer);
  }, [body]);
  const live = canRun(meta, "print_template.preview");
  const page = useAsync(live && documentId && settled ? `${docType}|${documentId}|${settled}` : null, (signal) =>
    api.run("print_template.preview", { doc_type: docType, id: documentId ?? "", body: settled }, { signal }),
  );
  if (!type) return null;
  const linkType = { kind: "document_link" as const, doc_type: type.name, states: [] };
  const pick: FieldMeta = {
    name: "preview_document",
    label: t("print_template.preview_document", { label: type.label }),
    type: linkType,
    decimal: null,
    default: null,
    help: null,
    immutable: false,
    readonly: false,
    required: false,
  };
  return (
    <section className="space-y-3 rounded-md border bg-card p-4">
      <h2 className="text-sm font-semibold">{t("print_template.preview")}</h2>
      <div className="space-y-1">
        <Label htmlFor="preview-document">{pick.label}</Label>
        <DocumentLinkInput
          id="preview-document"
          field={pick}
          type={linkType}
          value={documentId}
          onChange={setDocumentId}
        />
      </div>
      {page.error ? (
        <Alert title={t("print.failed")}>{page.error.message}</Alert>
      ) : page.data ? (
        <PrintFrame html={page.data.html} title={page.data.title} className="h-print" />
      ) : documentId ? (
        <Skeleton className="h-print w-full" />
      ) : (
        <p className="text-sm text-muted-foreground">{t("print_template.preview_pick")}</p>
      )}
    </section>
  );
}

export const printTemplateForm: FormCustomization = {
  wide: ["body"],
  field: ({ field, id, value, values, invalid, describedBy, onChange }) => {
    if (field.name === "doc_type") {
      return (
        <DocTypeSelect id={id} value={text(value)} invalid={invalid} describedBy={describedBy} onChange={onChange} />
      );
    }
    if (field.name === "body") {
      return (
        <BodyInput
          id={id}
          value={text(value)}
          docType={text(values.doc_type)}
          invalid={invalid}
          describedBy={describedBy}
          onChange={onChange}
        />
      );
    }
    return undefined;
  },
  aside: (values) =>
    text(values.doc_type) && text(values.body) ? (
      <Preview body={text(values.body)} docType={text(values.doc_type)} />
    ) : (
      <p className="rounded-md border bg-card p-4 text-sm text-muted-foreground">
        {t("print_template.preview_needs_body")}
      </p>
    ),
};

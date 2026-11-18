// Sending a document by email (`document.send`), the messages it was sent with
// (`document.messages`), and its PDF (`document.render_pdf`).

import { FileDown, Mail } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { ApiError, api } from "@/api/client";
import type { CommandOutputs, DocumentSendInput } from "@/api/generated";
import type { Json, JsonObject } from "@/api/types";
import { useApp } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge, type BadgeTone } from "@/components/badge";
import { Button } from "@/components/button";
import { type Column, DataTable } from "@/components/data-table";
import { Dialog } from "@/components/dialog";
import { FormField } from "@/components/form-field";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { type MessageKey, t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { canRun, findCommand } from "@/lib/meta";
import { useAsync } from "@/lib/use-async";

type Message = CommandOutputs["document.messages"]["items"][number];

/** Saves bytes the server sent in base64 as a file the browser downloads. */
function downloadBase64(name: string, mediaType: string, base64: string): void {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  const url = URL.createObjectURL(new Blob([bytes], { type: mediaType }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Downloads the document as the PDF the server renders. */
export function PdfButton({ docType, id }: { docType: string; id: string }) {
  const { meta } = useApp();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!canRun(meta, "document.render_pdf")) return null;
  return (
    <>
      {/* A failure stays beside the button that caused it, in the server's words. */}
      {error ? (
        <span role="alert" className="max-w-64 text-sm text-destructive">
          {t("pdf.failed_because", { reason: error })}
        </span>
      ) : null}
      <Button
        variant="outline"
        busy={busy}
        onClick={() => {
          setBusy(true);
          setError(null);
          api
            .run("document.render_pdf", { doc_type: docType, id })
            .then((pdf) => downloadBase64(pdf.filename, pdf.content_type, pdf.data))
            .catch((cause: unknown) => setError(cause instanceof Error ? cause.message : String(cause)))
            .finally(() => setBusy(false));
        }}
      >
        <FileDown aria-hidden="true" />
        {t("pdf.download")}
      </Button>
    </>
  );
}

/** The preview's result, which the registry types as the command's output. */
function asMessage(value: Json): Message {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    throw new ApiError(0, "error", t("error.unexpected"));
  return value as Message;
}

/** Addresses typed with commas, semicolons or spaces between them. */
function addresses(text: string): string[] {
  return text
    .split(/[\s,;]+/)
    .map((part) => part.trim())
    .filter((part) => part !== "");
}

/** "Send by email": what to send, then the server's preview of the message, then send. */
export function SendButton({
  docType,
  id,
  onSent,
}: {
  docType: string;
  id: string;
  /** After a message has gone out (or been queued), such as to show the Sent tab. */
  onSent: () => void;
}) {
  const { meta } = useApp();
  const [open, setOpen] = useState(false);
  const command = findCommand(meta, "document.send");
  if (!command || !canRun(meta, "document.send")) return null;
  return (
    <>
      <Button variant="outline" onClick={() => setOpen(true)}>
        <Mail aria-hidden="true" />
        {command.label}
      </Button>
      {open ? (
        <SendDialog
          docType={docType}
          id={id}
          title={command.label}
          onClose={() => setOpen(false)}
          onSent={() => {
            setOpen(false);
            onSent();
          }}
        />
      ) : null}
    </>
  );
}

function SendDialog({
  docType,
  id,
  title,
  onClose,
  onSent,
}: {
  docType: string;
  id: string;
  title: string;
  onClose: () => void;
  onSent: () => void;
}) {
  const [to, setTo] = useState("");
  const [cc, setCc] = useState("");
  const [subject, setSubject] = useState("");
  const [message, setMessage] = useState("");
  const [preview, setPreview] = useState<Message | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [key] = useState(() => crypto.randomUUID());
  const dirty = to !== "" || cc !== "" || subject !== "" || message !== "";

  const input = {
    doc_type: docType,
    id,
    to: to.trim() ? addresses(to) : null,
    cc: addresses(cc),
    subject: subject.trim() || null,
    message: message.trim() || null,
  } satisfies DocumentSendInput & JsonObject;

  async function step(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      if (preview === null) {
        // Nothing is sent yet: the server says what the message will be.
        setPreview(asMessage(await api.preview("document.send", input)));
      } else {
        const sent = await api.run("document.send", input, { idempotencyKey: key });
        toast.success(t(`mail.status.${sent.status}` as MessageKey), { description: sent.to_addresses.join(", ") });
        onSent();
        return;
      }
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  const fieldError = (field: string): string | null => (error?.field === field ? error.message : null);
  const placed = error !== null && ["to", "cc", "subject", "message"].includes(error.field ?? "");

  return (
    <Dialog
      open
      wide
      onClose={onClose}
      dirty={dirty || busy}
      title={title}
      description={preview ? t("mail.preview_hint") : t("mail.compose_hint")}
      footer={
        <>
          <Button variant="outline" onClick={preview ? () => setPreview(null) : onClose} disabled={busy}>
            {preview ? t("mail.back") : t("action.cancel")}
          </Button>
          <Button busy={busy} onClick={() => void step()}>
            {preview ? t("mail.send") : t("mail.preview")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        {error && !placed ? <Alert title={t("mail.failed")}>{error.message}</Alert> : null}
        {preview ? (
          <dl className="grid gap-x-4 gap-y-2 text-sm sm:grid-cols-[auto_1fr]">
            <dt className="text-muted-foreground">{t("mail.to")}</dt>
            <dd>{preview.to_addresses.join(", ")}</dd>
            {preview.cc_addresses.length > 0 ? (
              <>
                <dt className="text-muted-foreground">{t("mail.cc")}</dt>
                <dd>{preview.cc_addresses.join(", ")}</dd>
              </>
            ) : null}
            <dt className="text-muted-foreground">{t("mail.subject")}</dt>
            <dd className="font-medium">{preview.subject}</dd>
            <dt className="text-muted-foreground">{t("mail.attachment")}</dt>
            <dd>{preview.attachment_name}</dd>
            <dd className="mt-2 whitespace-pre-line rounded-md border bg-muted/40 p-3 sm:col-span-2">{preview.body}</dd>
          </dl>
        ) : (
          <div className="grid gap-4 sm:grid-cols-2">
            <FormField id="mail-to" label={t("mail.to")} error={fieldError("to")} hint={t("mail.to_hint")}>
              <Input
                id="mail-to"
                type="text"
                inputMode="email"
                autoComplete="email"
                value={to}
                aria-invalid={fieldError("to") ? true : undefined}
                onChange={(event) => setTo(event.target.value)}
              />
            </FormField>
            <FormField id="mail-cc" label={t("mail.cc")} error={fieldError("cc")}>
              <Input
                id="mail-cc"
                type="text"
                inputMode="email"
                value={cc}
                aria-invalid={fieldError("cc") ? true : undefined}
                onChange={(event) => setCc(event.target.value)}
              />
            </FormField>
            <FormField
              id="mail-subject"
              label={t("mail.subject")}
              className="sm:col-span-2"
              error={fieldError("subject")}
              hint={t("mail.subject_hint")}
            >
              <Input id="mail-subject" value={subject} onChange={(event) => setSubject(event.target.value)} />
            </FormField>
            <FormField
              id="mail-message"
              label={t("mail.message")}
              className="sm:col-span-2"
              error={fieldError("message")}
              hint={t("mail.message_hint")}
            >
              <Textarea
                id="mail-message"
                rows={6}
                value={message}
                onChange={(event) => setMessage(event.target.value)}
              />
            </FormField>
          </div>
        )}
      </div>
    </Dialog>
  );
}

const STATUS_TONES: Record<string, BadgeTone> = { queued: "info", sent: "success", failed: "destructive" };

/** The messages a document was sent with, the latest first. */
export function SentMessages({ docType, id, version }: { docType: string; id: string; version: string }) {
  const messages = useAsync(`messages|${docType}|${id}|${version}`, (signal) =>
    api.run("document.messages", { doc_type: docType, id }, { signal }),
  );
  const columns: Column<Message>[] = [
    {
      id: "when",
      header: t("mail.when"),
      cell: (m) => <span className="whitespace-nowrap">{formatTimestamp(m.sent_at ?? m.created_at)}</span>,
    },
    {
      id: "status",
      header: t("mail.status"),
      cell: (m) => (
        <Badge tone={STATUS_TONES[m.status] ?? "neutral"} title={m.last_error ?? undefined}>
          {t(`mail.status.${m.status}` as MessageKey)}
        </Badge>
      ),
    },
    { id: "to", header: t("mail.to"), cell: (m) => [...m.to_addresses, ...m.cc_addresses].join(", ") },
    {
      id: "subject",
      header: t("mail.subject"),
      cell: (m) => (
        <span className="block whitespace-normal">
          {m.subject}
          {m.last_error ? <span className="block text-destructive">{m.last_error}</span> : null}
        </span>
      ),
    },
    { id: "by", header: t("mail.by"), cell: (m) => m.sent_by_name ?? m.sent_by },
  ];
  return (
    <DataTable
      caption={t("mail.sent_title")}
      columns={columns}
      rows={messages.data?.items}
      rowKey={(m) => m.id}
      loading={messages.loading}
      error={messages.error}
      onRetry={messages.reload}
      empty={<p>{t("mail.none")}</p>}
    />
  );
}

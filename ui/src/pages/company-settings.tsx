// What a company prints and sends with: its logo (`company_logo.*`) and the mail
// server its documents are sent through (`smtp_settings.*`). Shown under the
// company's record; the mail settings are edited on a page of their own.

import { ImageUp, Pencil, Trash2 } from "lucide-react";
import { type ChangeEvent, type FormEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { ApiError, api } from "@/api/client";
import type { CommandOutputs, SmtpSettingsSetInput } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge } from "@/components/badge";
import { Button } from "@/components/button";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FormField } from "@/components/form-field";
import { PageHeader, Section } from "@/components/page";
import { Card, CardContent } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { Skeleton } from "@/components/ui/skeleton";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { type MessageKey, t } from "@/i18n/t";
import { COMPANY, canEntity, canRun, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";

type Settings = NonNullable<CommandOutputs["smtp_settings.get"]>;

const SECURITY = ["starttls", "tls", "none"] as const;

/** The file's content in base64, as the server takes an image. */
function readBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const url = typeof reader.result === "string" ? reader.result : "";
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.onerror = () => reject(reader.error ?? new Error(t("error.unexpected")));
    reader.readAsDataURL(file);
  });
}

/** The company's logo: shown as it prints, replaced by choosing a file, removed after asking. */
export function LogoPanel({ companyId, companyName }: { companyId: string; companyName: string }) {
  const { meta } = useApp();
  const logo = useAsync(`logo|${companyId}`, (signal) =>
    api.run("company_logo.get", { company_id: companyId }, { signal }),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  if (!canRun(meta, "company_logo.get")) return null;
  const canSet = canRun(meta, "company_logo.set");
  const canRemove = canRun(meta, "company_logo.remove");
  const current = logo.data?.data && logo.data.content_type ? logo.data : null;

  async function upload(event: ChangeEvent<HTMLInputElement>): Promise<void> {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    setBusy(true);
    setError(null);
    try {
      await api.run("company_logo.set", { company_id: companyId, data: await readBase64(file) });
      toast.success(t("logo.saved"));
      logo.reload();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  async function remove(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await api.run("company_logo.remove", { company_id: companyId });
      toast.success(t("logo.removed"));
      setConfirmRemove(false);
      logo.reload();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  return (
    <Section title={t("logo.title")}>
      <Card className="py-4">
        <CardContent className="space-y-3 px-4">
          <p className="text-sm text-muted-foreground">{t("logo.hint")}</p>
          {error ? <Alert title={t("logo.failed")}>{error.message}</Alert> : null}
          {logo.error ? <Alert>{logo.error.message}</Alert> : null}
          {logo.data === undefined && !logo.error ? (
            <Skeleton className="h-20 w-48" />
          ) : current ? (
            <div className="w-fit rounded-md border bg-paper p-3">
              <img
                // The server sends the image itself; the page's policy allows images from data: URLs.
                src={`data:${current.content_type};base64,${current.data}`}
                alt={t("logo.alt", { company: companyName })}
                className="max-h-20 max-w-64 object-contain"
              />
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">{t("logo.none")}</p>
          )}
          {canSet || (canRemove && current) ? (
            <div className="flex flex-wrap items-center gap-2">
              {canSet ? (
                <Button variant="outline" size="sm" busy={busy} onClick={() => fileRef.current?.click()}>
                  <ImageUp aria-hidden="true" />
                  {current ? t("logo.replace") : t("logo.choose")}
                </Button>
              ) : null}
              <input
                ref={fileRef}
                id="logo-file"
                type="file"
                accept="image/png,image/jpeg"
                // Chosen through the button beside it; the input itself is not shown.
                hidden
                tabIndex={-1}
                aria-label={t("logo.choose")}
                onChange={(event) => void upload(event)}
              />
              {canRemove && current ? (
                <Button variant="destructive" size="sm" onClick={() => setConfirmRemove(true)} disabled={busy}>
                  <Trash2 aria-hidden="true" />
                  {t("logo.remove")}
                </Button>
              ) : null}
            </div>
          ) : null}
        </CardContent>
      </Card>
      <ConfirmDialog
        open={confirmRemove}
        title={t("logo.remove")}
        description={t("logo.remove_confirm")}
        confirmLabel={t("logo.remove")}
        busy={busy}
        error={error}
        onConfirm={() => void remove()}
        onClose={() => setConfirmRemove(false)}
      />
    </Section>
  );
}

function Entry({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-1 sm:grid-cols-3 sm:gap-4">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="text-sm sm:col-span-2">{children}</dd>
    </div>
  );
}

function securityLabel(security: string): string {
  return (SECURITY as readonly string[]).includes(security) ? t(`smtp.security.${security}` as MessageKey) : security;
}

/** The mail server the company's documents are sent through, with a link to change it. */
export function SmtpPanel({ companyId }: { companyId: string }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const settings = useAsync(`smtp|${companyId}`, (signal) =>
    api.run("smtp_settings.get", { company_id: companyId }, { signal }),
  );
  if (!canRun(meta, "smtp_settings.get")) return null;
  const editable = canRun(meta, "smtp_settings.set") && canEntity(meta, COMPANY, "update");
  const data = settings.data;
  return (
    <Section
      title={t("smtp.title")}
      aside={
        editable ? (
          <Button asChild variant="outline" size="sm">
            <Link to={`${paths.entityRecord(code, COMPANY, companyId)}/email`}>
              <Pencil aria-hidden="true" />
              {t("smtp.edit")}
            </Link>
          </Button>
        ) : null
      }
    >
      {settings.error ? <Alert>{settings.error.message}</Alert> : null}
      {data === undefined && !settings.error ? <Skeleton className="h-24 w-full" /> : null}
      {data === null ? (
        <p className="rounded-lg border bg-card p-4 text-sm text-muted-foreground">{t("smtp.none")}</p>
      ) : null}
      {data ? (
        <Card className="py-4">
          <CardContent className="px-4">
            <dl className="space-y-2">
              <Entry label={t("smtp.host")}>
                {`${data.host}:${data.port}`}
                <Badge className="ms-2">{securityLabel(data.security)}</Badge>
              </Entry>
              <Entry label={t("smtp.from_address")}>
                {data.from_name ? `${data.from_name} <${data.from_address}>` : data.from_address}
              </Entry>
              {data.username ? <Entry label={t("smtp.username")}>{data.username}</Entry> : null}
              <Entry label={t("smtp.password")}>
                {data.has_password ? t("smtp.password_set") : t("smtp.password_none")}
              </Entry>
              {data.reply_to ? <Entry label={t("smtp.reply_to")}>{data.reply_to}</Entry> : null}
              {data.bcc ? <Entry label={t("smtp.bcc")}>{data.bcc}</Entry> : null}
            </dl>
          </CardContent>
        </Card>
      ) : null}
    </Section>
  );
}

interface Draft {
  host: string;
  port: string;
  security: string;
  username: string;
  password: string;
  clearPassword: boolean;
  from_address: string;
  from_name: string;
  reply_to: string;
  bcc: string;
}

function draftOf(settings: Settings | null): Draft {
  return {
    host: settings?.host ?? "",
    port: settings ? String(settings.port) : "",
    security: settings?.security ?? "starttls",
    username: settings?.username ?? "",
    password: "",
    clearPassword: false,
    from_address: settings?.from_address ?? "",
    from_name: settings?.from_name ?? "",
    reply_to: settings?.reply_to ?? "",
    bcc: settings?.bcc ?? "",
  };
}

const orNull = (text: string): string | null => (text.trim() === "" ? null : text.trim());

/** `/c/:company/e/company/:id/email`: the company's mail server. The password is written, never shown. */
export function SmtpEditPage() {
  const { id = "" } = useParams();
  const { meta } = useApp();
  const { code } = useCompany();
  const entity = findEntity(meta, COMPANY);
  const settings = useAsync(`smtp|${id}`, (signal) => api.run("smtp_settings.get", { company_id: id }, { signal }));
  const owner = useAsync(`smtp-owner|${id}`, (signal) => api.run("entity.get", { entity: COMPANY, id }, { signal }));
  const loaded = useMemo(() => (settings.data === undefined ? null : draftOf(settings.data)), [settings.data]);
  const [draft, setDraft] = useState<Draft | null>(null);
  useEffect(() => setDraft(loaded), [loaded]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [key] = useState(() => crypto.randomUUID());
  const navigate = useNavigate();
  const dirty = draft !== null && loaded !== null && JSON.stringify(draft) !== JSON.stringify(loaded);
  const { blocker, allowNext } = useUnsavedGuard(dirty);
  const back = paths.entityRecord(code, COMPANY, id);

  async function save(event?: FormEvent): Promise<void> {
    event?.preventDefault();
    if (!draft || busy) return;
    setBusy(true);
    setError(null);
    const port = parseInt(draft.port, 10);
    const input: SmtpSettingsSetInput = {
      company_id: id,
      host: draft.host.trim(),
      port: draft.port.trim() === "" || Number.isNaN(port) ? null : port,
      security: draft.security,
      username: orNull(draft.username),
      from_address: draft.from_address.trim(),
      from_name: orNull(draft.from_name),
      reply_to: orNull(draft.reply_to),
      bcc: orNull(draft.bcc),
      // Left out keeps the password on record; empty removes it.
      ...(draft.clearPassword ? { password: "" } : draft.password !== "" ? { password: draft.password } : {}),
    };
    try {
      await api.run("smtp_settings.set", input, { idempotencyKey: key });
      allowNext();
      toast.success(t("toast.saved", { label: t("smtp.title") }));
      navigate(back);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setBusy(false);
    }
  }

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const set = (patch: Partial<Draft>): void => setDraft((current) => (current ? { ...current, ...patch } : current));
  const fieldError = (field: string): string | null => (error?.field === field ? error.message : null);
  const fields = ["host", "port", "security", "username", "password", "from_address", "from_name", "reply_to", "bcc"];
  const placed = error !== null && fields.includes(error.field ?? "");
  const ownerName = owner.data && typeof owner.data.name === "string" ? owner.data.name : null;
  const text = (name: keyof Draft, label: string, extra: { hint?: string; required?: boolean; type?: string } = {}) => (
    <FormField id={`smtp-${name}`} label={label} required={extra.required} error={fieldError(name)} hint={extra.hint}>
      <Input
        id={`smtp-${name}`}
        type={extra.type ?? "text"}
        autoComplete="off"
        value={String(draft?.[name] ?? "")}
        aria-invalid={fieldError(name) ? true : undefined}
        onChange={(event) => set({ [name]: event.target.value })}
      />
    </FormField>
  );

  return (
    <>
      <PageHeader
        sticky
        crumbs={[
          { label: entity?.plural ?? COMPANY, to: paths.entityList(code, COMPANY) },
          { label: ownerName ?? entity?.label ?? COMPANY, to: back },
        ]}
        title={t("smtp.title")}
        badges={dirty ? <Badge tone="warning">{t("record.unsaved")}</Badge> : null}
        actions={
          <>
            <Button asChild variant="outline">
              <Link to={back}>{t("action.cancel")}</Link>
            </Button>
            <Button type="submit" form="smtp-form" busy={busy} disabled={!dirty}>
              {t("action.save")}
            </Button>
          </>
        }
      />
      {settings.error ? <Alert>{settings.error.message}</Alert> : null}
      {draft === null && !settings.error ? <Skeleton className="h-64 w-full max-w-3xl" /> : null}
      {draft ? (
        <form id="smtp-form" noValidate onSubmit={(event) => void save(event)} className="max-w-3xl space-y-4">
          {error && !placed ? <Alert title={t("smtp.not_saved")}>{error.message}</Alert> : null}
          <Card className="py-4">
            <CardContent className="grid gap-4 px-4 sm:grid-cols-2">
              {text("host", t("smtp.host"), { required: true })}
              <FormField id="smtp-security" label={t("smtp.security")} error={fieldError("security")}>
                <NativeSelect
                  id="smtp-security"
                  className="w-full"
                  value={draft.security}
                  onChange={(event) => set({ security: event.target.value })}
                >
                  {SECURITY.map((security) => (
                    <option key={security} value={security}>
                      {securityLabel(security)}
                    </option>
                  ))}
                </NativeSelect>
              </FormField>
              {text("port", t("smtp.port"), { hint: t("smtp.port_hint") })}
              {text("username", t("smtp.username"))}
              <FormField
                id="smtp-password"
                label={t("smtp.password")}
                error={fieldError("password")}
                hint={settings.data?.has_password ? t("smtp.password_set") : t("smtp.password_none")}
              >
                <Input
                  id="smtp-password"
                  type="password"
                  autoComplete="new-password"
                  value={draft.password}
                  disabled={draft.clearPassword}
                  onChange={(event) => set({ password: event.target.value })}
                />
              </FormField>
              {settings.data?.has_password ? (
                <div className="flex items-center gap-2 self-center">
                  <Checkbox
                    id="smtp-clear-password"
                    checked={draft.clearPassword}
                    onCheckedChange={(checked) => set({ clearPassword: checked === true, password: "" })}
                  />
                  <Label htmlFor="smtp-clear-password" className="font-normal">
                    {t("smtp.password_clear")}
                  </Label>
                </div>
              ) : (
                <div />
              )}
              {text("from_address", t("smtp.from_address"), { required: true, type: "email" })}
              {text("from_name", t("smtp.from_name"), { hint: t("smtp.from_name_hint") })}
              {text("reply_to", t("smtp.reply_to"), { type: "email" })}
              {text("bcc", t("smtp.bcc"), { hint: t("smtp.bcc_hint"), type: "email" })}
            </CardContent>
          </Card>
          <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
        </form>
      ) : null}
      <UnsavedDialog blocker={blocker} />
    </>
  );
}

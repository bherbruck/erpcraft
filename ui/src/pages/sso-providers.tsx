// Single sign-on providers. They are read and written through their own
// commands, not the generic entity layer: the client secret is write-only, the
// list carries the redirect URI to register at the provider, and bindings of
// users to identities have commands of their own. The form is built from the
// commands' input schemas, with each input shown as the entity's field of the
// same name where there is one (so a role is a lookup, not an id).

import { Copy, KeyRound, Link2, Plus, Trash2 } from "lucide-react";
import { type FormEvent, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { AuthEventsOutput, EntityName, SsoIdentityListOutput, SsoProviderListOutput } from "@/api/generated";
import type { CommandMeta, EntityMeta, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge } from "@/components/badge";
import { Button } from "@/components/button";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { type Column, DataTable } from "@/components/data-table";
import { Dialog } from "@/components/dialog";
import { FieldInput, type FieldValues } from "@/components/field-input";
import { Empty } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader, Section } from "@/components/page";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { canRun, commandInputs, findCommand, type InputProperty, USER } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

export const SSO_PROVIDER: EntityName = "sso_provider";

type Provider = SsoProviderListOutput["items"][number];
type Binding = SsoIdentityListOutput["items"][number];
type SignIn = AuthEventsOutput["items"][number];

async function listProviders(signal?: AbortSignal): Promise<Provider[]> {
  const result = await api.run("sso_provider.list", {}, { signal });
  return result.items;
}

function useCommand(name: string): CommandMeta | undefined {
  const { meta } = useApp();
  const command = findCommand(meta, name);
  return command && canRun(meta, name) ? command : undefined;
}

// ------------------------------------------------------------------ the list

export function SsoProviderListPage({ entity }: { entity: EntityMeta }) {
  const { code } = useCompany();
  const list = useAsync("sso-providers", (signal) => listProviders(signal));
  const create = useCommand("sso_provider.create");
  const columns = useMemo<Column<Provider>[]>(() => {
    const label = (name: string): string => entity.fields.find((field) => field.name === name)?.label ?? name;
    return [
      {
        id: "name",
        header: label("name"),
        cell: (p) => <span className="whitespace-nowrap font-medium">{p.name}</span>,
      },
      {
        id: "slug",
        header: label("slug"),
        cell: (p) => <code className="whitespace-nowrap font-mono text-xs">{p.slug}</code>,
      },
      {
        id: "issuer",
        header: label("issuer"),
        className: "w-full",
        cell: (p) => <span className="break-all">{p.issuer}</span>,
      },
      {
        id: "secret",
        header: t("sso.secret"),
        cell: (p) =>
          p.has_secret ? (
            <Badge tone="success">{t("sso.secret_set")}</Badge>
          ) : (
            <Badge tone="warning">{t("sso.secret_missing")}</Badge>
          ),
      },
      {
        id: "active",
        header: label("active"),
        cell: (p) =>
          p.active ? <Badge tone="success">{t("value.yes")}</Badge> : <Badge tone="neutral">{t("value.no")}</Badge>,
      },
    ];
  }, [entity]);
  const newButton = create ? (
    <Button asChild>
      <Link to={paths.entityNew(code, entity.name)}>
        <Plus aria-hidden="true" />
        {t("list.new", { label: entity.label })}
      </Link>
    </Button>
  ) : null;
  return (
    <>
      <PageHeader title={entity.plural} actions={newButton} />
      <p className="mb-3 max-w-form text-sm text-muted-foreground">{t("sso.list_hint")}</p>
      <DataTable
        caption={entity.plural}
        columns={columns}
        rows={list.data}
        rowKey={(p) => p.id}
        rowHref={(p) => paths.entityRecord(code, entity.name, p.id)}
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        empty={
          <>
            <p>{t("list.empty", { label: entity.plural })}</p>
            {newButton}
          </>
        }
      />
    </>
  );
}

// ------------------------------------------------------------------ the form

/** The value a provider holds for an input, in the form an input edits. */
function heldValue(provider: Provider | undefined, name: string): Json | undefined {
  if (!provider) return undefined;
  return Object.entries(provider).find(([key]) => key === name)?.[1];
}

function same(a: Json | undefined, b: Json | undefined): boolean {
  const blank = (v: Json | undefined): boolean => v === undefined || v === null || v === "";
  if (blank(a) && blank(b)) return true;
  return JSON.stringify(a) === JSON.stringify(b);
}

function ProviderInput({
  input,
  field,
  value,
  error,
  onChange,
}: {
  input: InputProperty;
  field: FieldMeta | undefined;
  value: Json | undefined;
  error: string | undefined;
  onChange: (value: Json | undefined) => void;
}) {
  const id = `sso-${input.name}`;
  const describedBy = `${id}-help`;
  const label = field?.label ?? input.label;
  const secret = input.secret;
  return (
    <div
      className={input.name === "allowed_domains" || input.name === "issuer" ? "space-y-1 sm:col-span-2" : "space-y-1"}
    >
      <Label htmlFor={id}>
        {label}
        {input.required ? (
          <>
            <span aria-hidden="true" className="text-destructive">
              {t("field.required_mark")}
            </span>
            <span className="sr-only">{t("field.required")}</span>
          </>
        ) : null}
      </Label>
      {secret || !field ? (
        <Input
          id={id}
          type={secret ? "password" : "text"}
          autoComplete={secret ? "new-password" : "off"}
          value={typeof value === "string" ? value : ""}
          aria-invalid={error ? true : undefined}
          aria-describedby={describedBy}
          onChange={(event) => onChange(event.target.value === "" ? null : event.target.value)}
        />
      ) : (
        <FieldInput
          id={id}
          field={{ ...field, required: input.required }}
          value={value}
          invalid={Boolean(error)}
          aria-describedby={describedBy}
          onChange={onChange}
        />
      )}
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      {input.description ? (
        <p id={describedBy} className="whitespace-pre-line text-sm text-muted-foreground">
          {input.description}
        </p>
      ) : null}
    </div>
  );
}

export function SsoProviderPage({ entity }: { entity: EntityMeta }) {
  const params = useParams();
  const id = params.id ?? null;
  const create = useCommand("sso_provider.create");
  const update = useCommand("sso_provider.update");
  if ((id === null && !create) || (id !== null && !canSee(update, create))) return <NotFoundPage />;
  return <ProviderForm key={id ?? "new"} entity={entity} id={id} />;
}

function canSee(...commands: (CommandMeta | undefined)[]): boolean {
  return commands.some(Boolean);
}

function ProviderForm({ entity, id }: { entity: EntityMeta; id: string | null }) {
  const { code } = useCompany();
  const navigate = useNavigate();
  const isNew = id === null;
  const command = useCommand(isNew ? "sso_provider.create" : "sso_provider.update");
  const setSecret = useCommand("sso_provider.set_secret");
  const remove = useCommand("sso_provider.delete");
  const list = useAsync(isNew ? null : `sso-provider|${id}`, (signal) => listProviders(signal));
  const provider = list.data?.find((candidate) => candidate.id === id);
  // In the order of the entity's fields, each input not among them after the one it follows by name.
  const inputs = useMemo(() => {
    const order = (name: string): number => {
      const index = entity.fields.findIndex(
        (field) => name === field.name || name.startsWith(`${field.name.replace(/_id$/, "")}_`),
      );
      return index === -1 ? entity.fields.length : index + (entity.fields[index]?.name === name ? 0 : 0.5);
    };
    return command
      ? commandInputs(command)
          .filter((input) => input.name !== "id")
          .sort((a, b) => order(a.name) - order(b.name))
      : [];
  }, [command, entity]);
  const fieldOf = (name: string): FieldMeta | undefined => entity.fields.find((field) => field.name === name);

  const [edits, setEdits] = useState<FieldValues>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [secretOpen, setSecretOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);
  const [copied, setCopied] = useState(false);
  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });

  const valueFor = (name: string): Json | undefined => (name in edits ? edits[name] : heldValue(provider, name));
  const changed = inputs.filter(
    (input) => input.name in edits && !same(edits[input.name], heldValue(provider, input.name)),
  );
  const dirty = changed.length > 0;
  const { blocker, allowNext } = useUnsavedGuard(dirty);
  // Changing where the provider is, or which client this is, ends what it had.
  const resets = !isNew && changed.some((input) => input.name === "issuer" || input.name === "client_id");

  async function save(event?: FormEvent): Promise<void> {
    event?.preventDefault();
    if (!command || saving || (!isNew && !dirty)) return;
    setSaving(true);
    setErrors({});
    setFormError(null);
    const data: JsonObject = isNew ? {} : { id: id ?? "" };
    for (const input of isNew ? inputs : changed) {
      const value = valueFor(input.name);
      if (value === undefined || (isNew && value === null)) continue;
      data[input.name] = value;
    }
    const payload = JSON.stringify(data);
    if (keyRef.current.payload !== payload) keyRef.current = { payload, key: newKey() };
    try {
      const result = await api.runNamed(command.name, data, { idempotencyKey: keyRef.current.key });
      setEdits({});
      setSaved(true);
      if (isNew) {
        allowNext();
        const createdId = result && typeof result === "object" && !Array.isArray(result) ? result.id : null;
        if (typeof createdId === "string")
          navigate(paths.entityRecord(code, entity.name, createdId), { replace: true });
      } else {
        list.reload();
      }
    } catch (cause) {
      const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
      if (error.field && inputs.some((input) => input.name === error.field))
        setErrors({ [error.field]: error.message });
      setFormError(error.message);
    }
    setSaving(false);
  }

  async function deleteProvider(): Promise<void> {
    if (!remove || !id) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.runNamed(remove.name, { id });
      allowNext();
      navigate(paths.entityList(code, entity.name), { replace: true });
    } catch (cause) {
      setDeleteError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setDeleting(false);
    }
  }

  if (!isNew && list.data && !provider) return <NotFoundPage />;
  const ready = isNew || provider !== undefined;
  const title = isNew ? t("record.new", { label: entity.label }) : (provider?.name ?? entity.label);

  return (
    <>
      <PageHeader
        sticky
        crumbs={[{ label: entity.plural, to: paths.entityList(code, entity.name) }]}
        title={ready ? title : <Skeleton className="h-6 w-48" />}
        badges={
          provider ? (
            provider.active ? (
              <Badge tone="success">{fieldOf("active")?.label ?? t("value.yes")}</Badge>
            ) : (
              <Badge tone="neutral">{t("sso.inactive")}</Badge>
            )
          ) : null
        }
        actions={
          <>
            {saved && !dirty ? <output className="text-sm text-muted-foreground">{t("record.saved")}</output> : null}
            {dirty ? <output className="text-sm text-muted-foreground">{t("record.unsaved")}</output> : null}
            {provider && setSecret ? (
              <Button
                variant="outline"
                onClick={() => setSecretOpen(true)}
                disabled={dirty}
                title={dirty ? t("record.save_first") : undefined}
              >
                <KeyRound aria-hidden="true" />
                {setSecret.label}
              </Button>
            ) : null}
            {provider && remove ? (
              <Button variant="destructive" onClick={() => setDeleteOpen(true)}>
                <Trash2 aria-hidden="true" />
                {remove.label}
              </Button>
            ) : null}
            {command ? (
              <Button onClick={() => void save()} busy={saving} disabled={!ready || (!isNew && !dirty)}>
                {isNew ? t("record.create") : t("action.save")}
              </Button>
            ) : null}
          </>
        }
      />
      <div className="flex flex-col gap-5 xl:flex-row xl:items-start">
        <form noValidate onSubmit={(event) => void save(event)} className="w-full max-w-form shrink-0 space-y-4">
          {formError ? <Alert title={t("record.save_failed")}>{formError}</Alert> : null}
          {resets && command ? (
            <Alert tone="warning" title={t("sso.resets_title")}>
              {command.description}
            </Alert>
          ) : null}
          {provider ? (
            <section className="space-y-2 rounded-md border bg-card p-4 text-sm">
              <h2 className="font-medium">{t("sso.redirect_uri")}</h2>
              {provider.redirect_uri ? (
                <div className="flex items-center gap-2">
                  <code className="min-w-0 flex-1 break-all rounded-sm bg-muted px-2 py-1 font-mono text-xs">
                    {provider.redirect_uri}
                  </code>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      void navigator.clipboard?.writeText(provider.redirect_uri ?? "").then(() => setCopied(true));
                    }}
                  >
                    <Copy aria-hidden="true" />
                    {copied ? t("sso.copied") : t("sso.copy")}
                  </Button>
                </div>
              ) : (
                <p className="text-muted-foreground">{t("sso.no_public_url")}</p>
              )}
              <p className="text-sm text-muted-foreground">{t("sso.redirect_hint")}</p>
              <p className="flex items-center gap-2">
                {provider.has_secret ? (
                  <Badge tone="success">{t("sso.secret_set")}</Badge>
                ) : (
                  <Badge tone="warning">{t("sso.secret_missing")}</Badge>
                )}
                <span className="text-sm text-muted-foreground">{t("sso.secret_write_only")}</span>
              </p>
            </section>
          ) : null}
          <div className="grid gap-x-4 gap-y-3 rounded-md border bg-card p-4 sm:grid-cols-2">
            {!ready
              ? inputs.map((input) => <Skeleton key={input.name} className="h-8 w-full" />)
              : inputs.map((input) => (
                  <ProviderInput
                    key={input.name}
                    input={input}
                    field={fieldOf(input.name)}
                    value={valueFor(input.name)}
                    error={errors[input.name]}
                    onChange={(value) => {
                      setEdits((previous) => ({ ...previous, [input.name]: value }));
                      setSaved(false);
                    }}
                  />
                ))}
          </div>
          <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
        </form>
        {provider ? (
          <aside className="min-w-0 flex-1 space-y-5">
            <Bindings provider={provider} />
          </aside>
        ) : null}
      </div>
      <UnsavedDialog blocker={blocker} />
      {secretOpen && provider && setSecret ? (
        <SecretDialog
          command={setSecret}
          providerId={provider.id}
          subject={provider.name}
          onClose={() => setSecretOpen(false)}
          onDone={() => {
            setSecretOpen(false);
            list.reload();
          }}
        />
      ) : null}
      <ConfirmDialog
        open={deleteOpen}
        title={t("delete.title", { label: provider?.name ?? entity.label })}
        description={remove?.description ?? t("delete.description")}
        confirmLabel={remove?.label ?? t("action.delete")}
        busy={deleting}
        error={deleteError}
        onConfirm={() => void deleteProvider()}
        onClose={() => {
          setDeleteOpen(false);
          setDeleteError(null);
        }}
      />
    </>
  );
}

/** Replaces the client secret. The secret goes to the server and is never shown again. */
function SecretDialog({
  command,
  providerId,
  subject,
  onClose,
  onDone,
}: {
  command: CommandMeta;
  providerId: string;
  subject: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const [secret, setSecretValue] = useState("");
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [key] = useState(newKey);
  const description = commandInputs(command).find((input) => input.name === "client_secret")?.description;
  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    event.stopPropagation();
    setRunning(true);
    setError(null);
    try {
      await api.runNamed(command.name, { id: providerId, client_secret: secret }, { idempotencyKey: key });
      onDone();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause.message : t("error.unexpected"));
      setRunning(false);
    }
  }
  return (
    <Dialog
      open
      onClose={onClose}
      dirty={secret !== "" || running}
      title={t("preview.title", { action: command.label, subject })}
      description={command.description}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={running}>
            {t("action.cancel")}
          </Button>
          <Button type="submit" form="sso-secret" busy={running} disabled={secret === ""}>
            {command.label}
          </Button>
        </>
      }
    >
      <form id="sso-secret" noValidate onSubmit={(event) => void submit(event)} className="space-y-1">
        {error ? <Alert title={t("record.save_failed")}>{error}</Alert> : null}
        <Label htmlFor="sso-new-secret">{t("sso.new_secret")}</Label>
        <Input
          id="sso-new-secret"
          type="password"
          autoComplete="new-password"
          value={secret}
          onChange={(event) => setSecretValue(event.target.value)}
        />
        {description ? <p className="whitespace-pre-line text-sm text-muted-foreground">{description}</p> : null}
      </form>
    </Dialog>
  );
}

// ------------------------------------------------------------------ bindings

/**
 * Who signs in through this provider, and as whom. A user is bound to one
 * identity; an administrator can bind one from a sign-in the provider made,
 * such as one that was refused because the user was bound elsewhere.
 */
function Bindings({ provider }: { provider: Provider }) {
  const list = useCommand("sso_identity.list");
  const bind = useCommand("sso_identity.set");
  const unbind = useCommand("sso_identity.delete");
  const events = useCommand("auth.events");
  const bindings = useAsync(list ? `bindings|${provider.id}` : null, (signal) =>
    api.run("sso_identity.list", { provider_id: provider.id }, { signal }),
  );
  const signIns = useAsync(events && bind ? `sign-ins|${provider.slug}` : null, (signal) =>
    api.run("auth.events", { provider: provider.slug, limit: 20 }, { signal }),
  );
  const [removing, setRemoving] = useState<Binding | null>(null);
  const [bindUser, setBindUser] = useState<string | null>(null);
  const [bindSubject, setBindSubject] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);

  async function run(work: () => Promise<unknown>, slot: string): Promise<void> {
    setBusy(slot);
    setError(null);
    try {
      await work();
      bindings.reload();
      signIns.reload();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(null);
  }

  const bound = new Set((bindings.data?.items ?? []).map((binding) => `${binding.user_id}|${binding.subject}`));
  const bindable = (signIns.data?.items ?? []).filter(
    (event: SignIn) => event.user_id && event.subject && !bound.has(`${event.user_id}|${event.subject}`),
  );

  const bindingColumns: Column<Binding>[] = [
    { id: "email", header: t("sso.user"), cell: (b) => b.email },
    {
      id: "subject",
      header: t("sso.subject"),
      className: "w-full",
      cell: (b) => <code className="break-all font-mono text-xs">{b.subject}</code>,
    },
    {
      id: "since",
      header: t("sso.since"),
      cell: (b) => <span className="whitespace-nowrap">{formatTimestamp(b.created_at)}</span>,
    },
    ...(unbind
      ? [
          {
            id: "remove",
            header: t("sso.actions"),
            cell: (b: Binding) => (
              <Button variant="ghost" size="sm" onClick={() => setRemoving(b)}>
                {unbind.label}
              </Button>
            ),
          },
        ]
      : []),
  ];

  return (
    <>
      {list ? (
        <Section title={t("sso.bindings")}>
          {error ? <Alert title={t("sso.binding_failed")}>{error.message}</Alert> : null}
          <DataTable
            caption={t("sso.bindings")}
            columns={bindingColumns}
            rows={bindings.data?.items}
            rowKey={(b) => `${b.user_id}|${b.subject}`}
            loading={bindings.loading}
            error={bindings.error}
            onRetry={bindings.reload}
            empty={<p>{t("sso.no_bindings")}</p>}
          />
        </Section>
      ) : null}
      {bind ? (
        <Section title={t("sso.bind_user")}>
          <div className="flex flex-wrap items-end gap-3 rounded-md border bg-card p-3">
            <div className="space-y-1">
              <Label htmlFor="bind-user">{t("sso.user")}</Label>
              <Lookup
                id="bind-user"
                className="w-64"
                entity={USER}
                keyField="id"
                value={bindUser}
                allowCreate={false}
                onChange={setBindUser}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="bind-subject">{t("sso.bind_subject")}</Label>
              <Input
                id="bind-subject"
                className="w-64"
                value={bindSubject}
                onChange={(event) => setBindSubject(event.target.value)}
              />
            </div>
            <Button
              busy={busy === "bind"}
              disabled={!bindUser || bindSubject.trim() === "" || busy !== null}
              onClick={() =>
                void run(
                  () =>
                    api.run("sso_identity.set", {
                      user_id: bindUser ?? "",
                      provider_id: provider.id,
                      subject: bindSubject.trim(),
                    }),
                  "bind",
                ).then(() => {
                  setBindUser(null);
                  setBindSubject("");
                })
              }
            >
              <Link2 aria-hidden="true" />
              {bind.label}
            </Button>
          </div>
        </Section>
      ) : null}
      {bind && events ? (
        <Section title={t("sso.recent_sign_ins")}>
          <p className="text-sm text-muted-foreground">{bind.description}</p>
          <DataTable
            caption={t("sso.recent_sign_ins")}
            columns={[
              {
                id: "at",
                header: t("sso.when"),
                cell: (e: SignIn) => <span className="whitespace-nowrap">{formatTimestamp(e.at)}</span>,
              },
              { id: "email", header: t("sso.user"), cell: (e: SignIn) => e.email ?? <Empty /> },
              {
                id: "outcome",
                header: t("history.outcome"),
                className: "w-full",
                cell: (e: SignIn) =>
                  e.outcome === "success" ? (
                    <Badge tone="success">{t("sso.signed_in")}</Badge>
                  ) : (
                    <span className="flex flex-wrap items-center gap-2">
                      <Badge tone="destructive">{t("sso.refused")}</Badge>
                      <span className="text-sm text-muted-foreground">{e.reason}</span>
                    </span>
                  ),
              },
              {
                id: "bind",
                header: t("sso.actions"),
                cell: (e: SignIn) => (
                  <Button
                    variant="outline"
                    size="sm"
                    busy={busy === e.id}
                    disabled={busy !== null}
                    onClick={() =>
                      void run(
                        () =>
                          api.run("sso_identity.set", {
                            user_id: e.user_id ?? "",
                            provider_id: provider.id,
                            subject: e.subject ?? "",
                          }),
                        e.id,
                      )
                    }
                  >
                    <Link2 aria-hidden="true" />
                    {bind.label}
                  </Button>
                ),
              },
            ]}
            rows={signIns.data ? bindable : undefined}
            rowKey={(e) => e.id}
            loading={signIns.loading}
            error={signIns.error}
            onRetry={signIns.reload}
            empty={<p>{t("sso.no_sign_ins")}</p>}
          />
        </Section>
      ) : null}
      <ConfirmDialog
        open={removing !== null}
        title={t("sso.unbind_title", { email: removing?.email ?? "" })}
        description={unbind?.description ?? ""}
        confirmLabel={unbind?.label ?? t("action.delete")}
        busy={busy === "unbind"}
        error={null}
        onConfirm={() => {
          const target = removing;
          setRemoving(null);
          if (target) void run(() => api.run("sso_identity.delete", { user_id: target.user_id }), "unbind");
        }}
        onClose={() => setRemoving(null)}
      />
    </>
  );
}

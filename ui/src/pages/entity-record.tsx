import { ChevronDown, FileText, Plus, Trash2 } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { toast } from "sonner";
import { ApiError, api } from "@/api/client";
import type { EntityName } from "@/api/generated";
import type { DocTypeMeta, EntityMeta, EntityRecord, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { ActionButtons, ActionDialog } from "@/components/command-action";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FieldInput, type FieldValues, initialValues, toEditable } from "@/components/field-input";
import { FieldValue, Withheld } from "@/components/field-value";
import { FormField } from "@/components/form-field";
import { History } from "@/components/history";
import { LINE_PICKERS } from "@/components/line-pickers";
import { PageHeader, Section, Tabs } from "@/components/page";
import { RecordTree, treesLinkingTo } from "@/components/record-tree";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { t } from "@/i18n/t";
import { sameDecimal } from "@/lib/decimal";
import { forgetLinkLabels, recordLabel } from "@/lib/link-labels";
import {
  COMPANY,
  canDoc,
  canEntity,
  childEntities,
  commandInputs,
  createCommand,
  fieldReach,
  findEntity,
  isCompanyScoped,
  readableFields,
  recordActions,
  reports,
  reportTitle,
  USER,
  withoutSource,
} from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { AttachedRecords } from "./attached-records";
import { ChildRecords } from "./child-records";
import { CommandCreatePage } from "./command-create";
import { LogoPanel, SmtpPanel } from "./company-settings";
import { EDI_MESSAGE, EdiProposalPanel } from "./edi";
import { NotFoundPage } from "./not-found";
import { isParty, PartyPanel } from "./party-details";
import { policyRuleForm } from "./policy-rule";
import { printTemplateForm } from "./print-template";
import { SSO_PROVIDER, SsoProviderPage } from "./sso-providers";
import { UserRoles } from "./user-roles";

export function EntityRecordPage() {
  const { meta } = useApp();
  const params = useParams();
  const entity = findEntity(meta, params.entity);
  if (!entity || !canEntity(meta, entity.name, "read")) return <NotFoundPage />;
  // Entities written through commands of their own have a page of their own.
  if (entity.name === SSO_PROVIDER) return <SsoProviderPage entity={entity} />;
  // An entity the generic layer does not write is created through its own command.
  const creator = entity.read_only && !params.id ? createCommand(meta, entity.name) : undefined;
  if (creator) return <CommandCreatePage entity={entity} command={creator} />;
  if (!params.id && (entity.read_only || !canEntity(meta, entity.name, "create"))) return <NotFoundPage />;
  return (
    <EntityForm
      key={`${entity.name}:${params.id ?? "new"}`}
      entity={entity}
      id={params.id ?? null}
      custom={CUSTOM_FORMS[entity.name as EntityName]}
    />
  );
}

function same(field: FieldMeta, a: Json | undefined, b: Json | undefined): boolean {
  const empty = (value: Json | undefined): boolean => value === undefined || value === null || value === "";
  if (empty(a) && empty(b)) return true;
  if (field.type.kind === "decimal" && typeof a === "string" && typeof b === "string") return sameDecimal(a, b);
  return JSON.stringify(a) === JSON.stringify(b);
}

function editableValues(entity: EntityMeta, record: JsonObject): FieldValues {
  const values: FieldValues = {};
  for (const field of entity.fields) values[field.name] = toEditable(field, record[field.name]);
  return values;
}

interface Conflict {
  latest: EntityRecord;
  /** Fields the other person changed. */
  changed: FieldMeta[];
}

/**
 * What a module's own record screen changes about the generic one. The form
 * (loading, saving, conflicts, the unsaved-changes guard, history) stays the
 * same; only single fields and the space beside the form are replaced.
 */
export interface FormCustomization {
  /** A control for one field instead of the one its type would get. Return `undefined` to keep the generic one. */
  field?: (props: {
    field: FieldMeta;
    id: string;
    value: Json | undefined;
    values: FieldValues;
    invalid: boolean;
    describedBy: string | undefined;
    onChange: (value: Json | undefined) => void;
    /** Change several fields at once. */
    onChangeMany: (changes: FieldValues) => void;
  }) => ReactNode | undefined;
  /** Fields that take the full width of the form. */
  wide?: string[];
  /** Fields that say nothing for the values entered so far, and are left out of the form. */
  hidden?: (field: FieldMeta, values: FieldValues) => boolean;
  /** Shown beside the form: help that depends on what is being entered. */
  aside?: (values: FieldValues, state: { saved: boolean; dirty: boolean }) => ReactNode;
}

// Entities whose record page is more than the generic form.
const CUSTOM_FORMS: Partial<Record<EntityName, FormCustomization>> = {
  policy_rule: policyRuleForm,
  print_template: printTemplateForm,
};

/** More reports than this take the record: they are offered in one menu. */
const MAX_REPORT_BUTTONS = 3;

function EntityForm({ entity, id, custom }: { entity: EntityMeta; id: string | null; custom?: FormCustomization }) {
  const { meta, reloadCompanies } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const url = useUrlState();
  const isNew = id === null;
  const tab = url.get("tab") === "history" && !isNew ? "history" : "details";

  // A new record starts from the declared defaults, the current company, and
  // what the URL names (the parent record, for a line of it).
  const prefill = url.get(entity.parent?.field ?? "");
  const named = entity.fields
    .filter((field) => !field.readonly && url.get(field.name) !== "")
    .map((field) => [field.name, url.get(field.name)] as const);
  const namedKey = JSON.stringify(named);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `namedKey` stands for the values named in the address
  const defaults = useMemo<FieldValues>(() => {
    const values = initialValues(entity.fields);
    if (isCompanyScoped(entity) && company) values.company_id = company.id;
    for (const [name, value] of named) values[name] = value;
    return values;
  }, [entity, company, namedKey]);

  const loaded = useAsync(id ? `${entity.name}|${id}` : null, (signal) =>
    api.run("entity.get", { entity: entity.name, id: id ?? "" }, { signal }),
  );
  const [original, setOriginal] = useState<EntityRecord | null>(null);
  const [values, setValues] = useState<FieldValues>(defaults);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<ApiError | null>(null);

  // The record as read. After a create the page already holds it (the create's answer), and the
  // user may have typed since: the same version read again must not replace what they typed.
  const held = useRef<EntityRecord | null>(null);
  held.current = original;
  const typed = useRef(false);
  useEffect(() => {
    const next = loaded.data;
    if (!next) return;
    const current = held.current;
    if (typed.current && current && current.id === next.id && current._version === next._version) return;
    setOriginal(next);
    setValues(editableValues(entity, next));
    setConflict(null);
  }, [loaded.data, entity]);

  // A new record takes the current company once the companies are known.
  useEffect(() => {
    if (isNew) setValues((previous) => ({ ...defaults, ...previous }));
  }, [isNew, defaults]);

  const canWrite = !entity.read_only && canEntity(meta, entity.name, isNew ? "create" : "update");
  // Field rules: a field the user may not write for this action is read-only here,
  // and one they may not read at all is not shown.
  const isEditable = (field: FieldMeta): boolean =>
    canWrite &&
    !field.readonly &&
    (isNew || !field.immutable) &&
    fieldReach(meta, entity.name, field.name, isNew ? "create" : "update") !== "none";
  // A new record has nothing in the fields the system sets, so they are not shown until it exists.
  const shown = readableFields(meta, entity).filter(
    (field) =>
      !(field.readonly && (field.type.kind === "uuid" || isNew)) &&
      !(isNew && !field.readonly && fieldReach(meta, entity.name, field.name, "create") === "none" && canWrite),
  );

  const baseline: FieldValues = original ? editableValues(entity, original) : defaults;
  const changedFields = shown.filter(
    (field) => isEditable(field) && !same(field, values[field.name], baseline[field.name]),
  );
  const dirty = changedFields.length > 0;
  typed.current = dirty;
  const { blocker, allowNext } = useUnsavedGuard(dirty);

  // One idempotency key per request content: a double click or a retry sends the
  // same key, an edited form sends a new one.
  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });
  function keyFor(payload: string): string {
    if (keyRef.current.payload !== payload) keyRef.current = { payload, key: newKey() };
    return keyRef.current.key;
  }

  function change(field: FieldMeta, value: Json | undefined): void {
    setValues((previous) => ({ ...previous, [field.name]: value }));
    setSavedAt(null);
    setFieldErrors((previous) => {
      if (!(field.name in previous)) return previous;
      const { [field.name]: _, ...rest } = previous;
      return rest;
    });
  }

  function fail(cause: unknown): void {
    const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
    const named = error.field;
    if (named && entity.fields.some((field) => field.name === named)) {
      // Within a list held in one field (the rules of a role), the server also says which entry.
      const message =
        error.line === null ? error.message : t("record.entry_error", { entry: error.line, message: error.message });
      setFieldErrors({ [named]: message });
      setFormError(null);
    } else {
      setFormError(error.message);
    }
  }

  function afterSave(record: EntityRecord): void {
    forgetLinkLabels();
    if (entity.name === COMPANY) reloadCompanies();
    setOriginal(record);
    setValues(editableValues(entity, record));
    setSavedAt(Date.now());
    toast.success(t("toast.saved", { label: entity.label }));
  }

  async function save(): Promise<void> {
    if (!canWrite || saving || (!dirty && !isNew)) return;
    setSaving(true);
    setFormError(null);
    setFieldErrors({});
    setConflict(null);
    const data: JsonObject = {};
    for (const field of isNew ? shown.filter(isEditable) : changedFields) {
      // A field the form leaves out for these values is not sent either.
      if (custom?.hidden?.(field, values)) continue;
      const value = values[field.name];
      if (value === undefined || (isNew && value === null)) continue;
      data[field.name] = value;
    }
    // Fields the entity declares are only accepted together: when one is sent, all are (an empty one as null).
    for (const group of entity.written_together) {
      if (group.some((name) => name in data)) {
        for (const name of group) data[name] = values[name] ?? null;
      }
    }
    try {
      if (isNew) {
        const payload = { entity: entity.name, data };
        const record = await api.run("entity.create", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) });
        afterSave(record);
        allowNext();
        const nextCompany = entity.name === COMPANY && typeof record.code === "string" ? record.code : code;
        navigate(paths.entityRecord(nextCompany, entity.name, record.id), { replace: true });
      } else if (original) {
        const payload = { entity: entity.name, id: original.id, data, version: original._version ?? null };
        const record = await api.run("entity.update", payload, { idempotencyKey: keyFor(JSON.stringify(payload)) });
        afterSave(record);
      }
    } catch (cause) {
      if (cause instanceof ApiError && cause.isStale && original) {
        // Someone else may have changed the record. Look, and say what changed.
        try {
          const latest = await api.run("entity.get", { entity: entity.name, id: original.id });
          if (latest._version !== original._version) {
            const changed = shown.filter((field) => !same(field, latest[field.name], original[field.name]));
            setConflict({ latest, changed });
            setSaving(false);
            return;
          }
        } catch {
          // Fall through to the server's own words.
        }
      }
      fail(cause);
    }
    setSaving(false);
  }

  const saveRef = useRef(save);
  saveRef.current = save;
  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        // While a dialog is open the page underneath is not what the user is working on.
        if (document.querySelector("[role='dialog']")) return;
        void saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function takeTheirs(): void {
    if (!conflict) return;
    setOriginal(conflict.latest);
    setValues(editableValues(entity, conflict.latest));
    setConflict(null);
  }

  function keepMine(): void {
    if (!conflict || !original) return;
    // Fields this user did not touch follow the other person's change; the ones
    // they edited stay as typed, to be saved over the newer version on purpose.
    const theirs = editableValues(entity, conflict.latest);
    const mine = editableValues(entity, original);
    const next: FieldValues = {};
    for (const field of entity.fields) {
      const touched = !same(field, values[field.name], mine[field.name]);
      next[field.name] = touched ? values[field.name] : theirs[field.name];
    }
    setOriginal(conflict.latest);
    setValues(next);
    setConflict(null);
  }

  async function remove(): Promise<void> {
    if (!original) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await api.run("entity.delete", { entity: entity.name, id: original.id, version: original._version ?? null });
      forgetLinkLabels();
      if (entity.name === COMPANY) reloadCompanies();
      allowNext();
      const parent = entity.parent && typeof original[entity.parent.field] === "string" ? entity.parent : null;
      navigate(
        parent
          ? paths.entityRecord(code, parent.entity, String(original[parent.field]))
          : paths.entityList(code, entity.name),
        { replace: true },
      );
    } catch (cause) {
      setDeleteError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setDeleting(false);
    }
  }

  // An entity that declares a state field offers each action only from the states it applies in.
  const state = entity.state_field && original ? original[entity.state_field] : null;
  const actions = useMemo(
    () =>
      original
        ? recordActions(meta, entity.name, typeof state === "string" ? state : null, (name) => name in LINE_PICKERS)
        : [],
    [meta, entity.name, original, state],
  );
  const actionName = url.get("action");
  const openAction = original && !dirty ? actions.find((action) => action.action === actionName) : undefined;
  // A command that takes a document line asks for it first, in a picker of its own.
  const Picker = openAction ? LINE_PICKERS[openAction.command.name] : undefined;
  const [picked, setPicked] = useState<JsonObject | null>(null);
  const children = useMemo(() => childEntities(meta, entity.name), [meta, entity.name]);
  // Reports about one record of this entity: those that take its id, such as an account's ledger.
  const recordReports = useMemo(
    () =>
      reports(meta).filter(
        (report) =>
          commandInputs(report).some((input) => input.name === `${entity.name}_id` && input.required) ||
          // A report on one record of the entity, by its id: an asset's depreciation schedule.
          (report.target && report.resource === entity.name),
      ),
    [meta, entity.name],
  );

  // Documents about one record of this entity, started from it: a disposal of an asset. A document is
  // about the record its header requires a link to and its summary names (an order's vendor), not
  // every account it needs (a year-end close's retained earnings).
  const startedHere = useMemo(
    () =>
      meta.doc_types.flatMap((docType): { docType: DocTypeMeta; field: string; without: FieldMeta | null }[] => {
        if (!canDoc(meta, docType.name, "create")) return [];
        const link = docType.fields.find(
          (field) =>
            !field.readonly &&
            field.type.kind === "link" &&
            field.type.entity === entity.name &&
            field.type.entity !== COMPANY &&
            docType.summary.some((shown) => shown.name === field.name),
        );
        if (!link) return [];
        if (link.required) return [{ docType, field: link.name, without: null }];
        // Not required: the party is named only where the document has no source of its own (an
        // invoice without an order). Offered as that.
        const source = withoutSource(docType);
        return source ? [{ docType, field: link.name, without: source }] : [];
      }),
    [meta, entity.name],
  );

  const title = isNew
    ? t("record.new", { label: entity.label })
    : original
      ? recordLabel(entity.name, original)
      : entity.label;
  const parentId = entity.parent
    ? typeof values[entity.parent.field] === "string"
      ? String(values[entity.parent.field])
      : prefill
    : "";
  const parentEntity = entity.parent ? findEntity(meta, entity.parent.entity) : undefined;
  const crumbs =
    parentEntity && parentId
      ? [
          { label: parentEntity.plural, to: paths.entityList(code, parentEntity.name) },
          { label: parentEntity.label, to: paths.entityRecord(code, parentEntity.name, parentId) },
        ]
      : [{ label: entity.plural, to: paths.entityList(code, entity.name) }];

  if (!isNew && loaded.error && !original) {
    return (
      <>
        <PageHeader crumbs={crumbs} title={entity.label} />
        {loaded.error.status === 404 ? (
          <NotFoundPage />
        ) : (
          <Alert
            title={t("record.load_failed")}
            actions={
              <Button variant="outline" size="sm" onClick={loaded.reload}>
                {t("action.retry")}
              </Button>
            }
          >
            {loaded.error.message}
          </Alert>
        )}
      </>
    );
  }

  const ready = isNew || original !== null;

  return (
    <>
      <PageHeader
        sticky
        crumbs={crumbs}
        title={ready ? title : <Skeleton className="h-6 w-48" />}
        actions={
          <>
            {savedAt && !dirty ? (
              <output className="text-sm text-muted-foreground">{t("record.saved")}</output>
            ) : dirty ? (
              <output className="text-sm text-muted-foreground">{t("record.unsaved")}</output>
            ) : null}
            {original && recordReports.length > MAX_REPORT_BUTTONS ? (
              // Many reports take this record (a company): one menu instead of a wall of buttons.
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button variant="outline">
                    <FileText aria-hidden="true" />
                    {t("record.reports", { count: recordReports.length })}
                    <ChevronDown aria-hidden="true" />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  {recordReports.map((report) => (
                    <DropdownMenuItem key={report.name} asChild>
                      <Link
                        to={`${paths.report(code, report.name)}?${new URLSearchParams({ [entity.name]: original.id })}`}
                      >
                        {reportTitle(report)}
                      </Link>
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            ) : original ? (
              recordReports.map((report) => (
                <Button key={report.name} asChild variant="outline">
                  <Link
                    to={`${paths.report(code, report.name)}?${new URLSearchParams({ [entity.name]: original.id })}`}
                  >
                    {reportTitle(report)}
                  </Link>
                </Button>
              ))
            ) : null}
            {original
              ? startedHere.map(({ docType, field, without }) => (
                  <Button key={docType.name} asChild variant="outline">
                    <Link
                      to={`${paths.docNew(code, docType.name)}?${new URLSearchParams({
                        [field]: original.id,
                        ...(without ? { without: without.name } : {}),
                      })}`}
                    >
                      <Plus aria-hidden="true" />
                      {without
                        ? t("document.new_without", { label: docType.label, source: without.label })
                        : t("record.new_document", { label: docType.label })}
                    </Link>
                  </Button>
                ))
              : null}
            {!isNew ? (
              <ActionButtons
                actions={actions}
                disabledReason={dirty ? t("record.save_first") : null}
                onChoose={(action) => url.set({ action: action.action })}
              />
            ) : null}
            {!isNew && original && !entity.read_only && canEntity(meta, entity.name, "delete") ? (
              <Button variant="destructive" onClick={() => setConfirmDelete(true)}>
                <Trash2 aria-hidden="true" />
                {t("action.delete")}
              </Button>
            ) : null}
            {canWrite ? (
              <Button onClick={() => void save()} busy={saving} disabled={!ready || (!dirty && !isNew)}>
                {isNew ? t("record.create") : t("action.save")}
              </Button>
            ) : null}
          </>
        }
      />

      {!isNew ? (
        <Tabs
          current={tab}
          tabs={[
            { id: "details", label: t("record.details"), to: "?" },
            { id: "history", label: t("history.title"), to: "?tab=history" },
          ]}
        />
      ) : null}

      {/* The form stays mounted under the History tab, so nothing typed is lost by looking. */}
      <div hidden={tab !== "details"} className="space-y-8">
        <div className="flex flex-col gap-5 xl:flex-row xl:items-start">
          <form
            noValidate
            className="w-full max-w-form shrink-0 space-y-4"
            onSubmit={(event) => {
              event.preventDefault();
              void save();
            }}
          >
            {conflict ? (
              <Alert
                tone="warning"
                title={t("conflict.title")}
                actions={
                  <>
                    <Button variant="outline" size="sm" onClick={takeTheirs}>
                      {t("conflict.take_theirs")}
                    </Button>
                    <Button variant="outline" size="sm" onClick={keepMine}>
                      {t("conflict.keep_mine")}
                    </Button>
                  </>
                }
              >
                <p>{t("conflict.body")}</p>
                {conflict.changed.length > 0 ? (
                  <ul className="mt-1 list-disc space-y-0.5 ps-5">
                    {conflict.changed.map((field) => (
                      <li key={field.name}>
                        <span className="font-medium">{field.label}</span>
                        {t("conflict.was")}
                        <FieldValue field={field} value={original?.[field.name]} />
                        {t("conflict.now")}
                        <FieldValue field={field} value={conflict.latest[field.name]} />
                      </li>
                    ))}
                  </ul>
                ) : null}
              </Alert>
            ) : null}
            {formError ? <Alert title={t("record.save_failed")}>{formError}</Alert> : null}
            {!canWrite && ready && actions.length === 0 ? <Alert tone="info">{t("record.read_only")}</Alert> : null}
            {ready && entity.fields.some((field) => fieldReach(meta, entity.name, field.name, "read") === "none") ? (
              <p className="text-sm text-muted-foreground">{t("record.fields_withheld")}</p>
            ) : null}

            <div className="grid gap-x-4 gap-y-3 rounded-md border bg-card p-4 sm:grid-cols-2">
              {shown
                .filter((field) => !custom?.hidden?.(field, values))
                .map((field) => {
                  const fieldId = `field-${field.name}`;
                  const error = fieldErrors[field.name];
                  const editable = isEditable(field);
                  const wide = field.type.kind === "json" || custom?.wide?.includes(field.name) === true;
                  const hintId = `${fieldId}-hint`;
                  const describedBy = error ? `${fieldId}-error` : field.help && editable ? hintId : undefined;
                  const special = editable
                    ? custom?.field?.({
                        field,
                        id: fieldId,
                        value: values[field.name],
                        values,
                        invalid: Boolean(error),
                        describedBy,
                        onChange: (value) => change(field, value),
                        onChangeMany: (changes) => {
                          setValues((previous) => ({ ...previous, ...changes }));
                          setSavedAt(null);
                        },
                      })
                    : undefined;
                  return (
                    <FormField
                      key={field.name}
                      id={fieldId}
                      labelFor={editable ? fieldId : null}
                      className={wide ? "sm:col-span-2" : undefined}
                      label={field.label}
                      required={field.required && editable}
                      error={error}
                      hint={field.help && editable ? field.help : null}
                      hintId={hintId}
                      note={!isNew && field.immutable && !field.readonly && canWrite ? t("field.immutable") : null}
                    >
                      {!ready ? (
                        <Skeleton className="h-9 w-full" />
                      ) : special !== undefined ? (
                        special
                      ) : editable ? (
                        <FieldInput
                          id={fieldId}
                          field={field}
                          value={values[field.name]}
                          onChange={(value) => change(field, value)}
                          invalid={Boolean(error)}
                          companyId={typeof values.company_id === "string" ? values.company_id : company?.id}
                          context={{ header: values, fields: entity.fields }}
                          aria-describedby={describedBy}
                        />
                      ) : (
                        <div className="flex min-h-9 items-center text-sm">
                          {original &&
                          !(field.name in original) &&
                          fieldReach(meta, entity.name, field.name, "read") === "some" ? (
                            <Withheld />
                          ) : (
                            <FieldValue
                              field={field}
                              value={values[field.name]}
                              context={{ header: values, fields: entity.fields }}
                              linkify
                            />
                          )}
                        </div>
                      )}
                    </FormField>
                  );
                })}
            </div>
            {/* Enter in a field saves, like the button in the header. */}
            <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
          </form>
          {custom?.aside && ready ? (
            <aside className="min-w-0 flex-1">{custom.aside(values, { saved: original !== null, dirty })}</aside>
          ) : null}
        </div>
        {original && entity.name === USER ? <UserRoles userId={original.id} /> : null}
        {original && entity.name === COMPANY ? (
          <LogoPanel companyId={original.id} companyName={typeof original.name === "string" ? original.name : ""} />
        ) : null}
        {original && entity.name === COMPANY ? <SmtpPanel companyId={original.id} /> : null}
        {original && isParty(entity.name) ? <PartyPanel partyType={entity.name} id={original.id} /> : null}
        {original && entity.name === EDI_MESSAGE ? <EdiProposalPanel messageId={original.id} /> : null}
        {original ? <AttachedRecords entity={entity} id={original.id} /> : null}
        {original
          ? treesLinkingTo(meta, entity).map(({ tree, field }) => (
              <Section key={tree.name} title={t("tree.at", { label: tree.plural })}>
                <RecordTree
                  meta={meta}
                  entity={tree}
                  filters={{ [field.name]: original.id }}
                  empty={t("tree.none_here", { label: tree.plural, record: entity.label })}
                />
              </Section>
            ))
          : null}
        {original && children.length > 0
          ? children.map((child) => (
              <ChildRecords key={child.name} parent={entity} parentId={original.id} child={child} />
            ))
          : null}
      </div>

      {tab === "history" && id ? <History resource={entity.name} id={id} version={original?._version} /> : null}

      <UnsavedDialog blocker={blocker} />
      <ConfirmDialog
        open={confirmDelete}
        title={t("delete.title", { label: title })}
        description={t("delete.description")}
        confirmLabel={t("action.delete")}
        busy={deleting}
        error={deleteError}
        onConfirm={() => void remove()}
        onClose={() => {
          setConfirmDelete(false);
          setDeleteError(null);
        }}
      />
      {openAction && original && Picker && !picked ? (
        <Picker
          record={original}
          title={t("preview.title", {
            action: openAction.label,
            subject: `${entity.label} ${recordLabel(entity.name, original)}`,
          })}
          onPick={setPicked}
          onClose={() => url.set({ action: null }, { replace: true })}
        />
      ) : openAction && original ? (
        <ActionDialog
          action={openAction}
          input={{ id: original.id, ...picked }}
          subject={`${entity.label} ${recordLabel(entity.name, original)}`}
          current={original}
          fields={entity.fields}
          prefill
          onClose={() => {
            setPicked(null);
            url.set({ action: null }, { replace: true });
          }}
          onDone={() => {
            setPicked(null);
            url.set({ action: null }, { replace: true });
            forgetLinkLabels();
            loaded.reload();
          }}
        />
      ) : null}
    </>
  );
}

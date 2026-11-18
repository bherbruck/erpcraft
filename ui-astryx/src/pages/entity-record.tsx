import { AlertDialog } from "@astryxdesign/core/AlertDialog";
import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { Center } from "@astryxdesign/core/Center";
import { HStack } from "@astryxdesign/core/HStack";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { Spinner } from "@astryxdesign/core/Spinner";
import { Tab, TabList } from "@astryxdesign/core/TabList";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { EntityMeta, EntityRecord, FieldMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { ActionButtons, ActionDialog } from "@/components/actions";
import { FieldInput, type FieldValues, initialValues, toEditable } from "@/components/field-input";
import { FieldValue, Withheld } from "@/components/field-value";
import { History } from "@/components/history";
import { Page } from "@/components/page";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { t } from "@/i18n/t";
import { sameDecimal } from "@/lib/decimal";
import { forgetLinkLabels, recordLabel } from "@/lib/link-labels";
import { COMPANY, canEntity, fieldReach, findEntity, isCompanyScoped, readableFields, recordActions, type ResourceAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

export function EntityRecordPage() {
  const { meta } = useApp();
  const params = useParams();
  const entity = findEntity(meta, params.entity);
  if (!entity || !canEntity(meta, entity.name, "read")) return <NotFoundPage />;
  if (!params.id && (entity.read_only || !canEntity(meta, entity.name, "create"))) return <NotFoundPage />;
  return <EntityForm key={`${entity.name}:${params.id ?? "new"}`} entity={entity} id={params.id ?? null} />;
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
  changed: FieldMeta[];
}

/**
 * The record page of any entity, generated from the registry: one input per field,
 * chosen by the field's type; read-only fields beside them; history; the actions the
 * registry offers on the record.
 */
function EntityForm({ entity, id }: { entity: EntityMeta; id: string | null }) {
  const { meta, reloadCompanies } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const url = useUrlState();
  const isNew = id === null;
  const tab = url.get("tab") === "history" && !isNew ? "history" : "details";

  const defaults = useMemo<FieldValues>(() => {
    const values = initialValues(entity.fields);
    if (isCompanyScoped(entity) && company) values.company_id = company.id;
    return values;
  }, [entity, company]);

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
  const [action, setAction] = useState<ResourceAction | null>(null);

  useEffect(() => {
    if (!loaded.data) return;
    setOriginal(loaded.data);
    setValues(editableValues(entity, loaded.data));
    setConflict(null);
  }, [loaded.data, entity]);

  const canWrite = !entity.read_only && canEntity(meta, entity.name, isNew ? "create" : "update");
  const isEditable = (field: FieldMeta): boolean =>
    canWrite &&
    !field.readonly &&
    (isNew || !field.immutable) &&
    fieldReach(meta, entity.name, field.name, isNew ? "create" : "update") !== "none";
  const shown = readableFields(meta, entity).filter(
    (field) => !(field.readonly && (field.type.kind === "uuid" || isNew)) && !(isCompanyScoped(entity) && field.name === "company_id"),
  );
  const editable = shown.filter(isEditable);
  const readOnly = shown.filter((field) => !isEditable(field));

  const baseline: FieldValues = original ? editableValues(entity, original) : defaults;
  const changedFields = editable.filter((field) => !same(field, values[field.name], baseline[field.name]));
  const dirty = changedFields.length > 0;
  const { blocker, allowNext } = useUnsavedGuard(dirty);

  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });
  const keyFor = (payload: string): string => {
    if (keyRef.current.payload !== payload) keyRef.current = { payload, key: newKey() };
    return keyRef.current.key;
  };

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
      setFieldErrors({ [named]: error.line === null ? error.message : t("record.entry_error", { entry: error.line, message: error.message }) });
      setFormError(null);
    } else setFormError(error.message);
  }

  function afterSave(record: EntityRecord): void {
    forgetLinkLabels();
    if (entity.name === COMPANY) reloadCompanies();
    setOriginal(record);
    setValues(editableValues(entity, record));
    setSavedAt(Date.now());
  }

  async function save(): Promise<void> {
    if (!canWrite || saving || (!dirty && !isNew)) return;
    setSaving(true);
    setFormError(null);
    setFieldErrors({});
    setConflict(null);
    const data: JsonObject = {};
    if (isNew && isCompanyScoped(entity) && company) data.company_id = company.id;
    for (const field of isNew ? editable : changedFields) {
      const value = values[field.name];
      if (value === undefined || (isNew && value === null)) continue;
      data[field.name] = value;
    }
    for (const group of entity.written_together) {
      if (group.some((name) => name in data)) for (const name of group) data[name] = values[name] ?? null;
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
        try {
          const latest = await api.run("entity.get", { entity: entity.name, id: original.id });
          if (latest._version !== original._version) {
            setConflict({ latest, changed: shown.filter((field) => !same(field, latest[field.name], original[field.name])) });
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
        if (document.querySelector("dialog[open]")) return;
        void saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function keepMine(): void {
    if (!conflict || !original) return;
    // Untouched fields follow the other person's change; edited ones stay as typed.
    const theirs = editableValues(entity, conflict.latest);
    const mine = editableValues(entity, original);
    const next: FieldValues = {};
    for (const field of entity.fields) next[field.name] = same(field, values[field.name], mine[field.name]) ? theirs[field.name] : values[field.name];
    setOriginal(conflict.latest);
    setValues(next);
    setConflict(null);
  }

  async function remove(): Promise<void> {
    if (!original) return;
    setDeleting(true);
    try {
      await api.run("entity.delete", { entity: entity.name, id: original.id, version: original._version ?? null });
      forgetLinkLabels();
      if (entity.name === COMPANY) reloadCompanies();
      allowNext();
      navigate(paths.entityList(code, entity.name), { replace: true });
    } catch (cause) {
      setConfirmDelete(false);
      setDeleting(false);
      fail(cause);
    }
  }

  const state = entity.state_field && original ? original[entity.state_field] : null;
  const actions = useMemo(
    () => (original ? recordActions(meta, entity.name, typeof state === "string" ? state : null) : []),
    [meta, entity.name, original, state],
  );

  if (loaded.error && !original) {
    return (
      <VStack padding={4}>
        <Banner status="error" title={t("record.load_failed")} description={loaded.error.message} />
      </VStack>
    );
  }
  if (!isNew && !original) {
    return (
      <Center height="100%">
        <Spinner label={t("value.loading")} />
      </Center>
    );
  }

  const title = isNew ? t("record.new", { label: entity.label }) : recordLabel(entity.name, original ?? {});
  const canDelete = !isNew && !entity.read_only && canEntity(meta, entity.name, "delete");

  return (
    <Page
      back
      kicker={entity.label}
      title={title}
      badge={dirty ? <Badge variant="warning" label={t("record.unsaved")} /> : savedAt ? <Badge variant="success" label={t("record.saved")} /> : null}
      actions={
        <>
          <ActionButtons actions={actions} disabledReason={dirty ? t("record.save_first") : null} onChoose={setAction} />
          {canDelete ? <Button label={t("action.delete")} variant="ghost" onClick={() => setConfirmDelete(true)} /> : null}
          {canWrite ? (
            <Button
              label={isNew ? t("record.create") : t("action.save")}
              variant="primary"
              isLoading={saving}
              isDisabled={!isNew && !dirty}
              tooltip={t("record.save_hint")}
              onClick={() => void save()}
            />
          ) : null}
        </>
      }
      toolbar={
        isNew ? null : (
          <TabList value={tab} onChange={(next) => url.set({ tab: next === "details" ? null : next }, { replace: true })} size="sm">
            <Tab value="details" label={t("record.details")} />
            <Tab value="history" label={t("history.title")} />
          </TabList>
        )
      }
    >
      {tab === "history" && original ? (
        <History resource={entity.name} id={original.id} version={original._version} />
      ) : (
        <VStack gap={4} maxWidth={1100}>
          {!canWrite && !isNew ? <Banner status="info" title={t("record.read_only")} /> : null}
          {formError ? <Banner status="error" title={t("record.save_failed")} description={formError} /> : null}
          {conflict ? (
            <Banner
              status="warning"
              title={t("conflict.title")}
              description={conflict.changed.map((field) => field.label).join(", ")}
              endContent={
                <HStack gap={2}>
                  <Button
                    label={t("conflict.take_theirs")}
                    size="sm"
                    onClick={() => {
                      setOriginal(conflict.latest);
                      setValues(editableValues(entity, conflict.latest));
                      setConflict(null);
                    }}
                  />
                  <Button label={t("conflict.keep_mine")} size="sm" onClick={keepMine} />
                </HStack>
              }
            />
          ) : null}
          {editable.length > 0 ? (
            <Card padding={4}>
              <form
                noValidate
                onSubmit={(event) => {
                  event.preventDefault();
                  void save();
                }}
              >
                <div className="form-grid">
                  {editable.map((field) => (
                    <div key={field.name} className={field.type.kind === "json" ? "span-all" : undefined}>
                      <FieldInput
                        field={field}
                        value={values[field.name]}
                        error={fieldErrors[field.name]}
                        companyId={company?.id}
                        context={{ header: values, fields: entity.fields }}
                        onChange={(value) => change(field, value)}
                      />
                    </div>
                  ))}
                </div>
              </form>
            </Card>
          ) : null}
          {readOnly.length > 0 && original ? (
            <MetadataList columns="multi">
              {readOnly.map((field) => (
                <MetadataListItem key={field.name} label={field.label}>
                  {!(field.name in original) && fieldReach(meta, entity.name, field.name, "read") === "some" ? (
                    <Withheld />
                  ) : (
                    <FieldValue field={field} value={original[field.name]} linkify context={{ header: original, fields: entity.fields }} />
                  )}
                </MetadataListItem>
              ))}
            </MetadataList>
          ) : null}
          {!isNew ? <Text type="supporting">{t("record.save_hint")}</Text> : null}
        </VStack>
      )}
      <UnsavedDialog blocker={blocker} />
      <AlertDialog
        isOpen={confirmDelete}
        onOpenChange={(open) => setConfirmDelete(open)}
        title={t("delete.title", { label: title })}
        description={t("delete.description")}
        actionLabel={t("action.delete")}
        isActionLoading={deleting}
        onAction={() => void remove()}
      />
      {action && original ? (
        <ActionDialog
          action={action}
          id={original.id}
          subject={title}
          currency={company?.base_currency ?? ""}
          onClose={() => setAction(null)}
          onDone={() => {
            setAction(null);
            loaded.reload();
          }}
        />
      ) : null}
    </Page>
  );
}

export default EntityRecordPage;

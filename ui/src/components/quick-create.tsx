import { type FormEvent, useRef, useState } from "react";
import { ApiError, api } from "@/api/client";
import type { EntityMeta, EntityRecord, Json, JsonObject } from "@/api/types";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Dialog } from "@/components/dialog";
import { Label } from "@/components/ui/label";
import { t } from "@/i18n/t";
import { forgetLinkLabels } from "@/lib/link-labels";
import { isCompanyScoped } from "@/lib/meta";
import { newKey } from "@/lib/text";
import { FieldInput, type FieldValues } from "./field-input";

/**
 * Creates a record from inside a lookup, in a dialog, so the form underneath
 * keeps everything typed into it. One of the two uses of a modal the
 * guidelines allow.
 */
export function QuickCreateDialog({
  entity,
  companyId,
  suggestedName,
  onCreated,
  onClose,
}: {
  entity: EntityMeta;
  /** The company the new record belongs to, where the entity belongs to one. */
  companyId: string | null;
  /** What the user had typed into the lookup; it becomes the name. */
  suggestedName: string;
  onCreated: (record: EntityRecord) => void;
  onClose: () => void;
}) {
  const scoped = isCompanyScoped(entity) && companyId !== null;
  const fields = entity.fields.filter((field) => !field.readonly && !(scoped && field.name === "company_id"));
  const nameField = fields.find((field) => field.name === "name" && field.type.kind === "text");
  const [initial] = useState<FieldValues>(() => (nameField && suggestedName ? { name: suggestedName } : {}));
  const [values, setValues] = useState<FieldValues>(initial);
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });
  const dirty = JSON.stringify(values) !== JSON.stringify(initial);

  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    // A dialog inside a form must not submit the form around it.
    event.stopPropagation();
    if (saving) return;
    setSaving(true);
    setFormError(null);
    setFieldErrors({});
    const data: JsonObject = scoped && companyId ? { company_id: companyId } : {};
    for (const field of fields) {
      const value: Json | undefined = values[field.name];
      if (value !== undefined && value !== null) data[field.name] = value;
    }
    const payload = { entity: entity.name, data };
    const text = JSON.stringify(payload);
    if (keyRef.current.payload !== text) keyRef.current = { payload: text, key: newKey() };
    try {
      const record = await api.run("entity.create", payload, { idempotencyKey: keyRef.current.key });
      forgetLinkLabels();
      onCreated(record);
    } catch (cause) {
      const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
      if (error.field && fields.some((field) => field.name === error.field))
        setFieldErrors({ [error.field]: error.message });
      else setFormError(error.message);
      setSaving(false);
    }
  }

  const formId = `quick-create-${entity.name}`;
  return (
    <Dialog
      open
      onClose={onClose}
      dirty={dirty || saving}
      title={t("record.new", { label: entity.label })}
      description={t("quick_create.description")}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={saving}>
            {t("action.cancel")}
          </Button>
          <Button type="submit" form={formId} busy={saving}>
            {t("quick_create.submit")}
          </Button>
        </>
      }
    >
      <form
        id={formId}
        noValidate
        onSubmit={(event) => void submit(event)}
        className="grid gap-x-4 gap-y-3 sm:grid-cols-2"
      >
        {formError ? (
          <Alert className="sm:col-span-2" title={t("record.save_failed")}>
            {formError}
          </Alert>
        ) : null}
        {fields.map((field) => {
          const fieldId = `${formId}-${field.name}`;
          const error = fieldErrors[field.name];
          return (
            <div key={field.name} className={field.type.kind === "json" ? "space-y-1 sm:col-span-2" : "space-y-1"}>
              <Label htmlFor={fieldId}>
                {field.label}
                {field.required ? (
                  <>
                    <span aria-hidden="true" className="text-destructive">
                      {t("field.required_mark")}
                    </span>
                    <span className="sr-only">{t("field.required")}</span>
                  </>
                ) : null}
              </Label>
              <FieldInput
                id={fieldId}
                field={field}
                value={values[field.name]}
                invalid={Boolean(error)}
                companyId={companyId}
                aria-describedby={error ? `${fieldId}-error` : undefined}
                onChange={(value) => setValues((previous) => ({ ...previous, [field.name]: value }))}
              />
              {error ? (
                <p id={`${fieldId}-error`} role="alert" className="text-sm text-destructive">
                  {error}
                </p>
              ) : null}
            </div>
          );
        })}
      </form>
    </Dialog>
  );
}

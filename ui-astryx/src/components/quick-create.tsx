import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Dialog, DialogHeader } from "@astryxdesign/core/Dialog";
import { HStack } from "@astryxdesign/core/HStack";
import { VStack } from "@astryxdesign/core/VStack";
import { type FormEvent, useRef, useState } from "react";
import { ApiError, api } from "@/api/client";
import type { EntityMeta, EntityRecord, Json, JsonObject } from "@/api/types";
import { t } from "@/i18n/t";
import { forgetLinkLabels } from "@/lib/link-labels";
import { isCompanyScoped } from "@/lib/meta";
import { newKey } from "@/lib/text";
import { FieldInput, type FieldValues, initialValues } from "./field-input";

/**
 * Creates a record from inside a lookup, in a dialog, so the form underneath keeps
 * everything typed into it. Astryx's Dialog with purpose="form": no closing by an
 * outside click once something is typed.
 */
export function QuickCreateDialog({
  entity,
  companyId,
  suggestedName,
  onCreated,
  onClose,
}: {
  entity: EntityMeta;
  companyId: string | null;
  suggestedName: string;
  onCreated: (record: EntityRecord) => void;
  onClose: () => void;
}) {
  const scoped = isCompanyScoped(entity) && companyId !== null;
  const fields = entity.fields.filter((field) => !field.readonly && !(scoped && field.name === "company_id"));
  const hasName = fields.some((field) => field.name === "name" && field.type.kind === "text");
  const [values, setValues] = useState<FieldValues>(() => ({
    ...initialValues(fields),
    ...(hasName && suggestedName ? { name: suggestedName } : {}),
  }));
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const keyRef = useRef<{ payload: string; key: string }>({ payload: "", key: newKey() });

  async function submit(event?: FormEvent): Promise<void> {
    event?.preventDefault();
    event?.stopPropagation();
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

  return (
    <Dialog isOpen onOpenChange={(open) => (open ? null : onClose())} purpose="form" width={640}>
      <DialogHeader title={t("record.new", { label: entity.label })} subtitle={t("quick_create.description")} onOpenChange={() => onClose()} />
      <form noValidate onSubmit={(event) => void submit(event)}>
        <VStack gap={4} paddingBlockStart={3}>
          {formError ? <Banner status="error" title={t("record.save_failed")} description={formError} /> : null}
          <div className="form-grid">
            {fields.map((field) => (
              <div key={field.name} className={field.type.kind === "json" ? "span-all" : undefined}>
                <FieldInput
                  field={field}
                  value={values[field.name]}
                  error={fieldErrors[field.name]}
                  companyId={companyId}
                  onChange={(value) => setValues((previous) => ({ ...previous, [field.name]: value }))}
                />
              </div>
            ))}
          </div>
          <HStack gap={2} hAlign="end">
            <Button label={t("action.cancel")} onClick={onClose} isDisabled={saving} />
            <Button label={t("quick_create.submit")} type="submit" variant="primary" isLoading={saving} />
          </HStack>
        </VStack>
      </form>
    </Dialog>
  );
}

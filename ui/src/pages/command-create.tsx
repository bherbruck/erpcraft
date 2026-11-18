// A new record of an entity the generic layer does not write, such as a user:
// a form built from its create command's input schema, each input shown as the
// entity's field of the same name where there is one. A secret input is masked
// and never filled in by the browser.

import { type FormEvent, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CommandMeta, EntityMeta, Json, JsonObject } from "@/api/types";
import { useCompany } from "@/app/context";
import { FieldInput, type FieldValues } from "@/components/field-input";
import { PageHeader } from "@/components/page";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { UnsavedDialog, useUnsavedGuard } from "@/components/unsaved-guard";
import { t } from "@/i18n/t";
import { commandInputs } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";

export function CommandCreatePage({ entity, command }: { entity: EntityMeta; command: CommandMeta }) {
  const { code } = useCompany();
  const navigate = useNavigate();
  // What must be given first, then the rest, each in the schema's order.
  const inputs = useMemo(
    () => commandInputs(command).sort((a, b) => (a.required === b.required ? 0 : a.required ? -1 : 1)),
    [command],
  );
  const initial = useMemo(() => {
    const values: FieldValues = {};
    for (const input of inputs)
      if (input.default !== undefined && input.default !== null) values[input.name] = input.default;
    return values;
  }, [inputs]);
  const [values, setValues] = useState<FieldValues>(initial);
  const [error, setError] = useState<ApiError | null>(null);
  const [saving, setSaving] = useState(false);
  const key = useRef<{ input: string; key: string }>({ input: "", key: newKey() });
  const dirty = JSON.stringify(values) !== JSON.stringify(initial);
  const { blocker, allowNext } = useUnsavedGuard(dirty);

  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (saving) return;
    const input: JsonObject = {};
    for (const property of inputs) {
      const value: Json | undefined = values[property.name];
      if (value !== undefined && value !== null && value !== "") input[property.name] = value;
    }
    const text = JSON.stringify(input);
    if (key.current.input !== text) key.current = { input: text, key: newKey() };
    setSaving(true);
    setError(null);
    try {
      const result = await api.runNamed(command.name, input, { idempotencyKey: key.current.key });
      allowNext();
      const id = result && typeof result === "object" && !Array.isArray(result) ? result.id : null;
      navigate(
        typeof id === "string" ? paths.entityRecord(code, entity.name, id) : paths.entityList(code, entity.name),
        {
          replace: true,
        },
      );
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setSaving(false);
    }
  }

  return (
    <>
      <PageHeader
        sticky
        crumbs={[{ label: entity.plural, to: paths.entityList(code, entity.name) }]}
        title={t("record.new", { label: entity.label })}
        actions={
          <Button type="submit" form="command-create" busy={saving}>
            {command.label}
          </Button>
        }
      />
      <form
        id="command-create"
        noValidate
        onSubmit={(event) => void submit(event)}
        className="w-full max-w-form space-y-4"
      >
        <p className="text-sm text-muted-foreground">{command.description}</p>
        {error && !inputs.some((input) => input.name === error.field) ? (
          <Alert title={t("record.save_failed")}>{error.message}</Alert>
        ) : null}
        <div className="grid gap-x-4 gap-y-3 rounded-md border bg-card p-4 sm:grid-cols-2">
          {inputs.map((property) => {
            const id = `field-${property.name}`;
            const field = entity.fields.find((candidate) => candidate.name === property.name);
            const fieldError = error?.field === property.name ? error.message : null;
            const describedBy = fieldError ? `${id}-error` : property.description ? `${id}-help` : undefined;
            const value = values[property.name];
            const change = (next: Json | undefined): void =>
              setValues((previous) => ({ ...previous, [property.name]: next }));
            return (
              <div key={property.name} className="space-y-1">
                <Label htmlFor={id}>
                  {field?.label ?? property.label}
                  {property.required ? (
                    <>
                      <span aria-hidden="true" className="text-destructive">
                        {t("field.required_mark")}
                      </span>
                      <span className="sr-only">{t("field.required")}</span>
                    </>
                  ) : null}
                </Label>
                {property.secret || !field ? (
                  property.type === "boolean" ? (
                    <span className="flex h-8 items-center">
                      <input
                        id={id}
                        type="checkbox"
                        className="size-4 accent-primary"
                        checked={value === true}
                        onChange={(event) => change(event.target.checked)}
                      />
                    </span>
                  ) : (
                    <Input
                      id={id}
                      type={property.secret ? "password" : "text"}
                      autoComplete={property.secret ? "new-password" : "off"}
                      value={typeof value === "string" ? value : ""}
                      aria-invalid={fieldError ? true : undefined}
                      aria-describedby={describedBy}
                      onChange={(event) => change(event.target.value)}
                    />
                  )
                ) : (
                  <FieldInput
                    id={id}
                    field={{ ...field, required: property.required, readonly: false }}
                    value={value}
                    invalid={Boolean(fieldError)}
                    aria-describedby={describedBy}
                    onChange={change}
                  />
                )}
                {fieldError ? (
                  <p id={`${id}-error`} role="alert" className="text-xs text-destructive">
                    {fieldError}
                  </p>
                ) : property.description ? (
                  <p id={`${id}-help`} className="text-xs text-muted-foreground">
                    {property.description}
                  </p>
                ) : null}
              </div>
            );
          })}
        </div>
        <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
      </form>
      <UnsavedDialog blocker={blocker} />
    </>
  );
}

import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { CheckboxInput } from "@astryxdesign/core/CheckboxInput";
import { DateInput } from "@astryxdesign/core/DateInput";
import { Dialog, DialogHeader } from "@astryxdesign/core/Dialog";
import { HStack } from "@astryxdesign/core/HStack";
import { Selector } from "@astryxdesign/core/Selector";
import { Spinner } from "@astryxdesign/core/Spinner";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import type { ISODateString } from "@astryxdesign/core/utils";
import { VStack } from "@astryxdesign/core/VStack";
import { useEffect, useRef, useState } from "react";
import { ApiError, api } from "@/api/client";
import type { FieldMeta, Json, JsonObject, Posting } from "@/api/types";
import { useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { askable, type InputProperty, type ResourceAction } from "@/lib/meta";
import { newKey } from "@/lib/text";
import { FieldInput } from "./field-input";
import { Postings } from "./postings";

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Buttons for the actions the registry offers on a record as it is now. */
export function ActionButtons({
  actions,
  disabledReason,
  onChoose,
}: {
  actions: ResourceAction[];
  disabledReason?: string | null;
  onChoose: (action: ResourceAction) => void;
}) {
  return (
    <>
      {actions.map((action) => (
        <Button
          key={action.action}
          label={action.label}
          variant={action.destructive ? "destructive" : "secondary"}
          isDisabled={Boolean(disabledReason)}
          tooltip={disabledReason ?? action.command.description ?? undefined}
          onClick={() => onChoose(action)}
        />
      ))}
    </>
  );
}

/** One input of a command, by what its schema says it is. */
function InputControl({ input, value, onChange }: { input: InputProperty; value: Json | undefined; onChange: (v: Json | undefined) => void }) {
  const { company } = useCompany();
  if (input.fieldType) {
    const field: FieldMeta = {
      name: input.name,
      label: input.label,
      type: input.fieldType,
      required: input.required,
      readonly: false,
      immutable: false,
      decimal: null,
      default: null,
      help: input.description,
    };
    return <FieldInput field={field} value={value} onChange={onChange} companyId={company?.id} />;
  }
  const text = typeof value === "string" ? value : value === undefined || value === null ? "" : String(value);
  if (input.type === "boolean")
    return <CheckboxInput label={input.label} description={input.description ?? undefined} value={value === true} onChange={onChange} />;
  if (input.options)
    return (
      <Selector
        label={input.label}
        description={input.description ?? undefined}
        isRequired={input.required}
        value={text}
        width="100%"
        onChange={(next: string) => onChange(next)}
        options={input.options.map((o) => ({ value: o.value, label: o.label, description: o.description ?? undefined }))}
      />
    );
  if (input.format === "date")
    return (
      <DateInput
        label={input.label}
        description={input.description ?? undefined}
        isRequired={input.required}
        format="date"
        value={text ? (text as ISODateString) : undefined}
        onChange={(next) => onChange(next ?? null)}
      />
    );
  return (
    <TextInput
      label={input.label}
      description={input.description ?? undefined}
      isRequired={input.required}
      type={input.secret ? "password" : "text"}
      inputMode={input.decimal || input.type === "integer" ? "decimal" : undefined}
      value={text}
      width="100%"
      onChange={(next) => onChange(next === "" ? null : input.type === "integer" && /^-?\d+$/.test(next) ? parseInt(next, 10) : next)}
    />
  );
}

/**
 * An action, asked and confirmed in a dialog. The server's preview of the same command
 * runs first and shows what will happen (the postings it will make), so nothing
 * irreversible happens unseen. Confirming runs it with one idempotency key.
 */
export function ActionDialog({
  action,
  id,
  subject,
  currency,
  onClose,
  onDone,
}: {
  action: ResourceAction;
  id: string;
  subject: string;
  currency: string;
  onClose: () => void;
  onDone: (result: Json) => void;
}) {
  const inputs = action.inputs.filter(askable);
  const [values, setValues] = useState<Record<string, Json | undefined>>({});
  const [preview, setPreview] = useState<{ result: Json | null; error: string | null; loading: boolean }>({
    result: null,
    error: null,
    loading: true,
  });
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const key = useRef(newKey());

  const input: JsonObject = { id };
  for (const each of inputs) {
    const value = values[each.name];
    if (value !== undefined && value !== null && value !== "") input[each.name] = value;
  }
  const inputKey = JSON.stringify(input);
  const ready = inputs.every((each) => !each.required || input[each.name] !== undefined);

  // biome-ignore lint/correctness/useExhaustiveDependencies: the input key names the input
  useEffect(() => {
    if (!ready) {
      setPreview({ result: null, error: null, loading: false });
      return;
    }
    const controller = new AbortController();
    setPreview((p) => ({ ...p, loading: true }));
    const timer = setTimeout(() => {
      api.preview(action.command.name, input, { signal: controller.signal }).then(
        (result) => setPreview({ result, error: null, loading: false }),
        (cause: unknown) => {
          if (controller.signal.aborted) return;
          setPreview({ result: null, error: cause instanceof ApiError ? cause.message : String(cause), loading: false });
        },
      );
    }, 250);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [inputKey, ready, action.command.name]);

  async function confirm(): Promise<void> {
    setRunning(true);
    setError(null);
    try {
      const result = await api.runNamed(action.command.name, input, { idempotencyKey: key.current });
      onDone(result);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause.message : String(cause));
      setRunning(false);
    }
  }

  const postings: Posting[] =
    isObject(preview.result) && Array.isArray(preview.result.postings) ? (preview.result.postings as unknown as Posting[]) : [];
  const status = isObject(preview.result) && typeof preview.result.status === "string" ? preview.result.status : null;

  return (
    <Dialog isOpen onOpenChange={(open) => (open ? null : onClose())} purpose="form" width={760} padding={0}>
      <DialogHeader title={t("preview.title", { action: action.label, subject })} subtitle={action.command.description} onOpenChange={() => onClose()} />
      <VStack gap={4} padding={4}>
        {inputs.length > 0 ? (
          <div className="form-grid">
            {inputs.map((each) => (
              <InputControl
                key={each.name}
                input={each}
                value={values[each.name]}
                onChange={(v) => setValues((previous) => ({ ...previous, [each.name]: v }))}
              />
            ))}
          </div>
        ) : null}
        <VStack gap={2}>
          <Text weight="semibold">{t("preview.heading")}</Text>
          {preview.loading ? <Spinner size="sm" aria-label={t("value.loading")} /> : null}
          {preview.error ? <Banner status="error" title={t("preview.refused")} description={preview.error} /> : null}
          {!preview.loading && !preview.error && preview.result !== null ? (
            postings.length > 0 ? (
              <Postings postings={postings} currency={currency} />
            ) : (
              <Text type="supporting">{status ? `${t("document.status")}: ${status}` : t("preview.no_change")}</Text>
            )
          ) : null}
        </VStack>
        {error ? <Banner status="error" title={t("preview.failed", { action: action.label })} description={error} /> : null}
        <HStack gap={2} hAlign="end">
          <Button label={t("action.cancel")} onClick={onClose} isDisabled={running} />
          <Button
            label={t("preview.confirm", { action: action.label })}
            variant={action.destructive ? "destructive" : "primary"}
            isLoading={running}
            isDisabled={!ready || Boolean(preview.error) || preview.loading}
            onClick={() => void confirm()}
          />
        </HStack>
      </VStack>
    </Dialog>
  );
}

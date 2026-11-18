import { CheckboxInput } from "@astryxdesign/core/CheckboxInput";
import { DateInput } from "@astryxdesign/core/DateInput";
import { Selector } from "@astryxdesign/core/Selector";
import { TextArea } from "@astryxdesign/core/TextArea";
import { TextInput } from "@astryxdesign/core/TextInput";
import type { ISODateString } from "@astryxdesign/core/utils";
import { type KeyboardEvent, useEffect, useState } from "react";
import type { FieldMeta, Json } from "@/api/types";
import { t } from "@/i18n/t";
import { formatTimestamp } from "@/lib/dates";
import { trimDecimal } from "@/lib/decimal";
import { DecimalInput } from "./decimal-input";
import { enumLabel } from "@/lib/meta";
import { useMinFraction } from "./decimal-value";
import { toRowContext } from "./field-value";
import { Lookup } from "./lookup";
import {
  CommandLookupInput,
  DocumentLinkInput,
  EntityLinkInput,
  type FieldContext,
  LineLinkInput,
  type ReferenceInputProps,
} from "./references";

export type FieldValues = Record<string, Json | undefined>;

/** A value as it is shown in its input: stored decimals lose their trailing zeros. */
export function toEditable(field: FieldMeta, value: Json | undefined): Json | undefined {
  if (field.type.kind === "decimal" && typeof value === "string") return trimDecimal(value);
  return value;
}

/** The value a new record or line starts with, as the registry declares it. */
export function defaultValue(field: FieldMeta): Json | undefined {
  const value = field.default;
  if (value === null || value === undefined) return undefined;
  switch (field.type.kind) {
    case "decimal":
      return typeof value === "number" ? String(value) : typeof value === "string" ? trimDecimal(value) : undefined;
    case "text_array":
      if (Array.isArray(value)) return value;
      if (typeof value === "string" && /^\{.*\}$/.test(value)) {
        const inner = value.slice(1, -1).trim();
        return inner === "" ? [] : inner.split(",").map((part) => part.trim().replace(/^"(.*)"$/, "$1"));
      }
      return undefined;
    case "json":
      if (typeof value !== "string") return value;
      try {
        return JSON.parse(value) as Json;
      } catch {
        return undefined;
      }
    default:
      return value;
  }
}

/** The starting values of a new record or line: each field's declared default. */
export function initialValues(fields: FieldMeta[]): FieldValues {
  const values: FieldValues = {};
  for (const field of fields) {
    if (field.readonly) continue;
    const value = defaultValue(field);
    if (value !== undefined) values[field.name] = value;
  }
  return values;
}

export interface FieldInputProps {
  field: FieldMeta;
  value: Json | undefined;
  onChange: (value: Json | undefined) => void;
  /** The label Astryx renders; defaults to the field's. */
  label?: string;
  /** In a grid cell the column header names the field; the label stays for screen readers. */
  isLabelHidden?: boolean;
  size?: "sm" | "md";
  disabled?: boolean;
  /** The server's words about this field, shown at the field. */
  error?: string | null;
  /** The company whose records a link field offers. */
  companyId?: string | null;
  context?: FieldContext;
  onKeyDown?: (event: KeyboardEvent<HTMLElement>) => void;
}

/** A decimal field: the shared DecimalInput, at its currency's decimals while not being edited. */
function DecimalField(props: FieldInputProps) {
  const minFraction = useMinFraction(props.field.decimal, toRowContext(props.context));
  return (
    <DecimalInput
      label={props.label ?? props.field.label}
      isLabelHidden={props.isLabelHidden}
      isRequired={props.field.required}
      isDisabled={props.disabled}
      labelTooltip={props.isLabelHidden ? undefined : (props.field.help ?? undefined)}
      size={props.size}
      scale={props.field.decimal?.scale}
      minFraction={minFraction}
      value={typeof props.value === "string" ? props.value : props.value == null ? null : String(props.value)}
      onChange={(next) => props.onChange(next)}
      onKeyDown={props.onKeyDown}
      error={props.error}
      isCompactStatus={props.isLabelHidden}
    />
  );
}

function JsonInput(props: FieldInputProps) {
  const { value, onChange } = props;
  const [text, setText] = useState(() => (value === undefined || value === null ? "" : JSON.stringify(value, null, 2)));
  const [bad, setBad] = useState(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: only a value from outside (a reload) replaces the text
  useEffect(() => {
    if (!bad) setText(value === undefined || value === null ? "" : JSON.stringify(value, null, 2));
  }, [value]);
  return (
    <TextArea
      label={props.label ?? props.field.label}
      isLabelHidden={props.isLabelHidden}
      isRequired={props.field.required}
      isDisabled={props.disabled}
      value={text}
      status={bad ? { type: "error", message: t("field.json_invalid") } : props.error ? { type: "error", message: props.error } : undefined}
      onChange={(next) => {
        setText(next);
        if (next.trim() === "") {
          setBad(false);
          onChange(null);
          return;
        }
        try {
          onChange(JSON.parse(next) as Json);
          setBad(false);
        } catch {
          setBad(true);
        }
      }}
    />
  );
}

/**
 * The control for one field, chosen by the field's type in the registry. Each is an
 * Astryx input used as it comes, with Astryx's own label, required marker and error.
 */
export function FieldInput(props: FieldInputProps) {
  const { field, value, onChange, disabled, companyId, onKeyDown, context } = props;
  const type = field.type;
  const label = props.label ?? field.label;
  const status = props.error ? ({ type: "error", message: props.error } as const) : undefined;
  const common = {
    label,
    isLabelHidden: props.isLabelHidden,
    isRequired: field.required,
    isDisabled: disabled,
    size: props.size,
    status,
    // In a grid cell the error sits behind the status icon, so a row does not grow.
    // In a form it floats below the field instead of the large attached box.
    statusVariant: props.isLabelHidden ? ("tooltip" as const) : ("detached" as const),
    // The registry's help sits behind an info button on the label, so it does not push the form apart.
    labelTooltip: props.isLabelHidden ? undefined : (field.help ?? undefined),
  };
  const text = typeof value === "string" ? value : value === null || value === undefined ? "" : String(value);

  switch (type.kind) {
    case "decimal":
      return <DecimalField {...props} />;
    case "int":
      return (
        <div className="num">
          <TextInput
            {...common}
            inputMode="numeric"
            value={text}
            onKeyDown={onKeyDown}
            width="100%"
            onChange={(next) => {
              const trimmed = next.trim();
              if (trimmed === "") onChange(null);
              else if (/^-?\d+$/.test(trimmed)) onChange(parseInt(trimmed, 10));
              else if (trimmed === "-") onChange(trimmed);
            }}
          />
        </div>
      );
    case "bool":
      return (
        <CheckboxInput
          label={label}
          isLabelHidden={props.isLabelHidden}
          description={common.labelTooltip}
          isDisabled={disabled}
          // Not yet answered: the server will apply its own default.
          value={value === undefined || value === null ? "indeterminate" : value === true}
          onChange={(checked) => onChange(checked)}
        />
      );
    case "date":
      return (
        <DateInput
          {...common}
          format="date"
          hasClear={!field.required}
          value={text ? (text as ISODateString) : undefined}
          onChange={(next) => onChange(next ?? null)}
        />
      );
    case "enum":
      return (
        <Selector
          {...common}
          width="100%"
          value={text}
          hasClear={!field.required}
          placeholder={field.required ? t("field.choose") : t("value.none")}
          onChange={(next: string | null) => onChange(next === "" || next === null ? null : next)}
          options={type.values.map((option) => ({ value: option, label: enumLabel(field, option) }))}
        />
      );
    case "link":
      return (
        // biome-ignore lint/a11y/noStaticElementInteractions: listens to keys that bubble up from the lookup inside
        <div onKeyDown={onKeyDown} role="presentation" className="min-w-0">
          <Lookup
            label={label}
            isLabelHidden={props.isLabelHidden}
            labelTooltip={common.labelTooltip}
            size={props.size}
            recordLink={!props.isLabelHidden}
            entity={type.entity}
            keyField={type.key}
            filters={type.filters}
            value={typeof value === "string" ? value : null}
            onChange={(next) => onChange(next)}
            companyId={companyId}
            required={field.required}
            disabled={disabled}
            error={props.error}
          />
        </div>
      );
    case "document_link":
    case "line_link":
    case "lookup":
    case "entity_link": {
      const reference: ReferenceInputProps = {
        field,
        value: typeof value === "string" ? value : null,
        onChange: (next) => onChange(next),
        context,
        label,
        isLabelHidden: props.isLabelHidden,
        disabled,
        error: props.error,
        size: props.size,
      };
      return (
        // biome-ignore lint/a11y/noStaticElementInteractions: listens to keys that bubble up from the picker inside
        <div onKeyDown={onKeyDown} role="presentation" className="min-w-0">
          {type.kind === "document_link" ? (
            <DocumentLinkInput {...reference} type={type} />
          ) : type.kind === "line_link" ? (
            <LineLinkInput {...reference} type={type} />
          ) : type.kind === "lookup" ? (
            <CommandLookupInput {...reference} type={type} />
          ) : (
            <EntityLinkInput {...reference} type={type} companyId={companyId} />
          )}
        </div>
      );
    }
    case "text_array":
      return (
        <TextInput
          {...common}
          width="100%"
          value={Array.isArray(value) ? value.join(", ") : text}
          onKeyDown={onKeyDown}
          onChange={(next) =>
            onChange(
              next
                .split(",")
                .map((part) => part.trim())
                .filter((part) => part !== ""),
            )
          }
        />
      );
    case "json":
      return <JsonInput {...props} />;
    case "posting":
    case "timestamp":
      return <TextInput {...common} width="100%" isReadOnly value={text ? formatTimestamp(text) : ""} />;
    default:
      return (
        <TextInput
          {...common}
          width="100%"
          value={text}
          onKeyDown={onKeyDown}
          hasSpellCheck={type.kind === "uuid" ? false : undefined}
          onChange={(next) => onChange(next === "" ? null : next)}
        />
      );
  }
}

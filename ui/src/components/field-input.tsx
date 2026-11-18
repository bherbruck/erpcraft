import { type ComponentProps, type KeyboardEvent, useEffect, useState } from "react";
import type { FieldMeta, Json } from "@/api/types";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { NativeSelect } from "@/components/ui/native-select";
import { Textarea } from "@/components/ui/textarea";
import { t } from "@/i18n/t";
import { trimDecimal } from "@/lib/decimal";
import { enumLabel } from "@/lib/meta";
import { DateInput } from "./date-input";
import { useMinFraction } from "./decimal-value";
import { FieldValue, toRowContext } from "./field-value";
import { Lookup } from "./lookup";
import { MoneyInput } from "./money-input";
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

/**
 * The value a new record or line starts with, as the registry declares it.
 * An entity's default is read from its table, so it can arrive in the
 * database's own notation ("{}" for an empty list, "[]" as text for JSON).
 */
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
  id?: string;
  disabled?: boolean;
  invalid?: boolean;
  /** The company whose records a link field offers. */
  companyId?: string | null;
  /** The record or line the field is part of, for references that depend on other fields. */
  context?: FieldContext;
  /** Names the control where there is no visible label, as in a grid cell. */
  "aria-label"?: string;
  "aria-describedby"?: string;
  onKeyDown?: (event: KeyboardEvent<HTMLElement>) => void;
}

function JsonInput({ value, onChange, id, disabled, ...aria }: FieldInputProps) {
  const [text, setText] = useState(() => (value === undefined || value === null ? "" : JSON.stringify(value, null, 2)));
  const [bad, setBad] = useState(false);
  // biome-ignore lint/correctness/useExhaustiveDependencies: only a value from outside (a reload) replaces the text
  useEffect(() => {
    if (!bad) setText(value === undefined || value === null ? "" : JSON.stringify(value, null, 2));
  }, [value]);
  return (
    <div className="space-y-1">
      <Textarea
        id={id}
        disabled={disabled}
        value={text}
        aria-invalid={bad || aria.invalid || undefined}
        aria-label={aria["aria-label"]}
        spellCheck={false}
        onChange={(event) => {
          const next = event.target.value;
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
      {bad ? <p className="text-sm text-destructive">{t("field.json_invalid")}</p> : null}
    </div>
  );
}

/** A decimal field, shown at the decimals its currency has while it is not being edited. */
function DecimalInput({
  field,
  context,
  ...props
}: Omit<ComponentProps<typeof MoneyInput>, "minFraction"> & { field: FieldMeta; context?: FieldContext }) {
  const minFraction = useMinFraction(field.decimal, toRowContext(context));
  return <MoneyInput {...props} minFraction={minFraction} />;
}

/** The control for one field, chosen by the field's type in the registry. */
export function FieldInput(props: FieldInputProps) {
  const { field, value, onChange, id, disabled, invalid, companyId, onKeyDown, context } = props;
  const type = field.type;
  const common = {
    id,
    disabled,
    "aria-invalid": invalid || undefined,
    "aria-label": props["aria-label"],
    "aria-describedby": props["aria-describedby"],
    "aria-required": field.required || undefined,
    onKeyDown,
  };
  const text = typeof value === "string" ? value : value === null || value === undefined ? "" : String(value);

  switch (type.kind) {
    case "decimal":
      return (
        <DecimalInput
          {...common}
          field={field}
          context={context}
          value={text}
          onChange={(next) => onChange(next === "" ? null : next)}
        />
      );
    case "int":
      return (
        <Input
          {...common}
          inputMode="numeric"
          className="text-end tabular-nums"
          value={text}
          onChange={(event) => {
            const next = event.target.value.trim();
            if (next === "") onChange(null);
            else if (/^-?\d+$/.test(next)) onChange(parseInt(next, 10));
            else if (next === "-") onChange(next);
          }}
        />
      );
    case "bool":
      return (
        <span className="flex h-8 items-center">
          <Checkbox
            {...common}
            // Not yet answered: the server will apply its own default.
            checked={value === true ? true : value === undefined || value === null ? "indeterminate" : false}
            onCheckedChange={(checked) => onChange(checked === true)}
          />
        </span>
      );
    case "date":
      return <DateInput {...common} value={text} onChange={(next) => onChange(next === "" ? null : next)} />;
    case "enum":
      return (
        <NativeSelect
          className="w-full"
          {...common}
          value={text}
          onChange={(event) => onChange(event.target.value === "" ? null : event.target.value)}
        >
          {field.required && text !== "" ? null : (
            <option value="">{field.required ? t("field.choose") : t("value.none")}</option>
          )}
          {type.values.map((option) => (
            <option key={option} value={option}>
              {enumLabel(field, option)}
            </option>
          ))}
        </NativeSelect>
      );
    case "link":
      return (
        // biome-ignore lint/a11y/noStaticElementInteractions: listens to keys that bubble up from the lookup inside
        <div onKeyDown={onKeyDown} role="presentation" className="min-w-0">
          <Lookup
            id={id}
            entity={type.entity}
            keyField={type.key}
            filters={type.filters}
            value={typeof value === "string" ? value : null}
            onChange={(next) => onChange(next)}
            companyId={companyId}
            required={field.required}
            disabled={disabled}
            invalid={invalid}
            aria-label={props["aria-label"]}
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
        id,
        disabled,
        invalid,
        "aria-label": props["aria-label"],
        "aria-describedby": props["aria-describedby"],
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
        <Input
          {...common}
          value={Array.isArray(value) ? value.join(", ") : text}
          onChange={(event) =>
            onChange(
              event.target.value
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
      return (
        <span className="flex h-8 items-center text-sm">
          <FieldValue field={field} value={value} />
        </span>
      );
    case "uuid":
      return (
        <Input
          {...common}
          className="font-mono text-xs"
          value={text}
          spellCheck={false}
          onChange={(event) => onChange(event.target.value.trim() === "" ? null : event.target.value.trim())}
        />
      );
    default:
      return (
        <Input
          {...common}
          value={text}
          onChange={(event) => onChange(event.target.value === "" ? null : event.target.value)}
        />
      );
  }
}

import type { FieldMeta, Json } from "@/api/types";
import { Input } from "@/components/ui/input";
import { NativeSelect } from "@/components/ui/native-select";
import { t } from "@/i18n/t";
import type { InputProperty } from "@/lib/meta";
import { DateInput } from "./date-input";
import { FieldInput } from "./field-input";
import { MoneyInput } from "./money-input";

/** A command's input as a field, for an input the server describes as one (`x-field-type`). */
function asField(property: InputProperty): FieldMeta | null {
  if (!property.fieldType) return null;
  return {
    name: property.name,
    label: property.label,
    type: property.fieldType,
    required: property.required,
    readonly: false,
    immutable: false,
    help: property.description,
    default: null,
    decimal: null,
  };
}

/** The control for one input of a command, chosen by what its schema says it is. */
export function CommandInput({
  property,
  id,
  value,
  onChange,
  disabled,
  companyId,
  describedBy,
}: {
  property: InputProperty;
  id: string;
  value: Json | undefined;
  onChange: (value: Json) => void;
  disabled?: boolean;
  companyId?: string | null;
  describedBy?: string;
}) {
  const text = typeof value === "string" ? value : typeof value === "number" ? String(value) : "";
  const field = asField(property);
  if (field) {
    return (
      <FieldInput
        id={id}
        field={field}
        value={value}
        disabled={disabled}
        companyId={companyId}
        aria-describedby={describedBy}
        onChange={(next) => onChange(next ?? null)}
      />
    );
  }
  if (property.type === "boolean") {
    return (
      <span className="flex h-8 items-center">
        <input
          id={id}
          type="checkbox"
          className="size-4 accent-primary"
          checked={value === true}
          disabled={disabled}
          aria-describedby={describedBy}
          onChange={(event) => onChange(event.target.checked)}
        />
      </span>
    );
  }
  if (property.format === "date") {
    return <DateInput id={id} value={text} onChange={onChange} disabled={disabled} aria-describedby={describedBy} />;
  }
  if (property.options) {
    return (
      <NativeSelect
        className="w-full"
        id={id}
        value={text}
        disabled={disabled}
        aria-describedby={describedBy}
        onChange={(event) => onChange(event.target.value)}
      >
        {property.required ? null : <option value="">{t("value.none")}</option>}
        {property.options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </NativeSelect>
    );
  }
  if (property.decimal) {
    return <MoneyInput id={id} value={text} onChange={onChange} disabled={disabled} aria-describedby={describedBy} />;
  }
  return (
    <Input
      id={id}
      type={property.secret ? "password" : "text"}
      autoComplete={property.secret ? "new-password" : "off"}
      inputMode={property.type === "integer" ? "numeric" : undefined}
      value={text}
      disabled={disabled}
      aria-describedby={describedBy}
      onChange={(event) => onChange(event.target.value)}
    />
  );
}

/** The label of an input, with the mark of a required one. */
export function InputLabelText({ property }: { property: InputProperty }) {
  return (
    <>
      {property.label}
      {property.required ? (
        <>
          <span aria-hidden="true" className="text-destructive">
            {t("field.required_mark")}
          </span>
          <span className="sr-only">{t("field.required")}</span>
        </>
      ) : null}
    </>
  );
}

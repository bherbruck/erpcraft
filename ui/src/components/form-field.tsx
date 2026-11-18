import type { ReactNode } from "react";
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field";
import { t } from "@/i18n/t";
import { cn } from "@/lib/utils";

/**
 * One field of a form, laid out with shadcn/ui's Field: the label (with the
 * required mark, GUIDELINES.md Forms 6), the input, and under it the server's
 * error for this field (Forms 3) or else its help.
 *
 * The ids are the ones the input's `aria-describedby` names: `<id>-error` and
 * `<id>-hint` unless given.
 */
export function FormField({
  id,
  label,
  required = false,
  error,
  hint,
  hintId,
  note,
  labelFor,
  className,
  children,
}: {
  id: string;
  label: ReactNode;
  /** Marks the field required before anything is submitted. */
  required?: boolean;
  error?: ReactNode;
  hint?: ReactNode;
  hintId?: string;
  /** A further line under the field, such as "Cannot be changed later". */
  note?: ReactNode;
  /** The control the label names; `id` unless the field shows a value instead of an input. */
  labelFor?: string | null;
  className?: string;
  children: ReactNode;
}) {
  return (
    <Field data-invalid={error ? true : undefined} className={cn("gap-2", className)}>
      <FieldLabel htmlFor={labelFor === null ? undefined : (labelFor ?? id)} className="gap-1">
        {label}
        {required ? (
          <>
            <span aria-hidden="true" className="text-destructive">
              {t("field.required_mark")}
            </span>
            <span className="sr-only">{t("field.required")}</span>
          </>
        ) : null}
      </FieldLabel>
      {children}
      {error ? (
        <FieldError id={`${id}-error`}>{error}</FieldError>
      ) : hint ? (
        <FieldDescription id={hintId ?? `${id}-hint`}>{hint}</FieldDescription>
      ) : null}
      {note ? <FieldDescription>{note}</FieldDescription> : null}
    </Field>
  );
}

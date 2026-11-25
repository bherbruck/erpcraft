import { Info } from "lucide-react";
import type { ReactNode } from "react";
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { t } from "@/i18n/t";
import { cn } from "@/lib/utils";

/**
 * One field of a form, laid out with shadcn/ui's Field: the label (with the
 * required mark, GUIDELINES.md Forms 6), the input, and under it the server's
 * error for this field (Forms 3).
 *
 * The field's help is behind an ⓘ beside the label, so a form reads as its
 * labels and values rather than as paragraphs. The tip opens on hover and on
 * keyboard focus, and the same text is the input's description (`<id>-hint`,
 * which its `aria-describedby` names), so a screen reader reads it with the field.
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
      <div className="flex items-center gap-1.5">
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
        {hint ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <button
                type="button"
                className="inline-flex size-5 items-center justify-center rounded-full text-muted-foreground hover:text-foreground"
                aria-label={t("field.about", { label: typeof label === "string" ? label : "" })}
              >
                <Info aria-hidden="true" className="size-4" />
              </button>
            </TooltipTrigger>
            <TooltipContent side="top" className="max-w-xs">
              {hint}
            </TooltipContent>
          </Tooltip>
        ) : null}
      </div>
      {children}
      {hint ? (
        <FieldDescription id={hintId ?? `${id}-hint`} className="sr-only">
          {hint}
        </FieldDescription>
      ) : null}
      {error ? <FieldError id={`${id}-error`}>{error}</FieldError> : null}
      {note ? <FieldDescription>{note}</FieldDescription> : null}
    </Field>
  );
}

import { type InputHTMLAttributes, type Ref, useState } from "react";
import { Input } from "@/components/ui/input";
import { formatDecimal, isDecimal, isLocalDecimalDraft, parseLocalDecimal, toLocalDraft } from "@/lib/decimal";
import { cn } from "@/lib/utils";

/**
 * An input for amounts and quantities. The value is a decimal string from the
 * first keystroke to the request body; it is never parsed into a number.
 * While it is not being edited it shows the value as read-only values show it:
 * grouped, and with the decimals its kind always has (an amount in dollars as
 * `-282.30`); while it is being edited, the digits as typed.
 */
export function MoneyInput({
  value,
  onChange,
  minFraction = 0,
  className,
  onFocus,
  onBlur,
  ...props
}: Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "type"> & {
  value: string;
  onChange: (value: string) => void;
  /** Decimals always shown while the value is not being edited. */
  minFraction?: number;
  ref?: Ref<HTMLInputElement>;
}) {
  // While it is edited, the text as typed in the user's locale ("1.234,5" in German); the value is
  // the exact decimal it reads as, passed on whenever the text is one.
  const [draft, setDraft] = useState<string | null>(null);
  const shown = draft ?? (isDecimal(value) ? formatDecimal(value, { minFraction }) : value);
  return (
    <Input
      inputMode="decimal"
      autoComplete="off"
      value={shown}
      onFocus={(event) => {
        setDraft(toLocalDraft(value));
        onFocus?.(event);
      }}
      onBlur={(event) => {
        setDraft(null);
        onBlur?.(event);
      }}
      onChange={(event) => {
        const next = event.target.value;
        if (!isLocalDecimalDraft(next)) return;
        setDraft(next);
        // An empty field, or a sign alone, is no amount yet.
        const parsed = parseLocalDecimal(next);
        if (parsed !== null) onChange(parsed);
        else if (next.trim() === "") onChange("");
      }}
      className={cn("text-end tabular-nums", className)}
      {...props}
    />
  );
}

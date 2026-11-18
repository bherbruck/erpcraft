import { type InputHTMLAttributes, type Ref, useState } from "react";
import { Input } from "@/components/ui/input";
import { formatDecimal, isDecimal, isDecimalDraft } from "@/lib/decimal";
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
  const [editing, setEditing] = useState(false);
  const shown = editing || !isDecimal(value) ? value : formatDecimal(value, { minFraction });
  return (
    <Input
      inputMode="decimal"
      autoComplete="off"
      value={shown}
      onFocus={(event) => {
        setEditing(true);
        onFocus?.(event);
      }}
      onBlur={(event) => {
        setEditing(false);
        onBlur?.(event);
      }}
      onChange={(event) => {
        // A decimal comma is what many keyboards produce; the server expects a point.
        const next = event.target.value.replace(",", ".");
        if (isDecimalDraft(next)) onChange(next);
      }}
      className={cn("text-end tabular-nums", className)}
      {...props}
    />
  );
}

import type { InputHTMLAttributes } from "react";
import { Input } from "@/components/ui/input";

/**
 * An input for a calendar date. The value is the plain "YYYY-MM-DD" string the
 * server uses; no Date object and no time zone are involved.
 */
export function DateInput({
  value,
  onChange,
  ...props
}: Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "type"> & {
  value: string;
  onChange: (value: string) => void;
}) {
  return <Input type="date" value={value} onChange={(event) => onChange(event.target.value)} {...props} />;
}

import type { LabelHTMLAttributes } from "react";
import { cn } from "@/lib/cn";

export function Label({ className, children, ...props }: LabelHTMLAttributes<HTMLLabelElement>) {
  return (
    // biome-ignore lint/a11y/noLabelWithoutControl: the caller passes htmlFor or wraps the control
    <label className={cn("text-sm font-medium leading-none", className)} {...props}>
      {children}
    </label>
  );
}

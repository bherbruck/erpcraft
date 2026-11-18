import type { Ref, SelectHTMLAttributes } from "react";
import { cn } from "@/lib/cn";
import { inputClasses } from "./input";

/** A plain select, for values defined in code. Values from the database use a Lookup. */
export function Select({
  className,
  children,
  ...props
}: SelectHTMLAttributes<HTMLSelectElement> & { ref?: Ref<HTMLSelectElement> }) {
  return (
    <select className={cn(inputClasses, "pr-7", className)} {...props}>
      {children}
    </select>
  );
}

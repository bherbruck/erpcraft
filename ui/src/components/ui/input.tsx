import type { InputHTMLAttributes, Ref, TextareaHTMLAttributes } from "react";
import { cn } from "@/lib/cn";

export const inputClasses = cn(
  "flex h-8 w-full min-w-0 rounded-md border border-input bg-background px-2.5 text-sm",
  "placeholder:text-muted-foreground disabled:cursor-not-allowed disabled:bg-muted disabled:text-muted-foreground",
  "aria-invalid:border-destructive",
);

export function Input({
  className,
  type = "text",
  ...props
}: InputHTMLAttributes<HTMLInputElement> & { ref?: Ref<HTMLInputElement> }) {
  return <input type={type} className={cn(inputClasses, className)} {...props} />;
}

export function Textarea({ className, ...props }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea className={cn(inputClasses, "h-auto min-h-24 py-1.5 font-mono text-xs", className)} {...props} />;
}

import type { ComponentProps } from "react";
import { Button as ShadcnButton } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";

export type ButtonProps = ComponentProps<typeof ShadcnButton> & {
  /** Shows progress and disables the button while its command runs (GUIDELINES.md, Forms 5). */
  busy?: boolean;
};

/**
 * shadcn/ui's Button with the two things every screen needs from it: it is
 * `type="button"` unless it says otherwise, so a button inside a form never
 * submits it by accident, and `busy` shows a spinner and disables it.
 */
export function Button({ busy = false, disabled, children, type = "button", asChild, ...props }: ButtonProps) {
  if (asChild) {
    return (
      <ShadcnButton asChild {...props}>
        {children}
      </ShadcnButton>
    );
  }
  return (
    <ShadcnButton type={type} disabled={disabled || busy} aria-busy={busy || undefined} {...props}>
      {busy ? <Spinner data-icon="inline-start" /> : null}
      {children}
    </ShadcnButton>
  );
}

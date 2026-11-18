import type { ComponentProps } from "react";
import { Badge as ShadcnBadge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

export type BadgeTone = "neutral" | "info" | "success" | "warning" | "destructive";

// shadcn/ui's Badge has no status tones, so each is its outline badge on the tone's soft background.
// The word is always shown; the color only supports it (GUIDELINES.md, Accessibility 2).
const tones: Record<BadgeTone, string> = {
  neutral: "bg-muted text-foreground",
  info: "border-info/30 bg-info-soft text-info",
  success: "border-success/30 bg-success-soft text-success",
  warning: "border-warning/30 bg-warning-soft text-warning",
  destructive: "border-destructive/30 bg-destructive-soft text-destructive",
};

/** A status: shadcn/ui's Badge in one of the status tones. */
export function Badge({
  tone = "neutral",
  className,
  ...props
}: Omit<ComponentProps<typeof ShadcnBadge>, "variant"> & { tone?: BadgeTone }) {
  return <ShadcnBadge variant="outline" className={cn(tones[tone], className)} {...props} />;
}

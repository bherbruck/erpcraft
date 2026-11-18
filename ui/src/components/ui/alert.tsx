import { CircleAlert, Info, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "@/lib/cn";

type Tone = "info" | "warning" | "destructive";

const tones: Record<Tone, string> = {
  info: "border-info/30 bg-info-soft text-foreground [&>svg]:text-info",
  warning: "border-warning/40 bg-warning-soft text-foreground [&>svg]:text-warning",
  destructive: "border-destructive/40 bg-destructive-soft text-foreground [&>svg]:text-destructive",
};

const icons: Record<Tone, typeof Info> = { info: Info, warning: TriangleAlert, destructive: CircleAlert };

/** A message that stays on the page, next to what it is about. */
export function Alert({
  tone = "destructive",
  title,
  children,
  actions,
  className,
}: {
  tone?: Tone;
  title?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  const Icon = icons[tone];
  return (
    <div
      role={tone === "info" ? "status" : "alert"}
      className={cn("flex items-start gap-2 rounded-md border px-3 py-2 text-sm", tones[tone], className)}
    >
      <Icon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-1">
        {title ? <p className="font-medium">{title}</p> : null}
        {children ? <div className="break-words">{children}</div> : null}
        {actions ? <div className="flex flex-wrap gap-2 pt-1">{actions}</div> : null}
      </div>
    </div>
  );
}

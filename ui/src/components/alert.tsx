import { CircleAlert, Info, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import { AlertDescription, AlertTitle, Alert as ShadcnAlert } from "@/components/ui/alert";
import { cn } from "@/lib/utils";

type Tone = "info" | "warning" | "destructive";

const tones: Record<Tone, string> = {
  info: "border-info/30 bg-info-soft text-foreground *:[svg]:text-info",
  warning: "border-warning/40 bg-warning-soft text-foreground *:[svg]:text-warning",
  destructive: "border-destructive/40 bg-destructive-soft",
};

const icons: Record<Tone, typeof Info> = { info: Info, warning: TriangleAlert, destructive: CircleAlert };

/**
 * A message that stays on the page, next to what it is about: shadcn/ui's
 * Alert with an icon per tone, an optional title and a row of actions.
 */
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
    <ShadcnAlert
      variant={tone === "destructive" ? "destructive" : "default"}
      role={tone === "info" ? "status" : "alert"}
      className={cn(tones[tone], className)}
    >
      <Icon aria-hidden="true" />
      {title ? <AlertTitle>{title}</AlertTitle> : null}
      {children || actions ? (
        <AlertDescription className={cn("text-foreground", tone === "destructive" && "text-destructive")}>
          {children ? <div className="break-words">{children}</div> : null}
          {actions ? <div className="flex flex-wrap gap-2 pt-2">{actions}</div> : null}
        </AlertDescription>
      ) : null}
    </ShadcnAlert>
  );
}

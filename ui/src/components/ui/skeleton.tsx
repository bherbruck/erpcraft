import { cn } from "@/lib/cn";

/** The shape of content that is still loading. */
export function Skeleton({ className }: { className?: string }) {
  return <div aria-hidden="true" className={cn("h-4 animate-pulse rounded-sm bg-muted", className)} />;
}

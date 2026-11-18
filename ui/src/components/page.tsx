import type { ReactNode } from "react";
import { Link } from "react-router";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";

export interface Crumb {
  label: string;
  to?: string;
}

/**
 * The top of a page: where it is, what it is, and what can be done.
 * With `sticky`, it stays in view while the page scrolls, which a document
 * needs so its status and number are never out of sight.
 */
export function PageHeader({
  crumbs,
  title,
  badges,
  actions,
  sticky = false,
}: {
  crumbs?: Crumb[];
  title: ReactNode;
  badges?: ReactNode;
  actions?: ReactNode;
  sticky?: boolean;
}) {
  return (
    <div className={cn("-mx-4 -mt-4 mb-4 border-b bg-background px-4 py-3", sticky && "sticky top-topbar z-10")}>
      {crumbs && crumbs.length > 0 ? (
        <nav
          aria-label={t("nav.breadcrumb")}
          className="mb-1 flex flex-wrap items-center gap-1 text-xs text-muted-foreground"
        >
          {crumbs.map((crumb, index) => (
            <span key={crumb.label} className="flex items-center gap-1">
              {index > 0 ? <span aria-hidden="true">{t("nav.crumb_separator")}</span> : null}
              {crumb.to ? (
                <Link to={crumb.to} className="hover:text-foreground hover:underline">
                  {crumb.label}
                </Link>
              ) : (
                crumb.label
              )}
            </span>
          ))}
        </nav>
      ) : null}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h1 className="min-w-0 truncate text-lg font-semibold tracking-tight">{title}</h1>
        {badges}
        <div className="flex flex-1 flex-wrap items-center justify-end gap-2">{actions}</div>
      </div>
    </div>
  );
}

/** Tabs whose selection is part of the URL: each tab is a link. */
export function Tabs({ tabs, current }: { tabs: { id: string; label: string; to: string }[]; current: string }) {
  return (
    <div role="tablist" className="mb-4 flex gap-1 border-b">
      {tabs.map((tab) => (
        <Link
          key={tab.id}
          to={tab.to}
          role="tab"
          aria-selected={tab.id === current}
          className={cn(
            "-mb-px border-b-2 px-3 py-1.5 text-sm",
            tab.id === current
              ? "border-foreground font-medium text-foreground"
              : "border-transparent text-muted-foreground hover:text-foreground",
          )}
        >
          {tab.label}
        </Link>
      ))}
    </div>
  );
}

export function Section({ title, children, aside }: { title: string; children: ReactNode; aside?: ReactNode }) {
  return (
    <section className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-sm font-semibold">{title}</h2>
        {aside}
      </div>
      {children}
    </section>
  );
}

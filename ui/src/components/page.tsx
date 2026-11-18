import { Fragment, type ReactNode } from "react";
import { Link } from "react-router";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";
import { Tabs as ShadcnTabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { t } from "@/i18n/t";
import { cn } from "@/lib/utils";

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
    <div
      className={cn(
        "-mx-4 -mt-4 mb-6 border-b bg-background px-4 py-4 md:-mx-6 md:-mt-6 md:px-6",
        sticky && "sticky top-14 z-10",
      )}
    >
      {crumbs && crumbs.length > 0 ? (
        <Breadcrumb aria-label={t("nav.breadcrumb")} className="mb-1.5">
          <BreadcrumbList>
            {crumbs.map((crumb, index) => (
              <Fragment key={crumb.label}>
                {index > 0 ? <BreadcrumbSeparator /> : null}
                <BreadcrumbItem>
                  {crumb.to ? (
                    <BreadcrumbLink asChild>
                      <Link to={crumb.to}>{crumb.label}</Link>
                    </BreadcrumbLink>
                  ) : (
                    <BreadcrumbPage>{crumb.label}</BreadcrumbPage>
                  )}
                </BreadcrumbItem>
              </Fragment>
            ))}
          </BreadcrumbList>
        </Breadcrumb>
      ) : null}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <h1 className="min-w-0 truncate text-xl font-semibold tracking-tight">{title}</h1>
        {badges}
        <div className="flex flex-1 flex-wrap items-center justify-end gap-2">{actions}</div>
      </div>
    </div>
  );
}

/**
 * shadcn/ui's Tabs (line variant) whose selection is part of the URL: each tab
 * is a link, so Back returns to the tab before and a tab can be shared.
 */
export function Tabs({ tabs, current }: { tabs: { id: string; label: string; to: string }[]; current: string }) {
  return (
    <ShadcnTabs value={current} activationMode="manual" className="mb-4">
      <TabsList variant="line" className="w-full justify-start border-b">
        {tabs.map((tab) => (
          <TabsTrigger key={tab.id} value={tab.id} asChild className="flex-none px-3">
            <Link to={tab.to}>{tab.label}</Link>
          </TabsTrigger>
        ))}
      </TabsList>
    </ShadcnTabs>
  );
}

export function Section({ title, children, aside }: { title: string; children: ReactNode; aside?: ReactNode }) {
  return (
    <section className="space-y-2">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-base font-semibold">{title}</h2>
        {aside}
      </div>
      {children}
    </section>
  );
}

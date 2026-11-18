import { Inbox, LogOut, Menu, Search, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Link, NavLink, Outlet, useLocation, useNavigate } from "react-router";
import { useApp, useCompany } from "@/app/context";
import { PageActionsProvider } from "@/app/page-actions";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { COMPANY, canEntity, canRun, findCommand, navigation, reportTitle } from "@/lib/meta";
import { NO_COMPANY, pathInCompany, paths } from "@/lib/paths";
import { CommandPalette } from "./command-palette";
import { Lookup } from "./lookup";

function NavItem({ to, label, onNavigate }: { to: string; label: string; onNavigate: () => void }) {
  return (
    <NavLink
      to={to}
      onClick={onNavigate}
      className={({ isActive }) =>
        cn(
          "block truncate rounded-md px-2 py-1 text-sm hover:bg-accent hover:text-accent-foreground",
          isActive ? "bg-accent font-medium text-accent-foreground" : "text-foreground/80",
        )
      }
    >
      {label}
    </NavLink>
  );
}

/** The frame around every screen: the menu, the current company, the user. */
export function AppShell() {
  const { meta, me, companies, signOut } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const location = useLocation();
  const groups = useMemo(() => navigation(meta), [meta]);
  const [navOpen, setNavOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteOpen((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const closeNav = (): void => setNavOpen(false);

  return (
    <PageActionsProvider>
      <div className="flex min-h-screen">
        <aside
          className={cn(
            "fixed inset-y-0 start-0 z-30 w-sidebar flex-col border-e bg-sidebar lg:sticky lg:top-0 lg:flex lg:h-screen",
            navOpen ? "flex" : "hidden",
          )}
        >
          <div className="flex h-topbar shrink-0 items-center justify-between border-b px-3">
            <Link to={paths.home(code)} onClick={closeNav} className="text-sm font-semibold tracking-tight">
              {t("app.name")}
            </Link>
            <Button variant="ghost" size="icon" className="lg:hidden" onClick={closeNav} aria-label={t("nav.close")}>
              <X aria-hidden="true" />
            </Button>
          </div>
          <nav aria-label={t("nav.label")} className="min-h-0 flex-1 space-y-4 overflow-y-auto p-2">
            {groups.map((group) => (
              <section key={group.module} aria-label={group.label}>
                <h2 className="px-2 pb-1 text-xs font-medium text-muted-foreground">{group.label}</h2>
                <ul className="space-y-px">
                  {group.reports.map((report) => (
                    <li key={report.name}>
                      <NavItem to={paths.report(code, report.name)} label={reportTitle(report)} onNavigate={closeNav} />
                    </li>
                  ))}
                  {group.docTypes.map((docType) => (
                    <li key={docType.name}>
                      <NavItem to={paths.docList(code, docType.name)} label={docType.plural} onNavigate={closeNav} />
                    </li>
                  ))}
                  {group.entities.map((entity) => (
                    <li key={entity.name}>
                      <NavItem to={paths.entityList(code, entity.name)} label={entity.plural} onNavigate={closeNav} />
                    </li>
                  ))}
                </ul>
              </section>
            ))}
          </nav>
        </aside>
        {navOpen ? (
          <button
            type="button"
            aria-label={t("nav.close")}
            className="fixed inset-0 z-20 bg-foreground/30 lg:hidden"
            onClick={closeNav}
          />
        ) : null}

        <div className="flex min-w-0 flex-1 flex-col">
          <header className="sticky top-0 z-20 flex h-topbar shrink-0 items-center gap-2 border-b bg-background px-3">
            <Button
              variant="ghost"
              size="icon"
              className="lg:hidden"
              onClick={() => setNavOpen(true)}
              aria-label={t("nav.open")}
            >
              <Menu aria-hidden="true" />
            </Button>
            <label htmlFor="company-switcher" className="hidden text-xs text-muted-foreground sm:block">
              {t("company.label")}
            </label>
            <Lookup
              id="company-switcher"
              aria-label={t("company.switch")}
              className="w-80 max-w-[45vw]"
              entity={COMPANY}
              keyField="code"
              value={company ? company.code : null}
              required
              recordLink={false}
              allowCreate={false}
              onChange={(next) => {
                if (next && next !== code) navigate(pathInCompany(location.pathname, next));
              }}
            />
            <div className="flex-1" />
            {canRun(meta, "approval.list") ? (
              <Button asChild variant="ghost" size="sm">
                <NavLink to={paths.approvals(code)}>
                  <Inbox aria-hidden="true" />
                  <span className="hidden md:inline">{findCommand(meta, "approval.list")?.label}</span>
                </NavLink>
              </Button>
            ) : null}
            <Button variant="outline" className="gap-2 text-muted-foreground" onClick={() => setPaletteOpen(true)}>
              <Search aria-hidden="true" />
              <span className="hidden md:inline">{t("palette.open")}</span>
              <kbd className="hidden rounded-sm border bg-muted px-1 font-mono text-xs md:inline">
                {t("palette.shortcut")}
              </kbd>
            </Button>
            <span className="hidden max-w-40 truncate text-sm sm:block" title={me.email}>
              {me.name}
            </span>
            <Button
              variant="ghost"
              size="icon"
              onClick={signOut}
              aria-label={t("auth.sign_out")}
              title={t("auth.sign_out")}
            >
              <LogOut aria-hidden="true" />
            </Button>
          </header>

          <main className="min-w-0 flex-1 p-4">
            {company === null ? (
              <Alert
                tone="info"
                className="mb-4"
                title={companies.length === 0 ? t("company.none_title") : t("company.unknown_title", { code })}
                actions={
                  companies.length === 0 ? (
                    canRun(meta, "company.setup") ? (
                      <Button asChild variant="outline" size="sm">
                        <Link to={paths.setup(NO_COMPANY)}>{t("setup.new_company")}</Link>
                      </Button>
                    ) : canEntity(meta, COMPANY, "create") ? (
                      <Button asChild variant="outline" size="sm">
                        <Link to={paths.entityNew(NO_COMPANY, COMPANY)}>{t("company.create")}</Link>
                      </Button>
                    ) : undefined
                  ) : (
                    <Button asChild variant="outline" size="sm">
                      <Link to={paths.home(companies[0]?.code ?? NO_COMPANY)}>
                        {t("company.go_to", { name: companies[0]?.name ?? "" })}
                      </Link>
                    </Button>
                  )
                }
              >
                {companies.length === 0 ? t("company.none_body") : t("company.unknown_body")}
              </Alert>
            ) : null}
            <Outlet />
          </main>
        </div>
        <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
      </div>
    </PageActionsProvider>
  );
}

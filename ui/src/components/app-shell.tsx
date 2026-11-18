import { Inbox, LogOut, Search } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Link, NavLink, Outlet, useLocation, useNavigate } from "react-router";
import { useApp, useCompany } from "@/app/context";
import { PageActionsProvider } from "@/app/page-actions";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Kbd } from "@/components/ui/kbd";
import { Separator } from "@/components/ui/separator";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar";
import { t } from "@/i18n/t";
import { COMPANY, canEntity, canRun, findCommand, navigation, reportTitle } from "@/lib/meta";
import { NO_COMPANY, pathInCompany, paths } from "@/lib/paths";
import { CommandPalette } from "./command-palette";
import { Lookup } from "./lookup";

function NavItem({ to, label }: { to: string; label: string }) {
  const { pathname } = useLocation();
  const { setOpenMobile } = useSidebar();
  const active = pathname === to || pathname.startsWith(`${to}/`);
  return (
    <SidebarMenuItem>
      <SidebarMenuButton asChild isActive={active}>
        <NavLink to={to} onClick={() => setOpenMobile(false)}>
          <span>{label}</span>
        </NavLink>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}

/** The menu: one group per module, listing only what the user may read. */
function AppSidebar() {
  const { meta } = useApp();
  const { code } = useCompany();
  const { setOpenMobile } = useSidebar();
  const groups = useMemo(() => navigation(meta), [meta]);
  return (
    <Sidebar>
      <SidebarHeader className="h-14 justify-center border-b px-4">
        <Link
          to={paths.home(code)}
          onClick={() => setOpenMobile(false)}
          className="text-base font-semibold tracking-tight"
        >
          {t("app.name")}
        </Link>
      </SidebarHeader>
      <SidebarContent>
        <nav aria-label={t("nav.label")}>
          {groups.map((group) => (
            <SidebarGroup key={group.module} aria-label={group.label}>
              <SidebarGroupLabel>{group.label}</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {group.reports.map((report) => (
                    <NavItem key={report.name} to={paths.report(code, report.name)} label={reportTitle(report)} />
                  ))}
                  {group.docTypes.map((docType) => (
                    <NavItem key={docType.name} to={paths.docList(code, docType.name)} label={docType.plural} />
                  ))}
                  {group.entities.map((entity) => (
                    <NavItem key={entity.name} to={paths.entityList(code, entity.name)} label={entity.plural} />
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          ))}
        </nav>
      </SidebarContent>
    </Sidebar>
  );
}

/** The frame around every screen: shadcn/ui's sidebar block with the menu, a header with the current company and the user. */
export function AppShell() {
  const { meta, me, companies, signOut } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const location = useLocation();
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

  return (
    <PageActionsProvider>
      <SidebarProvider>
        <AppSidebar />
        <SidebarInset className="min-w-0">
          <header className="sticky top-0 z-20 flex h-14 shrink-0 items-center gap-2 border-b bg-background px-4">
            <SidebarTrigger className="-ms-1" aria-label={t("nav.toggle")} />
            <Separator orientation="vertical" className="me-1 data-[orientation=vertical]:h-4" />
            <label htmlFor="company-switcher" className="hidden text-sm text-muted-foreground sm:block">
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
              <Button asChild variant="ghost">
                <NavLink to={paths.approvals(code)}>
                  <Inbox aria-hidden="true" />
                  <span className="hidden md:inline">{findCommand(meta, "approval.list")?.label}</span>
                </NavLink>
              </Button>
            ) : null}
            <Button variant="outline" className="text-muted-foreground" onClick={() => setPaletteOpen(true)}>
              <Search aria-hidden="true" />
              <span className="hidden md:inline">{t("palette.open")}</span>
              <Kbd className="hidden md:inline-flex">{t("palette.shortcut")}</Kbd>
            </Button>
            <span className="hidden max-w-40 truncate text-sm font-medium sm:block" title={me.email}>
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

          <div className="min-w-0 flex-1 p-4 md:p-6">
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
          </div>
        </SidebarInset>
        <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
      </SidebarProvider>
    </PageActionsProvider>
  );
}

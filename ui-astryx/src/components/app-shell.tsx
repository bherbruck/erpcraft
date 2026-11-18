import { AppShell as AstryxShell } from "@astryxdesign/core/AppShell";
import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Selector } from "@astryxdesign/core/Selector";
import { SideNav, SideNavHeading, SideNavItem, SideNavSection } from "@astryxdesign/core/SideNav";
import { Text } from "@astryxdesign/core/Text";
import { TopNav } from "@astryxdesign/core/TopNav";
import { Building2, Inbox, Search } from "lucide-react";
import { Icon } from "@astryxdesign/core/Icon";
import { Badge } from "@astryxdesign/core/Badge";
import { lazy, Suspense, useEffect, useMemo, useState } from "react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { canRun, navigation, reportTitle } from "@/lib/meta";
import { pathInCompany, paths } from "@/lib/paths";

/**
 * The frame of every signed-in screen. The menu is the registry's: every module's
 * document types and entities the user may read, so a new module appears here
 * without UI code. The current company is always visible and is part of the URL.
 */
// The palette is loaded the first time it is opened.
const CommandPalette = lazy(async () => ({ default: (await import("./command-palette")).CommandPalette }));

export function AppShell() {
  const { meta, me, companies, signOut } = useApp();
  const { code, company } = useCompany();
  const location = useLocation();
  const navigate = useNavigate();
  const groups = useMemo(() => navigation(meta), [meta]);
  const here = location.pathname;
  const isAt = (path: string) => here === path || here.startsWith(`${path}/`);
  const [palette, setPalette] = useState(false);
  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPalette((open) => !open);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const companyPicker = (
    <Selector
      label={t("company.label")}
      value={company?.code ?? ""}
      onChange={(next: string) => navigate(pathInCompany(here, next))}
      hasSearch={companies.length > 8}
      options={companies.map((c) => ({ value: c.code, label: c.name, description: c.code }))}
      startIcon={Building2}
      size="sm"
      width="100%"
    />
  );

  const sideNav = (
    <SideNav
      header={<SideNavHeading heading={t("app.name")} subheading={t("spike.subtitle")} headingHref={paths.home(code)} />}
      topContent={companyPicker}
      resizable={{ autoSaveId: "astryx-spike.sidenav" }}
      collapsible
    >
      {canRun(meta, "approval.list") ? (
        <SideNavSection title={t("nav.approvals")} isHeaderHidden>
          <SideNavItem label={t("nav.approvals")} icon={Inbox} href={paths.approvals(code)} isSelected={isAt(paths.approvals(code))} size="sm" />
        </SideNavSection>
      ) : null}
      {groups.map((group) => (
        <SideNavSection key={group.module} title={group.label}>
          {group.docTypes.map((docType) => (
            <SideNavItem
              key={`d.${docType.name}`}
              label={docType.plural}
              href={paths.docList(code, docType.name)}
              isSelected={isAt(paths.docList(code, docType.name))}
              size="sm"
            />
          ))}
          {group.reports.map((report) => (
            <SideNavItem
              key={`r.${report.name}`}
              label={reportTitle(report)}
              href={paths.report(code, report.name)}
              isSelected={isAt(paths.report(code, report.name))}
              size="sm"
            />
          ))}
          {group.entities.map((entity) => (
            <SideNavItem
              key={`e.${entity.name}`}
              label={entity.plural}
              href={paths.entityList(code, entity.name)}
              isSelected={isAt(paths.entityList(code, entity.name))}
              size="sm"
            />
          ))}
        </SideNavSection>
      ))}
    </SideNav>
  );

  const topNav = (
    <TopNav
      label={t("nav.label")}
      // The heading slot stays in the compact bar below the breakpoint, so the company is always visible.
      heading={
        company ? (
          <Text type="label" weight="semibold">
            {company.name} <Text type="supporting">({company.code})</Text>
          </Text>
        ) : null
      }
      endContent={
        <HStack gap={2} vAlign="center">
          <Button
            label={t("palette.open")}
            variant="secondary"
            size="sm"
            icon={<Icon icon={Search} />}
            endContent={<Badge label={t("palette.shortcut")} />}
            onClick={() => setPalette(true)}
          />
          <Text type="supporting">{me.name}</Text>
          <Button label={t("auth.sign_out")} variant="ghost" size="sm" onClick={signOut} />
        </HStack>
      }
    />
  );

  return (
    <AstryxShell sideNav={sideNav} topNav={topNav} variant="section" contentPadding={0} mobileNav={{ breakpoint: "lg" }}>
      <div className="page-fill">
        <Outlet />
      </div>
      {palette ? (
        <Suspense fallback={null}>
          <CommandPalette open={palette} onOpenChange={setPalette} />
        </Suspense>
      ) : null}
    </AstryxShell>
  );
}

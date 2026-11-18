import { AppShell as AstryxShell } from "@astryxdesign/core/AppShell";
import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Selector } from "@astryxdesign/core/Selector";
import { SideNav, SideNavHeading, SideNavItem, SideNavSection } from "@astryxdesign/core/SideNav";
import { Text } from "@astryxdesign/core/Text";
import { TopNav } from "@astryxdesign/core/TopNav";
import { Building2 } from "lucide-react";
import { useMemo } from "react";
import { Outlet, useLocation, useNavigate } from "react-router";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { navigation } from "@/lib/meta";
import { pathInCompany, paths } from "@/lib/paths";

/**
 * The frame of every signed-in screen. The menu is the registry's: every module's
 * document types and entities the user may read, so a new module appears here
 * without UI code. The current company is always visible and is part of the URL.
 */
export function AppShell() {
  const { meta, me, companies, signOut } = useApp();
  const { code, company } = useCompany();
  const location = useLocation();
  const navigate = useNavigate();
  const groups = useMemo(() => navigation(meta), [meta]);
  const here = location.pathname;
  const isAt = (path: string) => here === path || here.startsWith(`${path}/`);

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
    </AstryxShell>
  );
}

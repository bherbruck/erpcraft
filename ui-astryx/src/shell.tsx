import { AppShell } from "@astryxdesign/core/AppShell";
import { Button } from "@astryxdesign/core/Button";
import { HStack } from "@astryxdesign/core/HStack";
import { Selector } from "@astryxdesign/core/Selector";
import { SideNav, SideNavHeading, SideNavItem, SideNavSection } from "@astryxdesign/core/SideNav";
import { Text } from "@astryxdesign/core/Text";
import { TopNav } from "@astryxdesign/core/TopNav";
import { Building2, ShoppingCart, Users } from "lucide-react";
import { Navigate, Outlet, useLocation, useNavigate, useParams } from "react-router";
import { t } from "@/i18n/t";
import { paths } from "@/lib/paths";
import { useSession } from "./session";

/** The app frame: side navigation, the current company (always visible, part of the URL) and the user. */
export function Shell() {
  const { company: code = "" } = useParams();
  const { companies, me, signOut } = useSession();
  const location = useLocation();
  const navigate = useNavigate();
  const company = companies.find((c) => c.code === code);
  if (!company) {
    const first = companies[0];
    return first ? <Navigate to={paths.salesOrders(first.code)} replace /> : null;
  }

  const here = location.pathname;
  const switchCompany = (next: string) => {
    // Keep the screen, change the company; a record of another company would not belong there.
    const section = here.includes("/customers") ? paths.customers(next) : paths.salesOrders(next);
    navigate(section);
  };

  const companyPicker = (
    <Selector
      label={t("nav.company")}
      value={company.code}
      onChange={switchCompany}
      hasSearch={companies.length > 8}
      options={companies.map((c) => ({ value: c.code, label: c.name, description: c.code }))}
      startIcon={Building2}
      size="sm"
      width="100%"
    />
  );

  const sideNav = (
    <SideNav header={<SideNavHeading heading={t("app.name")} subheading={t("app.spike")} />} topContent={companyPicker}>
      <SideNavSection title={t("nav.sales")}>
        <SideNavItem
          label={t("nav.salesOrders")}
          icon={ShoppingCart}
          href={paths.salesOrders(company.code)}
          isSelected={here.startsWith(paths.salesOrders(company.code))}
        />
      </SideNavSection>
      <SideNavSection title={t("nav.masterData")}>
        <SideNavItem
          label={t("nav.customers")}
          icon={Users}
          href={paths.customers(company.code)}
          isSelected={here.startsWith(paths.customers(company.code))}
        />
      </SideNavSection>
    </SideNav>
  );

  const topNav = (
    <TopNav
      label={t("app.name")}
      startContent={
        <Text type="label" weight="semibold">
          {company.name} <Text type="supporting">({company.code})</Text>
        </Text>
      }
      endContent={
        <HStack gap={2} vAlign="center">
          <Text type="supporting">{t("nav.signedInAs", { name: me.name })}</Text>
          <Button label={t("nav.signOut")} variant="ghost" size="sm" clickAction={signOut} />
        </HStack>
      }
    />
  );

  return (
    <AppShell sideNav={sideNav} topNav={topNav} variant="section" contentPadding={0}>
      {/* Pages use Layout height="fill"; this gives it the content area's height so list footers stay in view. */}
      <div className="page-fill">
        <Outlet />
      </div>
    </AppShell>
  );
}

import { useMemo } from "react";
import { Link } from "react-router";
import { useApp, useCompany } from "@/app/context";
import { PageHeader } from "@/components/page";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { t } from "@/i18n/t";
import { navigation, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";

const linkClasses = "block truncate rounded-md px-2 py-1.5 text-sm hover:bg-accent hover:text-accent-foreground";

/** Every screen the user can open, by module. The same list as the menu, laid out as a page. */
export function HomePage() {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const groups = useMemo(() => navigation(meta), [meta]);
  return (
    <>
      <PageHeader title={company ? company.name : t("app.name")} />
      <p className="mb-4 text-sm text-muted-foreground">{t("home.hint")}</p>
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        {groups.map((group) => (
          <Card key={group.module} className="gap-2 py-4">
            <CardHeader className="px-4">
              <CardTitle className="text-sm text-muted-foreground">
                <h2>{group.label}</h2>
              </CardTitle>
            </CardHeader>
            <CardContent className="px-2">
              <ul>
                {group.reports.map((report) => (
                  <li key={report.name}>
                    <Link className={linkClasses} to={paths.report(code, report.name)}>
                      {reportTitle(report)}
                    </Link>
                  </li>
                ))}
                {group.docTypes.map((docType) => (
                  <li key={docType.name}>
                    <Link className={linkClasses} to={paths.docList(code, docType.name)}>
                      {docType.plural}
                    </Link>
                  </li>
                ))}
                {group.entities.map((entity) => (
                  <li key={entity.name}>
                    <Link className={linkClasses} to={paths.entityList(code, entity.name)}>
                      {entity.plural}
                    </Link>
                  </li>
                ))}
              </ul>
            </CardContent>
          </Card>
        ))}
      </div>
    </>
  );
}

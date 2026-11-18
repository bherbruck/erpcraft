import { Printer } from "lucide-react";
import { useState } from "react";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { PageHeader } from "@/components/page";
import { PrintFrame, printHtml } from "@/components/print-frame";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { quote } from "@/lib/link-labels";
import { canDoc, canEntity, canRun, findDocType, PRINT_TEMPLATE } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

/** The templates a document of this type can print with: those of its company and those for all companies. */
export function usePrintTemplates(docType: string, companyId: string | null) {
  const { meta } = useApp();
  const readable = canEntity(meta, PRINT_TEMPLATE, "read");
  return useAsync(readable && companyId ? `templates|${docType}|${companyId}` : null, async (signal) => {
    const page = await api.run(
      "entity.list",
      {
        entity: PRINT_TEMPLATE,
        active_only: true,
        where: `record.doc_type == ${quote(docType)} && (record.company_id == null || record.company_id == ${quote(companyId ?? "")})`,
        sort: ["name"],
        limit: 100,
      },
      { signal },
    );
    return page.items;
  });
}

export function PrintPage() {
  const { meta } = useApp();
  const params = useParams();
  const docType = findDocType(meta, params.docType);
  if (!docType || !params.id || !canDoc(meta, docType.name, "read") || !canRun(meta, "document.render"))
    return <NotFoundPage />;
  return <PrintView docType={docType} id={params.id} />;
}

function PrintView({ docType, id }: { docType: DocTypeMeta; id: string }) {
  const { code, company } = useCompany();
  const url = useUrlState();
  const templateId = url.get("template");
  const [blocked, setBlocked] = useState(false);
  const templates = usePrintTemplates(docType.name, company?.id ?? null);
  const page = useAsync(`${docType.name}|${id}|${templateId}`, (signal) =>
    api.run("document.render", { doc_type: docType.name, id, template_id: templateId || null }, { signal }),
  );
  const options: EntityRecord[] = templates.data ?? [];
  const title = page.data?.title ?? docType.label;

  return (
    <>
      <PageHeader
        crumbs={[
          { label: docType.plural, to: paths.docList(code, docType.name) },
          { label: page.data?.title ?? t("print.back"), to: paths.doc(code, docType.name, id) },
        ]}
        title={page.data ? title : <Skeleton className="h-6 w-56" />}
        actions={
          <>
            {options.length > 0 ? (
              <div className="flex items-center gap-2">
                <Label htmlFor="print-template">{t("print.template")}</Label>
                <NativeSelect
                  id="print-template"
                  className="w-56"
                  value={templateId}
                  onChange={(event) => url.set({ template: event.target.value }, { replace: true })}
                >
                  <option value="">{t("print.template_default")}</option>
                  {options.map((template) => (
                    <option key={template.id} value={template.id}>
                      {typeof template.name === "string" ? template.name : template.id}
                    </option>
                  ))}
                </NativeSelect>
              </div>
            ) : null}
            <Button
              disabled={!page.data}
              onClick={() => {
                if (page.data) setBlocked(!printHtml(page.data.html));
              }}
            >
              <Printer aria-hidden="true" />
              {t("print.print")}
            </Button>
          </>
        }
      />
      {blocked ? <Alert title={t("print.blocked_title")}>{t("print.blocked")}</Alert> : null}
      {page.error ? (
        <Alert
          title={t("print.failed")}
          actions={
            <Button variant="outline" size="sm" onClick={page.reload}>
              {t("action.retry")}
            </Button>
          }
        >
          {page.error.message}
        </Alert>
      ) : page.data ? (
        <PrintFrame html={page.data.html} title={title} className="h-print" />
      ) : (
        <Skeleton className="h-print w-full" />
      )}
    </>
  );
}

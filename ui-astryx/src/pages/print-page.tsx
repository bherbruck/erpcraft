import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Icon } from "@astryxdesign/core/Icon";
import { Selector } from "@astryxdesign/core/Selector";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { VStack } from "@astryxdesign/core/VStack";
import { Printer } from "lucide-react";
import { useState } from "react";
import { useParams } from "react-router";
import { api } from "@/api/client";
import type { DocTypeMeta, EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Page } from "@/components/page";
import { PrintFrame, printHtml } from "@/components/print-frame";
import { t } from "@/i18n/t";
import { quote } from "@/lib/link-labels";
import { canDoc, canEntity, canRun, findDocType, PRINT_TEMPLATE } from "@/lib/meta";
import { useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

/** The templates a document of this type can print with: its company's and those for all companies. */
function usePrintTemplates(docType: string, companyId: string | null) {
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

export default function PrintPage() {
  const { meta } = useApp();
  const params = useParams();
  const docType = findDocType(meta, params.docType);
  if (!docType || !params.id || !canDoc(meta, docType.name, "read") || !canRun(meta, "document.render")) return <NotFoundPage />;
  return <PrintView docType={docType} id={params.id} />;
}

function PrintView({ docType, id }: { docType: DocTypeMeta; id: string }) {
  const { company } = useCompany();
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
    <Page
      back
      kicker={docType.label}
      title={title}
      actions={
        <>
          {options.length > 0 ? (
            <Selector
              label={t("print.template")}
              isLabelHidden
              size="md"
              value={templateId}
              hasClear={templateId !== ""}
              placeholder={t("print.template_default")}
              onChange={(next: string | null) => url.set({ template: next ?? null }, { replace: true })}
              options={options.map((template) => ({ value: template.id, label: typeof template.name === "string" ? template.name : template.id }))}
            />
          ) : null}
          <Button
            label={t("print.print")}
            variant="primary"
            icon={<Icon icon={Printer} />}
            isDisabled={!page.data}
            onClick={() => {
              if (page.data) setBlocked(!printHtml(page.data.html));
            }}
          />
        </>
      }
    >
      <VStack gap={3} height="100%">
        {blocked ? <Banner status="warning" title={t("print.blocked_title")} description={t("print.blocked")} /> : null}
        {page.error ? (
          <Banner status="error" title={t("print.failed")} description={page.error.message} endContent={<Button label={t("action.retry")} size="sm" onClick={page.reload} />} />
        ) : page.data ? (
          <PrintFrame html={page.data.html} title={title} />
        ) : (
          <Skeleton height="70vh" />
        )}
      </VStack>
    </Page>
  );
}

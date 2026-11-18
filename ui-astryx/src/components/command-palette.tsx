// Ctrl+K: go to any screen, find any record or document, create anything, switch company.
// Astryx's CommandPalette with one search source: the registry's screens filtered here, and
// records and documents searched on the server (`search.records`), grouped by kind.

import { CommandPalette as AstryxPalette } from "@astryxdesign/core/CommandPalette";
import { Text } from "@astryxdesign/core/Text";
import type { SearchableItem, SearchSource } from "@astryxdesign/core/Typeahead";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo } from "react";
import { useNavigate } from "react-router";
import { api } from "@/api/client";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { canDoc, canEntity, canRun, findDocType, navigation, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";

interface Aux {
  group: string;
  to: string;
  detail?: string;
}
type Item = SearchableItem<Aux>;

export function CommandPalette({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const { meta, companies } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();

  const screens = useMemo<Item[]>(() => {
    const items: Item[] = [];
    const go = t("palette.go_to");
    const add = (id: string, label: string, group: string, to: string) => items.push({ id, label, auxiliaryData: { group, to } });
    if (canRun(meta, "approval.list")) add("approvals", t("nav.approvals"), go, paths.approvals(code));
    for (const group of navigation(meta)) {
      for (const docType of group.docTypes) add(`d:${docType.name}`, docType.plural, go, paths.docList(code, docType.name));
      for (const report of group.reports) add(`r:${report.name}`, reportTitle(report), go, paths.report(code, report.name));
      for (const entity of group.entities) add(`e:${entity.name}`, entity.plural, go, paths.entityList(code, entity.name));
    }
    for (const docType of meta.doc_types)
      if (canDoc(meta, docType.name, "create")) add(`nd:${docType.name}`, t("palette.new", { label: docType.label }), t("palette.create"), paths.docNew(code, docType.name));
    for (const entity of meta.entities)
      if (!entity.parent && !entity.read_only && canEntity(meta, entity.name, "create"))
        add(`ne:${entity.name}`, t("palette.new", { label: entity.label }), t("palette.create"), paths.entityNew(code, entity.name));
    for (const other of companies)
      if (other.code !== code) add(`c:${other.id}`, `${other.code} · ${other.name}`, t("palette.switch_company"), paths.home(other.code));
    return items;
  }, [meta, companies, code]);

  const source = useMemo<SearchSource<Item>>(() => {
    let controller: AbortController | null = null;
    return {
      bootstrap: () => screens.filter((item) => item.auxiliaryData?.group === t("palette.go_to")).slice(0, 12),
      search: async (query, context) => {
        const needle = query.trim().toLowerCase();
        const local = screens.filter((item) => `${item.label} ${item.auxiliaryData?.group ?? ""}`.toLowerCase().includes(needle)).slice(0, 12);
        // Screens show at once; records follow when the server answers.
        context?.update(local);
        if (needle.length < 2) return local;
        controller?.abort();
        controller = new AbortController();
        try {
          const result = await api.run("search.records", { q: query.trim(), company_id: company?.id ?? null, limit: 5 }, { signal: controller.signal });
          const found = result.groups.flatMap((group) =>
            group.items.map((hit): Item => {
              const docType = group.kind === "document" ? findDocType(meta, group.name) : undefined;
              const detail = [
                hit.status ? (docType?.states.find((s) => s.name === hit.status)?.label ?? hit.status) : null,
                hit.doc_date ? formatCalendarDate(hit.doc_date) : null,
              ]
                .filter(Boolean)
                .join(" · ");
              return {
                id: `${group.kind}:${group.name}:${hit.id}`,
                label: [hit.title, hit.subtitle].filter(Boolean).join(" · "),
                auxiliaryData: {
                  group: group.label,
                  to: group.kind === "document" ? paths.doc(code, group.name, hit.id) : paths.entityRecord(code, group.name, hit.id),
                  detail: detail || undefined,
                },
              };
            }),
          );
          return [...local, ...found];
        } catch {
          return local;
        }
      },
      cancel: () => controller?.abort(),
    };
  }, [screens, meta, code, company?.id]);

  return (
    <AstryxPalette<Item>
      isOpen={open}
      onOpenChange={onOpenChange}
      label={t("palette.title")}
      searchSource={source}
      emptySearchText={t("spike.no_results")}
      onValueChange={(id) => {
        const to = (screens.find((item) => item.id === id) ?? lastFound.get(id))?.auxiliaryData?.to;
        if (to) {
          onOpenChange(false);
          navigate(to);
        }
      }}
      renderItem={(item) => {
        if (item.auxiliaryData) lastFound.set(item.id, item);
        return (
          <VStack gap={0}>
            <Text maxLines={1}>{item.label}</Text>
            {item.auxiliaryData?.detail ? <Text type="supporting">{item.auxiliaryData.detail}</Text> : null}
          </VStack>
        );
      }}
    />
  );
}

/** Records found by the server, by id, so choosing one knows where it leads. */
const lastFound = new Map<string, Item>();

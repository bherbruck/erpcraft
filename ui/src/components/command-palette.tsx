import { useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { api } from "@/api/client";
import { useApp, useCompany } from "@/app/context";
import { useCurrentPageActions } from "@/app/page-actions";
import { Button } from "@/components/button";
import { Command, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
// The palette is not a form: it is shadcn's command dialog, composed here so that Escape can clear the search first.
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { canDoc, canEntity, canRun, createCommand, findDocType, navigation, reportTitle } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";

interface PaletteItem {
  id: string;
  group: string;
  label: string;
  detail?: string;
  to?: string;
  run?: () => void;
  destructive?: boolean;
}

const PER_SOURCE = 5;

/**
 * Ctrl+K: go to any screen, start any record, find any record.
 * Built from the same registry as the menu, so it offers only what the user may open.
 */
export function CommandPalette({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const { meta, companies } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const pageActions = useCurrentPageActions();
  const [query, setQuery] = useState("");
  const [settled, setSettled] = useState("");
  const [active, setActive] = useState("");

  useEffect(() => {
    const timer = setTimeout(() => setSettled(query.trim()), 250);
    return () => clearTimeout(timer);
  }, [query]);

  useEffect(() => {
    if (!open) setQuery("");
  }, [open]);

  const screens = useMemo<PaletteItem[]>(() => {
    const items: PaletteItem[] = [];
    for (const group of navigation(meta)) {
      for (const report of group.reports) {
        items.push({
          id: `r:${report.name}`,
          group: t("palette.go_to"),
          label: reportTitle(report),
          to: paths.report(code, report.name),
        });
      }
      for (const docType of group.docTypes) {
        items.push({
          id: `d:${docType.name}`,
          group: t("palette.go_to"),
          label: docType.plural,
          to: paths.docList(code, docType.name),
        });
      }
      for (const entity of group.entities) {
        items.push({
          id: `e:${entity.name}`,
          group: t("palette.go_to"),
          label: entity.plural,
          to: paths.entityList(code, entity.name),
        });
      }
    }
    for (const docType of meta.doc_types) {
      if (!canDoc(meta, docType.name, "create")) continue;
      items.push({
        id: `nd:${docType.name}`,
        group: t("palette.create"),
        label: t("palette.new", { label: docType.label }),
        to: paths.docNew(code, docType.name),
      });
    }
    for (const entity of meta.entities) {
      const creatable = entity.read_only
        ? createCommand(meta, entity.name) !== undefined
        : canEntity(meta, entity.name, "create");
      if (entity.parent || !creatable) continue;
      items.push({
        id: `ne:${entity.name}`,
        group: t("palette.create"),
        label: t("palette.new", { label: entity.label }),
        to: paths.entityNew(code, entity.name),
      });
    }
    if (canRun(meta, "company.setup")) {
      items.push({ id: "setup", group: t("palette.create"), label: t("setup.new_company"), to: paths.setup(code) });
    }
    for (const other of companies) {
      if (other.code === code) continue;
      items.push({
        id: `c:${other.id}`,
        group: t("palette.switch_company"),
        label: `${other.code} · ${other.name}`,
        to: paths.home(other.code),
      });
    }
    return items;
  }, [meta, companies, code]);

  // Records and documents are searched on the server in one request, grouped by kind.
  const found = useAsync(open && settled.length >= 2 ? `${code}|${settled}` : null, async (signal) => {
    const result = await api.run(
      "search.records",
      { q: settled, company_id: company?.id ?? null, limit: PER_SOURCE },
      { signal },
    );
    return result.groups.flatMap((group) =>
      group.items.map(
        (hit): PaletteItem => ({
          id: `${group.kind}:${group.name}:${hit.id}`,
          group: group.label,
          label: [hit.title, hit.subtitle].filter(Boolean).join(" · "),
          detail:
            group.kind === "document"
              ? [
                  hit.status
                    ? (findDocType(meta, group.name)?.states.find((s) => s.name === hit.status)?.label ?? hit.status)
                    : null,
                  hit.doc_date ? formatCalendarDate(hit.doc_date) : null,
                ]
                  .filter(Boolean)
                  .join(" · ")
              : undefined,
          to:
            group.kind === "document"
              ? paths.doc(code, group.name, hit.id)
              : paths.entityRecord(code, group.name, hit.id),
        }),
      ),
    );
  });

  const needle = query.trim().toLowerCase();
  const items = useMemo(() => {
    const matching = needle
      ? screens.filter((item) => `${item.label} ${item.group}`.toLowerCase().includes(needle))
      : screens;
    // The actions of the record on screen come first: they are what the user is most likely after.
    const actions: PaletteItem[] = (pageActions?.actions ?? []).map((action) => ({
      id: `a:${action.id}`,
      group: t("palette.actions_on", { subject: pageActions?.subject ?? "" }),
      label: action.label,
      run: action.run,
      destructive: action.destructive,
    }));
    const matchingActions = needle ? actions.filter((item) => item.label.toLowerCase().includes(needle)) : actions;
    return [...matchingActions, ...matching, ...(needle.length >= 2 ? (found.data ?? []) : [])];
  }, [screens, found.data, needle, pageActions]);

  // A new list starts at its first entry.
  const first = items[0]?.id ?? "";
  // biome-ignore lint/correctness/useExhaustiveDependencies: only a new list moves the highlight
  useEffect(() => setActive(first), [needle, found.data]);

  function go(item: PaletteItem): void {
    onOpenChange(false);
    if (item.run) item.run();
    else if (item.to) navigate(item.to);
  }

  // Consecutive items of one group are shown under its heading.
  const groups: { heading: string; items: PaletteItem[] }[] = [];
  for (const item of items) {
    const last = groups[groups.length - 1];
    if (last && last.heading === item.group) last.items.push(item);
    else groups.push({ heading: item.group, items: [item] });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        showCloseButton={false}
        aria-describedby={undefined}
        onInteractOutside={(event) => event.preventDefault()}
        onEscapeKeyDown={(event) => {
          // Escape first clears what was typed, then closes.
          if (query !== "") {
            event.preventDefault();
            setQuery("");
          }
        }}
        className="top-[10vh] translate-y-0 gap-0 overflow-hidden p-0 sm:max-w-2xl"
      >
        <DialogTitle className="sr-only">{t("palette.title")}</DialogTitle>
        <Command shouldFilter={false} loop value={active} onValueChange={setActive}>
          <div className="flex items-center gap-1 pe-1">
            <div className="min-w-0 flex-1">
              <CommandInput
                aria-label={t("palette.title")}
                value={query}
                onValueChange={setQuery}
                placeholder={t("palette.placeholder")}
              />
            </div>
            <Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
              {t("action.close")}
            </Button>
          </div>
          <div id="palette-list">
            <CommandList label={t("palette.title")} className="max-h-[60vh]">
              {groups.map((group, index) => (
                // biome-ignore lint/suspicious/noArrayIndexKey: a heading can recur after another group
                <CommandGroup key={`${index}:${group.heading}`} heading={group.heading}>
                  {group.items.map((item) => (
                    <CommandItem
                      key={item.id}
                      value={item.id}
                      onSelect={() => go(item)}
                      className={item.destructive ? "text-destructive" : undefined}
                    >
                      <span className="min-w-0 truncate">{item.label}</span>
                      {item.detail ? (
                        <span className="ms-auto shrink-0 text-muted-foreground">{item.detail}</span>
                      ) : null}
                    </CommandItem>
                  ))}
                </CommandGroup>
              ))}
              {items.length === 0 ? (
                <p className="px-2 py-6 text-center text-sm text-muted-foreground">
                  {found.loading ? t("value.loading") : t("palette.nothing", { query: query.trim() })}
                </p>
              ) : null}
            </CommandList>
          </div>
        </Command>
      </DialogContent>
    </Dialog>
  );
}

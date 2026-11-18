import * as DialogPrimitive from "@radix-ui/react-dialog";
import { Search } from "lucide-react";
import { type KeyboardEvent, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { api } from "@/api/client";
import { useApp, useCompany } from "@/app/context";
import { useCurrentPageActions } from "@/app/page-actions";
import { Button } from "@/components/ui/button";
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
  const [active, setActive] = useState(0);

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

  // biome-ignore lint/correctness/useExhaustiveDependencies: a new list starts at its first entry
  useEffect(() => setActive(0), [needle, found.data]);

  function go(item: PaletteItem | undefined): void {
    if (!item) return;
    onOpenChange(false);
    if (item.run) item.run();
    else if (item.to) navigate(item.to);
  }

  function onKey(event: KeyboardEvent<HTMLInputElement>): void {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((n) => (items.length === 0 ? 0 : (n + 1) % items.length));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((n) => (items.length === 0 ? 0 : (n - 1 + items.length) % items.length));
    } else if (event.key === "Enter") {
      event.preventDefault();
      go(items[active]);
    }
  }

  let lastGroup = "";
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-40 bg-foreground/30" />
        <DialogPrimitive.Content
          aria-describedby={undefined}
          onInteractOutside={(event) => event.preventDefault()}
          onEscapeKeyDown={(event) => {
            // Escape first clears what was typed, then closes.
            if (query !== "") {
              event.preventDefault();
              setQuery("");
            }
          }}
          className="fixed start-1/2 top-[10vh] z-50 flex max-h-[70vh] w-[calc(100vw-2rem)] max-w-palette -translate-x-1/2 flex-col rounded-lg border bg-popover text-popover-foreground shadow-lg rtl:translate-x-1/2"
        >
          <DialogPrimitive.Title className="sr-only">{t("palette.title")}</DialogPrimitive.Title>
          <div className="flex items-center gap-2 border-b px-3">
            <Search className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <input
              role="combobox"
              aria-expanded="true"
              aria-controls="palette-list"
              aria-activedescendant={items[active] ? `palette-${items[active].id}` : undefined}
              aria-label={t("palette.title")}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={onKey}
              placeholder={t("palette.placeholder")}
              className="h-11 min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
            />
            <Button variant="ghost" size="sm" onClick={() => onOpenChange(false)}>
              {t("action.close")}
            </Button>
          </div>
          <div
            id="palette-list"
            role="listbox"
            aria-label={t("palette.title")}
            className="min-h-0 flex-1 overflow-y-auto p-1"
          >
            {items.map((item, index) => {
              const heading = item.group !== lastGroup ? item.group : null;
              lastGroup = item.group;
              return (
                <div key={item.id}>
                  {heading ? (
                    <p className="px-2 pb-1 pt-2 text-xs font-medium text-muted-foreground">{heading}</p>
                  ) : null}
                  {/* biome-ignore lint/a11y/useKeyWithClickEvents: the keyboard is handled on the input, which keeps the focus */}
                  <div
                    id={`palette-${item.id}`}
                    role="option"
                    tabIndex={-1}
                    aria-selected={index === active}
                    onMouseEnter={() => setActive(index)}
                    onClick={() => go(item)}
                    className="flex cursor-pointer items-baseline gap-2 rounded-sm px-2 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
                  >
                    <span className={item.destructive ? "min-w-0 truncate text-destructive" : "min-w-0 truncate"}>
                      {item.label}
                    </span>
                    {item.detail ? (
                      <span className="ms-auto shrink-0 text-xs text-muted-foreground">{item.detail}</span>
                    ) : null}
                  </div>
                </div>
              );
            })}
            {items.length === 0 ? (
              <p className="px-2 py-6 text-center text-sm text-muted-foreground">
                {found.loading ? t("value.loading") : t("palette.nothing", { query: query.trim() })}
              </p>
            ) : null}
          </div>
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}

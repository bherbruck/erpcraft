// Named views of a list: its search, filters, sort and columns, saved by the
// user and switched between from a menu. They are kept in this browser, per
// user and per list; keeping them on the server, to follow the user to another
// browser and to share them, is a later step.

import { Bookmark, Check, Trash2 } from "lucide-react";
import { type FormEvent, useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { useApp } from "@/app/context";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { t } from "@/i18n/t";
import { readStored, writeStored } from "@/lib/storage";

export interface SavedView {
  name: string;
  /** The query string of the list, without the page. */
  search: string;
  /** The columns shown, or null for the defaults. */
  columns: string[] | null;
}

function isViews(value: unknown): value is SavedView[] {
  return (
    Array.isArray(value) &&
    value.every(
      (view) =>
        typeof view === "object" &&
        view !== null &&
        typeof view.name === "string" &&
        typeof view.search === "string" &&
        (view.columns === null ||
          (Array.isArray(view.columns) && view.columns.every((c: unknown) => typeof c === "string"))),
    )
  );
}

/** The list's state as a view keeps it: everything in the address but the page. */
function viewSearch(search: string): string {
  const params = new URLSearchParams(search);
  params.delete("page");
  params.sort();
  return params.toString();
}

export function SavedViews({
  tableKey,
  columns,
  onColumns,
}: {
  /** Which list: `document.purchase_order`, `entity.item`. */
  tableKey: string;
  columns: string[] | null;
  onColumns: (columns: string[] | null) => void;
}) {
  const { me } = useApp();
  const location = useLocation();
  const navigate = useNavigate();
  const storageKey = `views.${me.user_id}.${tableKey}`;
  const [views, setViews] = useState<SavedView[]>(() => readStored(storageKey, isViews) ?? []);
  const [name, setName] = useState("");
  const current = viewSearch(location.search);
  const active = views.find(
    (view) => view.search === current && JSON.stringify(view.columns) === JSON.stringify(columns),
  );

  function store(next: SavedView[]): void {
    setViews(next);
    writeStored(storageKey, next.length > 0 ? next : null);
  }

  function save(event: FormEvent): void {
    event.preventDefault();
    const trimmed = name.trim();
    if (!trimmed) return;
    store(
      [...views.filter((view) => view.name !== trimmed), { name: trimmed, search: current, columns }].sort((a, b) =>
        a.name.localeCompare(b.name),
      ),
    );
    setName("");
  }

  function apply(view: SavedView): void {
    onColumns(view.columns);
    navigate({ pathname: location.pathname, search: view.search ? `?${view.search}` : "" });
  }

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" title={t("views.title")}>
          <Bookmark aria-hidden="true" />
          {active ? active.name : t("views.button")}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-72 space-y-2 p-2">
        <p className="px-1 text-xs font-medium text-muted-foreground">{t("views.title")}</p>
        {views.length === 0 ? (
          <p className="px-1 text-sm text-muted-foreground">{t("views.none")}</p>
        ) : (
          <ul className="max-h-60 space-y-px overflow-y-auto">
            {views.map((view) => (
              <li key={view.name} className="flex items-center gap-1">
                <button
                  type="button"
                  onClick={() => apply(view)}
                  className="flex min-w-0 flex-1 items-center gap-2 rounded-sm px-1 py-1 text-start text-sm hover:bg-accent"
                >
                  <span className="min-w-0 flex-1 truncate">{view.name}</span>
                  {view === active ? <Check className="size-3.5 shrink-0" aria-label={t("views.current")} /> : null}
                </button>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={t("views.delete", { name: view.name })}
                  title={t("views.delete", { name: view.name })}
                  onClick={() => store(views.filter((other) => other !== view))}
                >
                  <Trash2 aria-hidden="true" />
                </Button>
              </li>
            ))}
          </ul>
        )}
        <form onSubmit={save} className="flex gap-1 border-t pt-2">
          <Input
            aria-label={t("views.name")}
            placeholder={t("views.name")}
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
          <Button type="submit" variant="outline" disabled={name.trim() === ""}>
            {t("views.save")}
          </Button>
        </form>
        <p className="px-1 text-xs text-muted-foreground">{t("views.local")}</p>
      </PopoverContent>
    </Popover>
  );
}

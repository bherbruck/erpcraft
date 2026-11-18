import { Columns3 } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import { useApp } from "@/app/context";
import { Button } from "@/components/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { t } from "@/i18n/t";
import { isStringArray, readStored, writeStored } from "@/lib/storage";
import type { Column } from "./data-table";

/**
 * Which columns of a table this user wants to see. The choice is kept in the
 * browser per user and per table; without storage the defaults apply.
 */
export function useColumnChoice<Row>(
  tableKey: string,
  columns: Column<Row>[],
): {
  visible: Column<Row>[];
  chooser: React.ReactNode;
  /** The columns chosen, or null for the defaults. */
  chosen: string[] | null;
  choose: (ids: string[] | null) => void;
} {
  const { me } = useApp();
  const storageKey = `columns.${me.user_id}.${tableKey}`;
  const defaults = useMemo(
    () => columns.filter((column) => !column.defaultHidden).map((column) => column.id),
    [columns],
  );
  const [stored, setStored] = useState<{ key: string; ids: string[] | null }>(() => ({
    key: storageKey,
    ids: readStored(storageKey, isStringArray),
  }));
  // Another table (or user): read its own choice.
  const chosen = stored.key === storageKey ? stored.ids : readStored(storageKey, isStringArray);
  const ids = chosen ?? defaults;
  const visible = useMemo(() => {
    const shown = columns.filter((column) => ids.includes(column.id));
    // Never an empty table: a stale choice falls back to the defaults.
    return shown.length > 0 ? shown : columns.filter((column) => defaults.includes(column.id));
  }, [columns, ids, defaults]);

  const choose = useCallback(
    (next: string[] | null) => {
      writeStored(storageKey, next);
      setStored({ key: storageKey, ids: next });
    },
    [storageKey],
  );

  const chooser = (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" title={t("columns.title")}>
          <Columns3 aria-hidden="true" />
          {t("columns.button")}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-56 gap-1 p-2">
        <p className="px-1 pb-1 text-xs font-medium text-muted-foreground">{t("columns.title")}</p>
        <ul className="max-h-72 overflow-y-auto">
          {columns.map((column) => {
            const checked = visible.some((shown) => shown.id === column.id);
            return (
              <li key={column.id}>
                <label
                  htmlFor={`column-${column.id}`}
                  className="flex cursor-pointer items-center gap-2 rounded-sm px-1 py-1 text-sm hover:bg-accent"
                >
                  <Checkbox
                    id={`column-${column.id}`}
                    checked={checked}
                    disabled={checked && visible.length === 1}
                    onCheckedChange={(next) => {
                      const now = visible.map((shown) => shown.id);
                      const wanted = next === true ? [...now, column.id] : now.filter((id) => id !== column.id);
                      // Kept in the table's own order, whatever the order of clicking.
                      choose(columns.map((each) => each.id).filter((id) => wanted.includes(id)));
                    }}
                  />
                  {column.header}
                </label>
              </li>
            );
          })}
        </ul>
        {chosen ? (
          <Button variant="ghost" size="sm" className="mt-1 w-full" onClick={() => choose(null)}>
            {t("columns.reset")}
          </Button>
        ) : null}
      </PopoverContent>
    </Popover>
  );
  return { visible, chooser, chosen, choose };
}

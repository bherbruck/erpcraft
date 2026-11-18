// Named views of a list: its filters, sort and columns, saved by the user and switched between
// from a menu. Kept in this browser per user and list, as in ../ui.

import { Button } from "@astryxdesign/core/Button";
import { DropdownMenu, DropdownMenuItem } from "@astryxdesign/core/DropdownMenu";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { Bookmark, Trash2 } from "lucide-react";
import { useState } from "react";
import { useLocation, useNavigate } from "react-router";
import { useApp } from "@/app/context";
import { t } from "@/i18n/t";
import { readStored, writeStored } from "@/lib/storage";

interface SavedView {
  name: string;
  search: string;
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
        (view.columns === null || (Array.isArray(view.columns) && view.columns.every((c: unknown) => typeof c === "string"))),
    )
  );
}

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
  const active = views.find((view) => view.search === current && JSON.stringify(view.columns) === JSON.stringify(columns));

  function store(next: SavedView[]): void {
    setViews(next);
    writeStored(storageKey, next.length > 0 ? next : null);
  }

  function save(): void {
    const trimmed = name.trim();
    if (!trimmed) return;
    store([...views.filter((view) => view.name !== trimmed), { name: trimmed, search: current, columns }].sort((a, b) => a.name.localeCompare(b.name)));
    setName("");
  }

  return (
    <DropdownMenu
      button={{ label: active ? active.name : t("views.button"), size: "sm", variant: "secondary", icon: <Icon icon={Bookmark} /> }}
      alignment="end"
      menuWidth={300}
      header={<Text type="supporting">{t("views.title")}</Text>}
      footer={
        <form
          onSubmit={(event) => {
            event.preventDefault();
            save();
          }}
        >
          <HStack gap={1} vAlign="end">
            <div className="grow">
              <TextInput label={t("views.name")} isLabelHidden placeholder={t("views.name")} size="sm" value={name} onChange={setName} />
            </div>
            <Button label={t("views.save")} type="submit" size="sm" isDisabled={!name.trim()} />
          </HStack>
        </form>
      }
    >
      {views.length === 0 ? (
        <DropdownMenuItem label={t("views.none")} isDisabled />
      ) : (
        views.map((view) => (
          <DropdownMenuItem
            key={view.name}
            label={view.name}
            onClick={() => {
              onColumns(view.columns);
              navigate({ pathname: location.pathname, search: view.search ? `?${view.search}` : "" });
            }}
            endContent={
              view === active ? <Text type="supporting">{t("views.current")}</Text> : undefined
            }
          />
        ))
      )}
      {views.length > 0 ? (
        <DropdownMenuItem
          label={active ? t("views.delete", { name: active.name }) : t("views.current")}
          icon={Trash2}
          variant="destructive"
          isDisabled={!active}
          onClick={() => active && store(views.filter((view) => view !== active))}
        />
      ) : null}
    </DropdownMenu>
  );
}


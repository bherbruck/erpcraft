import { Plus } from "lucide-react";
import { useMemo } from "react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { EntityMeta, EntityRecord } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { type Column, DataTable } from "@/components/data-table";
import { FieldValue, isNumeric } from "@/components/field-value";
import { Section } from "@/components/page";
import { Button } from "@/components/ui/button";
import { t } from "@/i18n/t";
import { canEntity, listFields } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";

/**
 * The records of an entity that belong to this record, such as the lines of a
 * bill of materials: a table on the parent's page instead of a menu entry.
 * A row opens the line; "Add" starts a new one for this parent.
 */
export function ChildRecords({ parent, parentId, child }: { parent: EntityMeta; parentId: string; child: EntityMeta }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const field = child.parent?.field ?? "";
  const rows = useAsync(`${child.name}|${parentId}`, (signal) =>
    api.run("entity.list", { entity: child.name, filters: { [field]: parentId }, limit: 500 }, { signal }),
  );
  const columns = useMemo<Column<EntityRecord>[]>(
    () =>
      listFields(meta, child)
        .filter((each) => each.type.kind !== "timestamp")
        .map((each) => ({
          id: each.name,
          header: each.label,
          align: isNumeric(each) ? "end" : "start",
          cell: (record) => (
            <FieldValue field={each} value={record[each.name]} context={{ header: record, fields: child.fields }} />
          ),
        })),
    [meta, child],
  );
  const canAdd = !child.read_only && canEntity(meta, child.name, "create");
  const add = canAdd ? (
    <Button asChild variant="outline" size="sm">
      <Link to={`${paths.entityNew(code, child.name)}?${new URLSearchParams({ [field]: parentId })}`}>
        <Plus aria-hidden="true" />
        {t("list.new", { label: child.label })}
      </Link>
    </Button>
  ) : null;
  return (
    <Section title={child.plural} aside={add}>
      <DataTable
        caption={child.plural}
        columns={columns}
        rows={rows.data?.items}
        rowKey={(record) => record.id}
        rowHref={(record) => paths.entityRecord(code, child.name, record.id)}
        loading={rows.loading}
        error={rows.error}
        onRetry={rows.reload}
        empty={
          <>
            <p>{t("children.empty", { label: child.plural, parent: parent.label })}</p>
            {add}
          </>
        }
      />
    </Section>
  );
}

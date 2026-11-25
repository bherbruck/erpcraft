// An entity whose records link to a record of the same entity (a warehouse's `parent_id`) is a
// tree: each record under the one it links to. Shown on its list (?view=tree) and, on the page of
// a record other records of the tree link to (a site), as the tree of those records.

import { Link } from "react-router";
import { api } from "@/api/client";
import type { EntityMeta, EntityRecord, FieldMeta, Meta } from "@/api/types";
import { useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { FieldValue } from "@/components/field-value";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { recordLabel } from "@/lib/link-labels";
import { canEntity, isCompanyScoped, readableFields } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";

/** The field by which records of the entity name the record they are under, if it has one. */
export function treeField(entity: EntityMeta): FieldMeta | undefined {
  return entity.fields.find(
    (field) => field.type.kind === "link" && field.type.entity === entity.name && field.type.key === "id",
  );
}

/** Trees whose records link to records of this entity: the warehouses of a site. */
export function treesLinkingTo(meta: Meta, entity: EntityMeta): { tree: EntityMeta; field: FieldMeta }[] {
  return meta.entities.flatMap((tree) => {
    // A place within a place (warehouses), not a ledger whose entries name one another.
    if (tree.name === entity.name || tree.read_only || !treeField(tree) || !canEntity(meta, tree.name, "read"))
      return [];
    const field = tree.fields.find(
      (candidate) =>
        candidate.type.kind === "link" && candidate.type.entity === entity.name && candidate.name !== "company_id",
    );
    return field ? [{ tree, field }] : [];
  });
}

const PAGE = 500;

/** Every record of the entity the filters allow, page by page. */
async function allRecords(
  entity: EntityMeta,
  filters: Record<string, string>,
  signal: AbortSignal,
): Promise<EntityRecord[]> {
  const sort = entity.fields.some((field) => field.name === "code") ? ["code"] : [];
  const out: EntityRecord[] = [];
  for (let offset = 0; ; offset += PAGE) {
    const page = await api.run("entity.list", { entity: entity.name, filters, sort, limit: PAGE, offset }, { signal });
    out.push(...page.items);
    if (page.items.length < PAGE || out.length >= page.total) return out;
  }
}

interface Node {
  record: EntityRecord;
  children: Node[];
}

/** The records nested under the ones they name; one whose parent is not among them is a root. */
function nest(records: EntityRecord[], parentField: string): Node[] {
  const nodes = new Map(records.map((record) => [record.id, { record, children: [] as Node[] }]));
  const roots: Node[] = [];
  for (const node of nodes.values()) {
    const parent = node.record[parentField];
    const owner = typeof parent === "string" ? nodes.get(parent) : undefined;
    if (owner && owner !== node) owner.children.push(node);
    else roots.push(node);
  }
  return roots;
}

export function RecordTree({
  meta,
  entity,
  filters = {},
  empty,
}: {
  meta: Meta;
  entity: EntityMeta;
  /** Which records: those of a site. The current company is added for a company's records. */
  filters?: Record<string, string>;
  empty: string;
}) {
  const { code, company } = useCompany();
  const parent = treeField(entity);
  const scoped = isCompanyScoped(entity);
  const all: Record<string, string> = { ...filters, ...(scoped && company ? { company_id: company.id } : {}) };
  const list = useAsync(scoped && !company ? null : `${entity.name}|${JSON.stringify(all)}`, (signal) =>
    allRecords(entity, all, signal),
  );
  // What each place says besides its name: the other records it names (its site), and the flags it has.
  const fields = readableFields(meta, entity);
  const links = fields.filter(
    (field) =>
      field.type.kind === "link" &&
      field.name !== parent?.name &&
      field.name !== "company_id" &&
      !(field.name in filters),
  );
  const flags = fields.filter((field) => field.type.kind === "bool" && field.name !== "active");

  if (!parent) return null;
  if (list.error) return <Alert title={t("list.load_failed")}>{list.error.message}</Alert>;
  if (!list.data)
    return (
      <div className="space-y-2 rounded-lg border bg-card p-4">
        <Skeleton className="h-5 w-48" />
        <Skeleton className="ms-6 h-5 w-40" />
        <Skeleton className="ms-6 h-5 w-44" />
      </div>
    );
  if (list.data.length === 0)
    return <p className="rounded-lg border bg-card p-4 text-sm text-muted-foreground">{empty}</p>;

  const render = (nodes: Node[], level: number) => (
    <ul className={level === 1 ? "space-y-1" : "mt-1 space-y-1 border-s ps-4 ms-2"}>
      {nodes.map(({ record, children }) => (
        <li key={record.id}>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 py-0.5 text-sm">
            <Link
              to={paths.entityRecord(code, entity.name, record.id)}
              className={
                record.active === false
                  ? "text-muted-foreground underline-offset-2 hover:underline"
                  : "font-medium underline-offset-2 hover:underline"
              }
            >
              {recordLabel(entity.name, record)}
            </Link>
            {record.active === false ? <span className="text-muted-foreground">{t("tree.inactive")}</span> : null}
            {flags.map((flag) =>
              record[flag.name] === true ? (
                <Badge key={flag.name} variant="secondary">
                  {flag.label}
                </Badge>
              ) : null,
            )}
            {links.map((link) =>
              record[link.name] ? (
                <span key={link.name} className="text-muted-foreground">
                  {t("tree.field", { label: link.label })} <FieldValue field={link} value={record[link.name]} />
                </span>
              ) : null,
            )}
          </div>
          {children.length > 0 ? render(children, level + 1) : null}
        </li>
      ))}
    </ul>
  );

  return (
    <nav aria-label={t("tree.label", { label: entity.plural })} className="rounded-lg border bg-card p-4">
      {render(nest(list.data, parent.name), 1)}
    </nav>
  );
}

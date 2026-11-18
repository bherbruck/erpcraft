// Records that belong to one record of another entity without being its lines:
// a customer's e-invoice data (`einvoice_party`, by a typed link), a company's
// e-invoice profile (`einvoice_profile`, whose display is the company). Found
// from the metadata: an entity whose required typed link (`entity_link`) can
// name this entity, or whose display starts with a required link to it.

import { Plus } from "lucide-react";
import { Link } from "react-router";
import { api } from "@/api/client";
import type { EntityMeta, Meta } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Section } from "@/components/page";
import { Button } from "@/components/ui/button";
import { t } from "@/i18n/t";
import { recordLabel } from "@/lib/link-labels";
import { canEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { useAsync } from "@/lib/use-async";

interface Attachment {
  entity: EntityMeta;
  /** The filters that find this record's, and the fields a new one starts with. */
  match: Record<string, string>;
}

/** The entities whose records attach to a record of `entity`, and how. */
function attachments(meta: Meta, entity: EntityMeta, id: string): Attachment[] {
  return meta.entities.flatMap((other): Attachment[] => {
    if (other.name === entity.name || other.parent || !canEntity(meta, other.name, "read")) return [];
    for (const field of other.fields) {
      if (!field.required || field.type.kind !== "entity_link") continue;
      const typeField = field.type.entity_field;
      const kinds = other.fields.find((candidate) => candidate.name === typeField)?.type;
      if (kinds?.kind === "enum" && kinds.values.includes(entity.name)) {
        return [{ entity: other, match: { [typeField]: entity.name, [field.name]: id } }];
      }
    }
    const first = other.fields.find((field) => field.name === other.display[0]);
    if (first?.required && first.type.kind === "link" && first.type.entity === entity.name) {
      return [{ entity: other, match: { [first.name]: id } }];
    }
    return [];
  });
}

export function AttachedRecords({ entity, id }: { entity: EntityMeta; id: string }) {
  const { meta } = useApp();
  const found = attachments(meta, entity, id);
  if (found.length === 0) return null;
  return (
    <>
      {found.map((attachment) => (
        <AttachedOf key={attachment.entity.name} attachment={attachment} />
      ))}
    </>
  );
}

function AttachedOf({ attachment }: { attachment: Attachment }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const { entity, match } = attachment;
  const rows = useAsync(`attached|${entity.name}|${JSON.stringify(match)}`, (signal) =>
    api.run("entity.list", { entity: entity.name, filters: match, limit: 20 }, { signal }),
  );
  const items = rows.data?.items ?? [];
  const canCreate = canEntity(meta, entity.name, "create") && !entity.read_only;
  return (
    <Section
      title={entity.label}
      aside={
        canCreate && rows.data && items.length === 0 ? (
          <Button asChild variant="outline" size="sm">
            <Link to={`${paths.entityNew(code, entity.name)}?${new URLSearchParams(match)}`}>
              <Plus aria-hidden="true" />
              {t("record.new", { label: entity.label })}
            </Link>
          </Button>
        ) : null
      }
    >
      {rows.data === undefined ? (
        <p className="text-sm text-muted-foreground">{t("value.loading")}</p>
      ) : items.length === 0 ? (
        <p className="rounded-md border bg-card p-3 text-sm text-muted-foreground">
          {t("attached.none", { label: entity.label })}
        </p>
      ) : (
        <ul className="divide-y rounded-md border bg-card text-sm">
          {items.map((item) => {
            const itemId = typeof item === "object" && item && !Array.isArray(item) ? item.id : null;
            return typeof itemId === "string" ? (
              <li key={itemId} className="px-3 py-2">
                <Link
                  to={paths.entityRecord(code, entity.name, itemId)}
                  className="underline underline-offset-2 hover:no-underline"
                >
                  {typeof item === "object" && item && !Array.isArray(item) ? recordLabel(entity.name, item) : itemId}
                </Link>
              </li>
            ) : null;
          })}
        </ul>
      )}
    </Section>
  );
}

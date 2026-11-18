// What an approval request is about, in business terms: the facts the rule
// decided on as the policy point declares them, the record the command acts
// on by its number and party, and the command's input with ids shown as what
// they name.

import { Link } from "react-router";
import type { DecimalMeta, FactMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatDecimal, isDecimal } from "@/lib/decimal";
import { documentLabel, useDocumentHeader } from "@/lib/doc-labels";
import { canDoc, commandInputs, findCommand, findDocType, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { minFraction } from "./decimal-value";
import { Empty, FieldValue, LinkLabel } from "./field-value";

/** A fact as the server may declare it: with a label and what kind of number it is (newer servers). */
type DeclaredFact = FactMeta & { label?: string | null; decimal?: DecimalMeta | null };

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * The facts of a request, labelled from the policy point's declaration, in its
 * order. A number the point declares as money is shown in the currency another
 * fact names ("150,000.00 USD"); a fact that only names that currency is then
 * not repeated.
 */
export function ApprovalFacts({
  point,
  facts,
  compact = false,
}: {
  point: string;
  facts: Record<string, Json>;
  compact?: boolean;
}) {
  const { meta, currencyDecimals } = useApp();
  const declared: DeclaredFact[] = meta.policies?.find((policy) => policy.name === point)?.facts ?? [];
  const known = new Map(declared.map((fact) => [fact.name, fact]));
  const currencyOf = (fact: DeclaredFact | undefined): string | null => {
    const source = fact?.decimal?.currency;
    if (!source || !("field" in source)) return null;
    const named = facts[source.field];
    return typeof named === "string" ? named : null;
  };
  // Facts that only say which currency another fact is in.
  const usedAsCurrency = new Set(
    declared.flatMap((fact) => {
      const source = fact.decimal?.currency;
      return source && "field" in source && fact.name in facts ? [source.field] : [];
    }),
  );
  const names = [
    ...declared.map((fact) => fact.name).filter((name) => name in facts),
    ...Object.keys(facts).filter((name) => !known.has(name)),
  ].filter((name) => !usedAsCurrency.has(name));
  if (names.length === 0) return null;
  const rows = names.map((name) => {
    const fact = known.get(name);
    const raw = facts[name];
    const value = typeof raw === "string" ? raw : JSON.stringify(raw);
    const currency = currencyOf(fact);
    const shown =
      fact?.type === "number" && isDecimal(value)
        ? formatDecimal(value, {
            minFraction: minFraction(fact.decimal ?? null, currency ? currencyDecimals[currency] : undefined),
          })
        : fact?.type === "bool"
          ? value === "true"
            ? t("value.yes")
            : t("value.no")
          : value;
    return { name, fact, shown, currency };
  });

  if (compact) {
    // In a list or a note: one line, each fact by its short name, its description on hover.
    return (
      <span className="flex flex-wrap gap-x-3 gap-y-0.5 text-xs">
        {rows.map(({ name, fact, shown, currency }) => (
          <span key={name} className="whitespace-nowrap" title={fact?.description}>
            <span className="text-muted-foreground">{fact?.label ?? name.replaceAll("_", " ")}</span>{" "}
            <span className="tabular-nums">{shown}</span>
            {currency ? <span className="ms-1 text-muted-foreground">{currency}</span> : null}
          </span>
        ))}
      </span>
    );
  }

  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-md border bg-card p-4 text-sm">
      {rows.map(({ name, fact, shown, currency }) => {
        return (
          <div key={name} className="contents">
            <dt className="text-muted-foreground" title={fact?.description}>
              {fact?.label ?? fact?.description ?? name}
            </dt>
            <dd className="tabular-nums">
              {shown}
              {currency ? <span className="ms-1 text-xs text-muted-foreground">{currency}</span> : null}
            </dd>
          </div>
        );
      })}
    </dl>
  );
}

/**
 * The record a command acts on: a document by its number and party, with its
 * totals unless `compact`; an entity record by its display.
 */
export function ApprovalSubject({
  command: name,
  input,
  compact = false,
}: {
  command: string;
  input: Json | undefined;
  compact?: boolean;
}) {
  const { meta } = useApp();
  const command = findCommand(meta, name);
  const id = isObject(input) && typeof input.id === "string" && command?.target ? input.id : null;
  const docType = command?.resource_kind === "document" ? findDocType(meta, command.resource) : undefined;
  if (!id || !command) return compact ? null : <Empty />;
  if (docType) return <DocumentSubject docTypeName={docType.name} id={id} compact={compact} />;
  if (findEntity(meta, command.resource)) {
    return <LinkLabel entity={command.resource} keyField="id" value={id} linkify={!compact} />;
  }
  return compact ? null : <Empty />;
}

function DocumentSubject({ docTypeName, id, compact }: { docTypeName: string; id: string; compact: boolean }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const docType = findDocType(meta, docTypeName);
  const header = useDocumentHeader(docTypeName, id);
  if (header === undefined) return <span className="text-muted-foreground">{t("value.loading")}</span>;
  if (header === null) return <span className="text-muted-foreground">{docType?.label}</span>;
  const label = `${docType?.label ?? docTypeName} ${documentLabel(docType, header)}`;
  const summary = isObject(header.summary) ? header.summary : {};
  const totals = (docType?.summary ?? []).filter(
    (field) => field.type.kind === "decimal" && !field.name.endsWith("_accounting") && summary[field.name] != null,
  );
  const readable = canDoc(meta, docTypeName, "read");
  const title = readable ? (
    <Link to={paths.doc(code, docTypeName, id)} className="underline underline-offset-2 hover:no-underline">
      {label}
    </Link>
  ) : (
    label
  );
  if (compact) return <span className="whitespace-nowrap">{label}</span>;
  return (
    <span className="space-y-0.5">
      <span className="block font-medium">{title}</span>
      {totals.map((field) => (
        <span key={field.name} className="flex gap-2 text-muted-foreground">
          <span>{field.label}</span>
          <span className="text-foreground">
            <FieldValue
              field={field}
              value={summary[field.name]}
              context={{ header: summary, fields: docType?.summary ?? [] }}
            />
          </span>
        </span>
      ))}
    </span>
  );
}

/**
 * The command's input, read as a person would: each value the command was
 * given, labelled from its schema, with ids shown as what they name. The id of
 * the record it acts on is the subject, shown apart.
 */
export function ApprovalInput({ command: name, input }: { command: string; input: Json | undefined }) {
  const { meta } = useApp();
  const command = findCommand(meta, name);
  if (!command || !isObject(input)) return null;
  const inputs = commandInputs(command);
  const shown = inputs.filter(
    (property) => !(property.name === "id" && command.target) && input[property.name] != null,
  );
  if (shown.length === 0) return <p className="text-sm text-muted-foreground">{t("approval.input_none")}</p>;
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-md border bg-card p-4 text-sm">
      {shown.map((property) => {
        const value = input[property.name];
        const link = property.fieldType?.kind === "link" ? property.fieldType : null;
        return (
          <div key={property.name} className="contents">
            <dt className="text-muted-foreground">{property.label}</dt>
            <dd>
              {link && typeof value === "string" ? (
                <LinkLabel entity={link.entity} keyField={link.key} value={value} />
              ) : typeof value === "string" ? (
                property.decimal ? (
                  formatDecimal(value)
                ) : (
                  value
                )
              ) : typeof value === "boolean" ? (
                value ? (
                  t("value.yes")
                ) : (
                  t("value.no")
                )
              ) : (
                <code className="font-mono text-xs">{JSON.stringify(value)}</code>
              )}
            </dd>
          </div>
        );
      })}
    </dl>
  );
}

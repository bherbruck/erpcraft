// What a posting puts in each ledger. The server returns the entries with
// every posting, keyed by ledger, on a posted document and on the preview of
// one about to be posted. Each ledger is described under `ledgers` in the
// registry: its label and the schema of one entry, whose titles name the
// columns here. A ledger the user may not read is absent, and so is its table.

import { Link } from "react-router";
import type { Json, JsonObject, Meta, Posting } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Badge } from "@/components/ui/badge";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { formatCalendarDate, formatTimestamp } from "@/lib/dates";
import { formatDecimal, isZeroDecimal } from "@/lib/decimal";
import { canDoc, canRun, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { Amount, Empty, LinkLabel } from "./field-value";

type Entries = Posting["entries"];
type GlRow = NonNullable<Entries["gl"]>[number];
type StockRow = NonNullable<Entries["stock"]>[number];
type OpenItemRow = NonNullable<Entries["open_items"]>[number];
type TaxRow = NonNullable<Entries["tax"]>[number];

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** What the registry says about one field of one ledger's entries. */
function property(meta: Meta, ledger: string, field: string): JsonObject {
  const schema = meta.ledgers.find((candidate) => candidate.name === ledger)?.entry_schema;
  const properties = isObject(schema) && isObject(schema.properties) ? schema.properties : {};
  const found = properties[field];
  return isObject(found) ? found : {};
}

/** A column's title, from the entry schema. */
function useTitle(ledger: string): (field: string, currency?: string) => string {
  const { meta } = useApp();
  return (field, currency) => {
    const title = property(meta, ledger, field).title;
    const label = typeof title === "string" ? title : field;
    return currency ? t("value.in_currency", { label, currency }) : label;
  };
}

function ledgerLabel(meta: Meta, ledger: string): string {
  return meta.ledgers.find((candidate) => candidate.name === ledger)?.label ?? ledger;
}

/** "1200 · Bank", from the code and name an entry carries. */
function codeName(code: string | null | undefined, name: string | null | undefined): string {
  return [code, name].filter(Boolean).join(" · ");
}

/** A debit or credit: an amount, or nothing where it is zero. */
function Side({ value, decimals }: { value: string | null; decimals: number }) {
  return value === null || isZeroDecimal(value) ? null : <Amount value={value} decimals={decimals} />;
}

/** The amount in the transaction's currency, where that is not the accounting currency. */
function InCurrency({
  amount,
  currency,
  accounting,
  decimals,
}: {
  amount: string | null;
  currency: string | null;
  accounting: string;
  decimals: number;
}) {
  if (!amount || !currency || currency === accounting || isZeroDecimal(amount)) return null;
  return (
    <span className="ms-2 whitespace-nowrap text-xs text-muted-foreground">
      {t("postings.in_currency", { currency, amount: formatDecimal(amount, { minFraction: decimals }) })}
    </span>
  );
}

function GeneralLedger({ entries, decimals, currency }: { entries: GlRow[]; decimals: number; currency: string }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const title = useTitle("gl");
  const ledgerOpen = canRun(meta, "gl.account_ledger");
  return (
    <Table aria-label={ledgerLabel(meta, "gl")}>
      <TableHeader>
        <TableRow>
          <TableHead className="w-full">{title("account_id")}</TableHead>
          <TableHead>{title("memo")}</TableHead>
          <TableHead className="text-end">{title("debit", currency)}</TableHead>
          <TableHead className="text-end">{title("credit", currency)}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {entries.map((entry, index) => {
          const label = codeName(entry.account_code, entry.account_name);
          const account = entry.account_id;
          const name = label || (account ? <LinkLabel entity="account" keyField="id" value={account} /> : <Empty />);
          const txn = entry.txn_debit && !isZeroDecimal(entry.txn_debit) ? entry.txn_debit : entry.txn_credit;
          return (
            <TableRow key={entry.id ?? index}>
              <TableCell className="whitespace-nowrap">
                {account && ledgerOpen ? (
                  <Link
                    to={`${paths.report(code, "gl.account_ledger")}?account=${account}`}
                    className="underline underline-offset-2 hover:no-underline"
                  >
                    {name}
                  </Link>
                ) : (
                  name
                )}
                <InCurrency amount={txn} currency={entry.currency} accounting={currency} decimals={decimals} />
              </TableCell>
              <TableCell className="whitespace-nowrap text-muted-foreground">{entry.memo}</TableCell>
              <TableCell className="text-end">
                <Side value={entry.debit} decimals={decimals} />
              </TableCell>
              <TableCell className="text-end">
                <Side value={entry.credit} decimals={decimals} />
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

function StockLedger({ entries, decimals, currency }: { entries: StockRow[]; decimals: number; currency: string }) {
  const { meta } = useApp();
  const title = useTitle("stock");
  const qty = (value: string | null, unit: string | null) =>
    value === null ? <Empty /> : t("postings.qty_unit", { qty: formatDecimal(value), unit: unit ?? "" });
  return (
    <Table aria-label={ledgerLabel(meta, "stock")}>
      <TableHeader>
        <TableRow>
          <TableHead className="w-full">{title("item_id")}</TableHead>
          <TableHead>{title("warehouse_id")}</TableHead>
          <TableHead className="text-end">{title("qty")}</TableHead>
          <TableHead className="text-end">{title("unit_cost", currency)}</TableHead>
          <TableHead className="text-end">{title("value", currency)}</TableHead>
          <TableHead className="text-end">{title("qty_after")}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {entries.map((entry, index) => (
          <TableRow key={entry.id ?? index}>
            <TableCell className="whitespace-nowrap">
              {codeName(entry.item_code, entry.item_name) || <Empty />}
            </TableCell>
            <TableCell className="whitespace-nowrap">{entry.warehouse_code ?? <Empty />}</TableCell>
            <TableCell className="whitespace-nowrap text-end tabular-nums">{qty(entry.qty, entry.uom)}</TableCell>
            <TableCell className="text-end">
              <Amount value={entry.unit_cost} decimals={decimals} />
            </TableCell>
            <TableCell className="text-end">
              <Amount value={entry.value} decimals={decimals} />
            </TableCell>
            <TableCell className="whitespace-nowrap text-end tabular-nums text-muted-foreground">
              {qty(entry.qty_after, entry.uom)}
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function OpenItemsLedger({
  entries,
  decimals,
  currency,
}: {
  entries: OpenItemRow[];
  decimals: number;
  currency: string;
}) {
  const { meta } = useApp();
  const { code } = useCompany();
  const title = useTitle("open_items");
  return (
    <Table aria-label={ledgerLabel(meta, "open_items")}>
      <TableHeader>
        <TableRow>
          <TableHead className="w-full">{title("entry")}</TableHead>
          <TableHead>{title("party_id")}</TableHead>
          <TableHead>{title("due_date")}</TableHead>
          <TableHead className="text-end">{title("amount", currency)}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {entries.map((entry, index) => {
          const settles = entry.entry === "settlement";
          const kind = t(entry.kind === "payable" ? "open_items.payable" : "open_items.receivable");
          const what = settles
            ? t(entry.cancels === true ? "postings.cancels" : "postings.settles", { kind })
            : t(entry.kind === "payable" ? "postings.becomes_payable" : "postings.becomes_receivable");
          return (
            <TableRow key={entry.id ?? index}>
              <TableCell className="whitespace-nowrap">
                {what}
                {settles && entry.number ? (
                  <>
                    {" "}
                    {entry.doc_type && entry.document_id && canDoc(meta, entry.doc_type, "read") ? (
                      <Link
                        to={paths.doc(code, entry.doc_type, entry.document_id)}
                        className="font-medium underline underline-offset-2 hover:no-underline"
                      >
                        {findDocType(meta, entry.doc_type)?.label ?? entry.doc_type} {entry.number}
                      </Link>
                    ) : (
                      <span className="font-medium">{entry.number}</span>
                    )}
                  </>
                ) : null}
                <InCurrency
                  amount={entry.txn_amount}
                  currency={entry.currency}
                  accounting={currency}
                  decimals={decimals}
                />
              </TableCell>
              <TableCell className="whitespace-nowrap">
                {entry.party_id && entry.party_type ? (
                  <LinkLabel entity={entry.party_type} keyField="id" value={entry.party_id} linkify />
                ) : (
                  <Empty />
                )}
              </TableCell>
              <TableCell className="whitespace-nowrap">
                {entry.due_date ? formatCalendarDate(entry.due_date) : <Empty />}
              </TableCell>
              <TableCell className="text-end">
                <Amount value={entry.amount} decimals={decimals} />
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );
}

function TaxLedger({ entries, decimals, currency }: { entries: TaxRow[]; decimals: number; currency: string }) {
  const { meta } = useApp();
  const title = useTitle("tax");
  return (
    <Table aria-label={ledgerLabel(meta, "tax")}>
      <TableHeader>
        <TableRow>
          <TableHead className="w-full">{title("tax_code_id")}</TableHead>
          <TableHead>{title("direction")}</TableHead>
          <TableHead className="text-end">{title("rate")}</TableHead>
          <TableHead className="text-end">{title("base", currency)}</TableHead>
          <TableHead className="text-end">{title("amount", currency)}</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {entries.map((entry, index) => (
          <TableRow key={entry.id ?? index}>
            <TableCell className="whitespace-nowrap">{codeName(entry.code, entry.name) || <Empty />}</TableCell>
            <TableCell className="whitespace-nowrap">
              {entry.direction === "paid"
                ? t("postings.tax_paid")
                : entry.direction === "collected"
                  ? t("postings.tax_collected")
                  : (entry.direction ?? <Empty />)}
            </TableCell>
            <TableCell className="whitespace-nowrap text-end tabular-nums">
              {entry.rate ? `${formatDecimal(entry.rate)}${t("value.percent_sign")}` : <Empty />}
            </TableCell>
            <TableCell className="text-end">
              <Amount value={entry.base} decimals={decimals} />
            </TableCell>
            <TableCell className="text-end">
              <Amount value={entry.amount} decimals={decimals} />
            </TableCell>
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

// A field of an entry that names nothing a person reads.
const QUIET = new Set(["uuid"]);

/** A ledger the UI has no layout for: a table of its entry schema's fields, titled from the schema. */
function OtherLedger({ ledger, entries, decimals }: { ledger: string; entries: JsonObject[]; decimals: number }) {
  const { meta } = useApp();
  const schema = meta.ledgers.find((candidate) => candidate.name === ledger)?.entry_schema;
  const properties = isObject(schema) && isObject(schema.properties) ? schema.properties : {};
  const columns = Object.entries(properties)
    .filter(([, value]) => isObject(value) && !(typeof value.format === "string" && QUIET.has(value.format)))
    .map(([name, value]) => ({
      name,
      title: isObject(value) && typeof value.title === "string" ? value.title : name,
      decimal: isObject(value) && typeof value.pattern === "string",
    }));
  return (
    <Table aria-label={ledgerLabel(meta, ledger)}>
      <TableHeader>
        <TableRow>
          {columns.map((column) => (
            <TableHead key={column.name} className={column.decimal ? "text-end" : undefined}>
              {column.title}
            </TableHead>
          ))}
        </TableRow>
      </TableHeader>
      <TableBody>
        {entries.map((entry, index) => (
          <TableRow key={typeof entry.id === "string" ? entry.id : index}>
            {columns.map((column) => {
              const value = entry[column.name];
              return (
                <TableCell key={column.name} className={column.decimal ? "text-end" : "whitespace-nowrap"}>
                  {typeof value === "string" && column.decimal ? (
                    <Amount value={value} decimals={decimals} />
                  ) : value === null || value === undefined ? (
                    <Empty />
                  ) : (
                    String(value)
                  )}
                </TableCell>
              );
            })}
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

/** The entries of one posting, ledger by ledger, in the order the registry lists the ledgers. */
export function PostingEntries({ posting, currency }: { posting: Posting; currency: string }) {
  const { meta, currencyDecimals } = useApp();
  const decimals = currencyDecimals[currency] ?? 2;
  const entries = posting.entries;
  const known = meta.ledgers.map((ledger) => ledger.name);
  const ledgers = [
    ...known.filter((name) => (entries[name]?.length ?? 0) > 0),
    ...Object.keys(entries).filter((name) => !known.includes(name) && (entries[name]?.length ?? 0) > 0),
  ];
  if (ledgers.length === 0)
    return <p className="px-3 py-2 text-sm text-muted-foreground">{t("postings.no_entries")}</p>;
  return (
    <div>
      {ledgers.map((ledger) => (
        <section key={ledger}>
          <h4 className="border-t px-3 pb-1 pt-2 text-xs font-medium text-muted-foreground first:border-t-0">
            {ledgerLabel(meta, ledger)}
          </h4>
          {ledger === "gl" && entries.gl ? (
            <GeneralLedger entries={entries.gl} decimals={decimals} currency={currency} />
          ) : ledger === "stock" && entries.stock ? (
            <StockLedger entries={entries.stock} decimals={decimals} currency={currency} />
          ) : ledger === "open_items" && entries.open_items ? (
            <OpenItemsLedger entries={entries.open_items} decimals={decimals} currency={currency} />
          ) : ledger === "tax" && entries.tax ? (
            <TaxLedger entries={entries.tax} decimals={decimals} currency={currency} />
          ) : (
            <OtherLedger ledger={ledger} entries={(entries[ledger] ?? []).filter(isObject)} decimals={decimals} />
          )}
        </section>
      ))}
    </div>
  );
}

/** What a document posted: each posting with its entries under it. */
export function Postings({ postings, currency }: { postings: Posting[]; currency: string }) {
  return (
    <div className="space-y-3">
      {postings.map((posting) => (
        <article key={posting.id} className="rounded-md border bg-card">
          <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b px-3 py-2 text-sm">
            <span className="font-medium">
              {t("postings.dated", { date: formatCalendarDate(posting.posting_date) })}
            </span>
            {posting.reverses_id ? <Badge tone="warning">{t("postings.reversal")}</Badge> : null}
            {posting.memo ? <span className="text-muted-foreground">{posting.memo}</span> : null}
            <span className="ms-auto text-xs text-muted-foreground">
              {posting.created_by_name
                ? t("postings.recorded_by", { at: formatTimestamp(posting.created_at), name: posting.created_by_name })
                : t("postings.recorded", { at: formatTimestamp(posting.created_at) })}
            </span>
          </header>
          <PostingEntries posting={posting} currency={currency} />
        </article>
      ))}
    </div>
  );
}

// The matching workspace of a draft bank statement: every line with what it
// probably is (`bank_statement.proposals`, best first), accepted with
// `bank_statement.match`. A person matches dozens of lines in a sitting, so it
// is built for the keyboard: arrows (or j and k) move between lines, Enter takes
// the best proposal and 1 to 9 a particular one, `a` goes to the account, `x`
// clears. A line matched to open items gets a drafted payment, posted here
// through its own preview; the statement is then reconciled with its own action.

import { Check, CornerDownLeft, X } from "lucide-react";
import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import { ApiError, api } from "@/api/client";
import type { BankStatementProposalsOutput } from "@/api/generated";
import type { DocTypeMeta, DocumentRecord, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { t } from "@/i18n/t";
import { cn } from "@/lib/cn";
import { formatCalendarDate } from "@/lib/dates";
import { isZeroDecimal } from "@/lib/decimal";
import { forgetDocuments, useDocumentHeader } from "@/lib/doc-labels";
import { canRun, findCommand, findDocType, toAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { ActionDialog } from "./command-action";
import { Amount, LinkLabel } from "./field-value";
import { Lookup } from "./lookup";

export const BANK_STATEMENT = "bank_statement";
const MATCH = "bank_statement.match";
const PROPOSALS = "bank_statement.proposals";

type LineProposals = BankStatementProposalsOutput["lines"][number];
type Proposal = LineProposals["proposals"][number];

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function text(line: JsonObject, name: string): string | null {
  const value = line[name];
  return typeof value === "string" && value !== "" ? value : null;
}

/** What a payment drafted for a line is, and posting it from here. */
function DraftedPayment({ id, onPosted }: { id: string; onPosted: () => void }) {
  const { meta } = useApp();
  const { code } = useCompany();
  const header = useDocumentHeader("payment", id);
  const [posting, setPosting] = useState(false);
  const post = findCommand(meta, "payment.post");
  const payments = findDocType(meta, "payment");
  if (!header) return null;
  const draft = header.status === "draft";
  return (
    <span className="flex flex-wrap items-center gap-2">
      <Link to={paths.doc(code, "payment", id)} className="underline underline-offset-2 hover:no-underline">
        {payments?.label} {header.number ?? t("document.unnumbered")}
      </Link>
      <Badge tone={draft ? "warning" : "success"}>
        {payments?.states.find((state) => state.name === header.status)?.label ?? header.status}
      </Badge>
      {draft && post && canRun(meta, post.name) ? (
        <Button size="sm" variant="outline" onClick={() => setPosting(true)}>
          {t("bank.post_payment", { action: post.label })}
        </Button>
      ) : null}
      {posting && post ? (
        <ActionDialog
          action={toAction(post)}
          input={{ id }}
          subject={`${payments?.label ?? ""} ${header.number ?? t("document.unnumbered")}`}
          current={header}
          fields={meta.document_header}
          docType={payments}
          onClose={() => setPosting(false)}
          onDone={() => {
            setPosting(false);
            forgetDocuments();
            onPosted();
          }}
        />
      ) : null}
    </span>
  );
}

/** What a matched line is now. */
function Matched({ line, onPosted }: { line: JsonObject; onPosted: () => void }) {
  const payment = text(line, "payment_id");
  const account = text(line, "account_id");
  const posting = text(line, "posting_id");
  if (payment) return <DraftedPayment id={payment} onPosted={onPosted} />;
  if (account)
    return (
      <span>
        {t("bank.to_account")} <LinkLabel entity="account" keyField="id" value={account} linkify />
      </span>
    );
  if (posting) return <span>{t("bank.to_posting")}</span>;
  return null;
}

function proposalText(proposal: Proposal): string {
  if (proposal.kind === "posting" && proposal.posting)
    return t("bank.proposal_posting", {
      number: proposal.posting.number ?? t("document.unnumbered"),
      date: formatCalendarDate(proposal.posting.posting_date),
    });
  if (proposal.kind === "open_items")
    return t("bank.proposal_open_items", { numbers: proposal.numbers.join(", "), party: proposal.party_name ?? "" });
  return t("bank.proposal_account");
}

export function BankMatching({
  doc,
  docType,
  onChanged,
}: {
  doc: DocumentRecord;
  docType: DocTypeMeta;
  onChanged: () => void;
}) {
  const { meta, currencyDecimals } = useApp();
  const { company } = useCompany();
  const currency = text(doc.body, "currency") ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const lines = useMemo(() => {
    const value = doc.body.lines;
    return Array.isArray(value) ? value.filter(isObject) : [];
  }, [doc]);
  const proposals = useAsync(canRun(meta, PROPOSALS) ? `proposals|${doc.id}|${doc.version}` : null, (signal) =>
    api.run("bank_statement.proposals", { id: doc.id }, { signal }),
  );
  const byLine = new Map((proposals.data?.lines ?? []).map((entry) => [entry.line_id, entry]));
  const [active, setActive] = useState(() =>
    Math.max(
      0,
      lines.findIndex((line) => !isMatched(line)),
    ),
  );
  const [accounts, setAccounts] = useState<Record<string, string | null>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<{ line: string; error: ApiError } | null>(null);
  const rows = useRef<(HTMLLIElement | null)[]>([]);
  const canMatch = canRun(meta, MATCH);
  const collection = docType.collections.find((candidate) => candidate.name === "lines");
  const label = (name: string): string => collection?.fields.find((field) => field.name === name)?.label ?? name;
  const unmatched = lines.filter((line) => !isMatched(line)).length;

  // The current line holds the keyboard: when it changes, and when the statement
  // comes back from the server with nothing else holding the focus (a dialog closed).
  const list = useRef<HTMLUListElement>(null);
  // biome-ignore lint/correctness/useExhaustiveDependencies: `doc.version` is a new copy of the statement
  useEffect(() => {
    const focused = document.activeElement;
    if (focused === document.body || focused === null || list.current?.contains(focused)) {
      rows.current[active]?.focus({ preventScroll: false });
    }
  }, [active, doc.version]);

  /** On to the first line after this one still to match, if any. */
  function advance(from: number): void {
    const next = lines.findIndex((other, index) => index > from && !isMatched(other));
    if (next !== -1) setActive(next);
  }

  async function match(line: JsonObject, input: JsonObject): Promise<void> {
    const lineId = text(line, "id") ?? "";
    setBusy(lineId);
    setError(null);
    try {
      await api.runNamed(MATCH, { id: doc.id, line_id: lineId, ...input }, { idempotencyKey: newKey() });
      forgetDocuments();
      onChanged();
      advance(lines.indexOf(line));
    } catch (cause) {
      setError({
        line: lineId,
        error: cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")),
      });
    }
    setBusy(null);
  }

  function accept(line: JsonObject, proposal: Proposal | undefined): void {
    if (!proposal) return;
    const lineId = text(line, "id") ?? "";
    if (proposal.kind === "posting" && proposal.posting)
      void match(line, { action: "posting", posting_id: proposal.posting.posting_id });
    else if (proposal.kind === "open_items")
      void match(line, { action: "open_items", open_item_ids: proposal.open_item_ids });
    else if (proposal.kind === "account") {
      const account = proposal.account_id ?? accounts[lineId] ?? null;
      if (account) void match(line, { action: "account", account_id: account });
      else document.getElementById(`match-account-${lineId}`)?.focus();
    }
  }

  function onKey(event: KeyboardEvent<HTMLLIElement>, index: number): void {
    // Keys typed into the account picker are the picker's.
    if (event.target !== event.currentTarget) return;
    const line = lines[index];
    if (!line) return;
    const lineId = text(line, "id") ?? "";
    const offered = byLine.get(lineId)?.proposals ?? [];
    if (event.key === "ArrowDown" || event.key === "j") setActive(Math.min(index + 1, lines.length - 1));
    else if (event.key === "ArrowUp" || event.key === "k") setActive(Math.max(index - 1, 0));
    else if (event.key === "Enter" && canMatch && !isMatched(line)) accept(line, offered[0]);
    else if (/^[1-9]$/.test(event.key) && canMatch && !isMatched(line))
      accept(line, offered[Number.parseInt(event.key, 10) - 1]);
    else if (event.key === "a" && canMatch) document.getElementById(`match-account-${lineId}`)?.focus();
    else if ((event.key === "x" || event.key === "Delete") && canMatch && isMatched(line))
      void match(line, { action: "clear" });
    else return;
    event.preventDefault();
  }

  return (
    <section className="space-y-2" aria-label={t("bank.matching")}>
      <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1">
        <h2 className="text-sm font-semibold">{t("bank.matching")}</h2>
        <span className="text-sm text-muted-foreground">
          {t("bank.to_match", { count: unmatched, total: lines.length })}
        </span>
        <span className="ms-auto hidden text-xs text-muted-foreground md:inline">{t("bank.keys")}</span>
      </div>
      {proposals.error ? <Alert title={t("bank.proposals_failed")}>{proposals.error.message}</Alert> : null}
      <ul ref={list} className="divide-y rounded-md border bg-card">
        {lines.map((line, index) => {
          const lineId = text(line, "id") ?? String(index);
          const matched = isMatched(line);
          const offered = byLine.get(lineId)?.proposals ?? [];
          const amount = text(line, "amount");
          const difference = text(line, "difference");
          return (
            <li
              key={lineId}
              ref={(element) => {
                rows.current[index] = element;
              }}
              tabIndex={index === active ? 0 : -1}
              aria-current={index === active ? "true" : undefined}
              aria-label={t("bank.line", { line: index + 1 })}
              onFocus={() => setActive(index)}
              onKeyDown={(event) => onKey(event, index)}
              className={cn(
                "grid gap-x-4 gap-y-2 px-3 py-2.5 text-sm outline-none md:grid-cols-[minmax(0,2fr)_auto_minmax(0,3fr)]",
                index === active && "bg-accent/50 ring-2 ring-inset ring-ring",
              )}
            >
              <div className="min-w-0 space-y-0.5">
                <div className="flex items-baseline gap-2">
                  <span className="w-6 shrink-0 text-end text-xs tabular-nums text-muted-foreground">{index + 1}</span>
                  <span className="whitespace-nowrap">
                    {text(line, "booking_date") ? formatCalendarDate(text(line, "booking_date") ?? "") : null}
                  </span>
                  <span className="truncate font-medium">{text(line, "counterparty_name") ?? ""}</span>
                </div>
                <p className="ms-8 truncate text-muted-foreground" title={text(line, "reference") ?? undefined}>
                  {text(line, "reference") ?? ""}
                </p>
              </div>
              <div className="text-end font-medium md:min-w-28">
                <Amount value={amount} decimals={decimals} />
                {difference && !isZeroDecimal(difference) ? (
                  <p className="text-xs text-muted-foreground">
                    {label("difference")} <Amount value={difference} decimals={decimals} />
                  </p>
                ) : null}
              </div>
              <div className="min-w-0 space-y-1.5">
                {error?.line === lineId ? (
                  <p role="alert" className="text-xs text-destructive">
                    {error.error.message}
                  </p>
                ) : null}
                {matched ? (
                  <div className="flex flex-wrap items-center gap-2">
                    <Check className="size-4 shrink-0 text-success" aria-label={t("bank.matched")} />
                    <Matched
                      line={line}
                      onPosted={() => {
                        onChanged();
                        advance(index);
                      }}
                    />
                    {canMatch ? (
                      <Button
                        size="sm"
                        variant="ghost"
                        className="ms-auto"
                        busy={busy === lineId}
                        tabIndex={-1}
                        onClick={() => void match(line, { action: "clear" })}
                      >
                        <X aria-hidden="true" />
                        {t("bank.clear")}
                      </Button>
                    ) : null}
                  </div>
                ) : (
                  <>
                    {proposals.data === undefined ? (
                      <p className="text-xs text-muted-foreground">{t("value.loading")}</p>
                    ) : offered.length === 0 ? (
                      <p className="text-xs text-muted-foreground">{t("bank.no_proposal")}</p>
                    ) : (
                      <ol className="space-y-1">
                        {offered.map((proposal, number) => (
                          <li key={proposalKey(proposal)} className="flex items-start gap-2">
                            <kbd className="mt-0.5 rounded-sm border bg-muted px-1 font-mono text-xs">{number + 1}</kbd>
                            <span className="min-w-0 flex-1">
                              <span className="block">{proposalText(proposal)}</span>
                              <span className="block text-xs text-muted-foreground">
                                {t("bank.reason", { reason: proposal.reason, score: proposal.score })}
                              </span>
                            </span>
                            {canMatch && !(proposal.kind === "account" && !proposal.account_id) ? (
                              <Button
                                size="sm"
                                variant="outline"
                                tabIndex={-1}
                                busy={busy === lineId}
                                onClick={() => accept(line, proposal)}
                              >
                                {number === 0 ? <CornerDownLeft aria-hidden="true" /> : null}
                                {t("bank.accept")}
                              </Button>
                            ) : null}
                          </li>
                        ))}
                      </ol>
                    )}
                    {canMatch ? (
                      <div className="flex flex-wrap items-center gap-2">
                        <Lookup
                          id={`match-account-${lineId}`}
                          aria-label={t("bank.account_for", { line: index + 1 })}
                          placeholder={t("bank.post_to_account")}
                          className="w-64 max-w-full"
                          entity="account"
                          keyField="id"
                          value={accounts[lineId] ?? null}
                          companyId={company?.id}
                          allowCreate={false}
                          recordLink={false}
                          onChange={(next) => {
                            setAccounts((previous) => ({ ...previous, [lineId]: next }));
                            if (next) void match(line, { action: "account", account_id: next });
                          }}
                        />
                      </div>
                    ) : null}
                  </>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/** What a proposal is, as a key: the posting, the items, or the account it names. */
function proposalKey(proposal: Proposal): string {
  return [proposal.kind, proposal.posting?.posting_id, ...proposal.open_item_ids, proposal.account_id]
    .filter(Boolean)
    .join("|");
}

function isMatched(line: JsonObject): boolean {
  return text(line, "posting_id") !== null || text(line, "payment_id") !== null || text(line, "account_id") !== null;
}

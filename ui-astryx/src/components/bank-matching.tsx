// The matching workspace of a draft bank statement: every line with what it probably is
// (`bank_statement.proposals`, best first), accepted with `bank_statement.match`. Built for the
// keyboard: arrows (or j and k) move between lines, Enter takes the best proposal and 1 to 9 a
// particular one, `a` goes to the account, `x` clears. A line matched to open items gets a drafted
// payment, posted here through its own preview.

import { Badge } from "@astryxdesign/core/Badge";
import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { Icon } from "@astryxdesign/core/Icon";
import { Link } from "@astryxdesign/core/Link";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { Check, CornerDownLeft, X } from "lucide-react";
import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { ApiError, api } from "@/api/client";
import type { BankStatementProposalsOutput } from "@/api/generated";
import type { DocTypeMeta, DocumentRecord, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { formatDecimal, isZeroDecimal } from "@/lib/decimal";
import { forgetDocuments, useDocumentHeader } from "@/lib/doc-labels";
import { canRun, findCommand, findDocType, toAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { ActionDialog } from "./actions";
import { LinkLabel } from "./field-value";
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

function isMatched(line: JsonObject): boolean {
  return text(line, "posting_id") !== null || text(line, "payment_id") !== null || text(line, "account_id") !== null;
}

function proposalKey(proposal: Proposal): string {
  return [proposal.kind, proposal.posting?.posting_id, ...proposal.open_item_ids, proposal.account_id].filter(Boolean).join("|");
}

function proposalText(proposal: Proposal): string {
  if (proposal.kind === "posting" && proposal.posting)
    return t("bank.proposal_posting", { number: proposal.posting.number ?? t("document.unnumbered"), date: formatCalendarDate(proposal.posting.posting_date) });
  if (proposal.kind === "open_items") return t("bank.proposal_open_items", { numbers: proposal.numbers.join(", "), party: proposal.party_name ?? "" });
  return t("bank.proposal_account");
}

/** A payment drafted for a line, and posting it from here. */
function DraftedPayment({ id, onPosted }: { id: string; onPosted: () => void }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const header = useDocumentHeader("payment", id);
  const [posting, setPosting] = useState(false);
  const post = findCommand(meta, "payment.post");
  const payments = findDocType(meta, "payment");
  if (!header) return null;
  const draft = header.status === "draft";
  const subject = `${payments?.label ?? ""} ${header.number ?? t("document.unnumbered")}`;
  return (
    <HStack gap={2} vAlign="center" wrap="wrap">
      <Link href={paths.doc(code, "payment", id)}>{subject}</Link>
      <Badge variant={draft ? "warning" : "success"} label={payments?.states.find((state) => state.name === header.status)?.label ?? header.status} />
      {draft && post && canRun(meta, post.name) ? (
        <Button label={t("bank.post_payment", { action: post.label })} size="sm" onClick={() => setPosting(true)} />
      ) : null}
      {posting && post ? (
        <ActionDialog
          action={toAction(post)}
          id={id}
          subject={subject}
          currency={company?.base_currency ?? ""}
          onClose={() => setPosting(false)}
          onDone={() => {
            setPosting(false);
            forgetDocuments();
            onPosted();
          }}
        />
      ) : null}
    </HStack>
  );
}

function Matched({ line, onPosted }: { line: JsonObject; onPosted: () => void }) {
  const payment = text(line, "payment_id");
  const account = text(line, "account_id");
  if (payment) return <DraftedPayment id={payment} onPosted={onPosted} />;
  if (account)
    return (
      <span>
        {t("bank.to_account")} <LinkLabel entity="account" keyField="id" value={account} linkify />
      </span>
    );
  if (text(line, "posting_id")) return <span>{t("bank.to_posting")}</span>;
  return null;
}

export function BankMatching({ doc, docType, onChanged }: { doc: DocumentRecord; docType: DocTypeMeta; onChanged: () => void }) {
  const { meta, currencyDecimals } = useApp();
  const { company } = useCompany();
  const currency = text(doc.body, "currency") ?? company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const lines = useMemo(() => (Array.isArray(doc.body.lines) ? doc.body.lines.filter(isObject) : []), [doc]);
  const proposals = useAsync(canRun(meta, PROPOSALS) ? `proposals|${doc.id}|${doc.version}` : null, (signal) =>
    api.run("bank_statement.proposals", { id: doc.id }, { signal }),
  );
  const byLine = new Map((proposals.data?.lines ?? []).map((entry) => [entry.line_id, entry]));
  const [active, setActive] = useState(() => Math.max(0, lines.findIndex((line) => !isMatched(line))));
  const [accounts, setAccounts] = useState<Record<string, string | null>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<{ line: string; message: string } | null>(null);
  const rows = useRef<(HTMLLIElement | null)[]>([]);
  const list = useRef<HTMLUListElement>(null);
  const canMatch = canRun(meta, MATCH);
  const collection = docType.collections.find((candidate) => candidate.name === "lines");
  const label = (name: string): string => collection?.fields.find((field) => field.name === name)?.label ?? name;
  const unmatched = lines.filter((line) => !isMatched(line)).length;

  // biome-ignore lint/correctness/useExhaustiveDependencies: `doc.version` is a new copy of the statement
  useEffect(() => {
    const focused = document.activeElement;
    if (focused === document.body || focused === null || list.current?.contains(focused)) rows.current[active]?.focus({ preventScroll: false });
  }, [active, doc.version]);

  const focusAccount = (lineId: string) => document.querySelector<HTMLElement>(`#match-account-${CSS.escape(lineId)} input`)?.focus();

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
      setError({ line: lineId, message: cause instanceof ApiError ? cause.message : String(cause) });
    }
    setBusy(null);
  }

  function accept(line: JsonObject, proposal: Proposal | undefined): void {
    if (!proposal) return;
    const lineId = text(line, "id") ?? "";
    if (proposal.kind === "posting" && proposal.posting) void match(line, { action: "posting", posting_id: proposal.posting.posting_id });
    else if (proposal.kind === "open_items") void match(line, { action: "open_items", open_item_ids: proposal.open_item_ids });
    else if (proposal.kind === "account") {
      const account = proposal.account_id ?? accounts[lineId] ?? null;
      if (account) void match(line, { action: "account", account_id: account });
      else focusAccount(lineId);
    }
  }

  function onKey(event: KeyboardEvent<HTMLLIElement>, index: number): void {
    if (event.target !== event.currentTarget) return;
    const line = lines[index];
    if (!line) return;
    const lineId = text(line, "id") ?? "";
    const offered = byLine.get(lineId)?.proposals ?? [];
    if (event.key === "ArrowDown" || event.key === "j") setActive(Math.min(index + 1, lines.length - 1));
    else if (event.key === "ArrowUp" || event.key === "k") setActive(Math.max(index - 1, 0));
    else if (event.key === "Enter" && canMatch && !isMatched(line)) accept(line, offered[0]);
    else if (/^[1-9]$/.test(event.key) && canMatch && !isMatched(line)) accept(line, offered[Number.parseInt(event.key, 10) - 1]);
    else if (event.key === "a" && canMatch) focusAccount(lineId);
    else if ((event.key === "x" || event.key === "Delete") && canMatch && isMatched(line)) void match(line, { action: "clear" });
    else return;
    event.preventDefault();
  }

  return (
    <section aria-label={t("bank.matching")}>
      <VStack gap={2}>
        <HStack gap={4} vAlign="center" wrap="wrap">
          <Heading level={2}>{t("bank.matching")}</Heading>
          <Text type="supporting">{t("bank.to_match", { count: unmatched, total: lines.length })}</Text>
          <span className="push-end">
            <Text type="supporting">{t("bank.keys")}</Text>
          </span>
        </HStack>
        {proposals.error ? <Banner status="error" title={t("bank.proposals_failed")} description={proposals.error.message} /> : null}
        <ul ref={list} className="match-list">
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
                className="match-row"
                data-active={index === active || undefined}
              >
                <VStack gap={0.5}>
                  <HStack gap={2} vAlign="center">
                    <Text type="supporting" hasTabularNumbers>
                      {index + 1}
                    </Text>
                    <span className="nowrap">{text(line, "booking_date") ? formatCalendarDate(text(line, "booking_date") ?? "") : null}</span>
                    <Text weight="medium" maxLines={1}>
                      {text(line, "counterparty_name") ?? ""}
                    </Text>
                  </HStack>
                  <Text type="supporting" maxLines={1}>
                    {text(line, "reference") ?? ""}
                  </Text>
                </VStack>
                <span className="num-block">
                  <Text weight="medium" hasTabularNumbers>
                    {amount ? formatDecimal(amount, { minFraction: decimals }) : ""}
                  </Text>
                  {difference && !isZeroDecimal(difference) ? (
                    <Text type="supporting" display="block">
                      {`${label("difference")} ${formatDecimal(difference, { minFraction: decimals })}`}
                    </Text>
                  ) : null}
                </span>
                <VStack gap={1.5}>
                  {error?.line === lineId ? <Banner status="error" title={error.message} /> : null}
                  {matched ? (
                    <HStack gap={2} vAlign="center" wrap="wrap">
                      <Icon icon={Check} color="success" label={t("bank.matched")} />
                      <Matched
                        line={line}
                        onPosted={() => {
                          onChanged();
                          advance(index);
                        }}
                      />
                      {canMatch ? (
                        <span className="push-end">
                          <Button label={t("bank.clear")} size="sm" variant="ghost" icon={<Icon icon={X} />} isLoading={busy === lineId} onClick={() => void match(line, { action: "clear" })} />
                        </span>
                      ) : null}
                    </HStack>
                  ) : (
                    <>
                      {proposals.data === undefined ? (
                        <Text type="supporting">{t("value.loading")}</Text>
                      ) : offered.length === 0 ? (
                        <Text type="supporting">{t("bank.no_proposal")}</Text>
                      ) : (
                        <VStack as="ol" gap={1}>
                          {offered.map((proposal, number) => (
                            <HStack as="li" key={proposalKey(proposal)} gap={2} vAlign="start">
                              <Badge label={String(number + 1)} />
                              <VStack gap={0}>
                                <Text>{proposalText(proposal)}</Text>
                                <Text type="supporting">{t("bank.reason", { reason: proposal.reason, score: proposal.score })}</Text>
                              </VStack>
                              {canMatch && !(proposal.kind === "account" && !proposal.account_id) ? (
                                <span className="push-end">
                                  <Button
                                    label={t("bank.accept")}
                                    size="sm"
                                    icon={number === 0 ? <Icon icon={CornerDownLeft} /> : undefined}
                                    isLoading={busy === lineId}
                                    onClick={() => accept(line, proposal)}
                                  />
                                </span>
                              ) : null}
                            </HStack>
                          ))}
                        </VStack>
                      )}
                      {canMatch ? (
                        <div id={`match-account-${lineId}`} className="report-filter">
                          <Lookup
                            label={t("bank.account_for", { line: index + 1 })}
                            isLabelHidden
                            size="sm"
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
                </VStack>
              </li>
            );
          })}
        </ul>
      </VStack>
    </section>
  );
}

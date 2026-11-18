import { useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ApiError, api } from "@/api/client";
import type { EntityName } from "@/api/generated";
import type { OpenItem } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { ActionDialog } from "@/components/command-action";
import { type Column, DataTable } from "@/components/data-table";
import { Amount, Empty, LinkLabel } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader } from "@/components/page";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { TableCell, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { formatCalendarDate, todayCalendarDate } from "@/lib/dates";
import { formatDecimal, isZeroDecimal } from "@/lib/decimal";
import { forgetDocuments } from "@/lib/doc-labels";
import { ACCOUNT, canDoc, canRun, findCommand, findDocType, findEntity, toAction } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { pageFromUrl, useUrlState } from "@/lib/url-state";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

const PAGE_SIZE = 50;

// Whom each kind of debt is with. The command takes the party's type by name;
// these are typed as entity names so a rename on the server breaks the build.
const PARTY: Record<"payable" | "receivable", EntityName> = { payable: "vendor", receivable: "customer" };

function isKind(value: string): value is "payable" | "receivable" {
  return value === "payable" || value === "receivable";
}

/** What the company owes and is owed, invoice by invoice, oldest first. */
export function OpenItemsPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const url = useUrlState();
  const kindParam = url.get("kind");
  const kind = isKind(kindParam) ? kindParam : null;
  const party = kind ? url.get("party") : "";
  const settled = url.get("settled") === "1";
  const page = pageFromUrl(url.get("page"));
  const allowed = canRun(meta, "open_item.list");
  const partyEntity = kind ? findEntity(meta, PARTY[kind]) : undefined;

  const list = useAsync(allowed && company ? `${company.id}|${kind}|${party}|${settled}|${page}` : null, (signal) =>
    api.run(
      "open_item.list",
      {
        company_id: company?.id ?? "",
        kind,
        party_type: kind && party ? PARTY[kind] : null,
        party_id: party || null,
        include_settled: settled,
        limit: PAGE_SIZE,
        offset: (page - 1) * PAGE_SIZE,
      },
      { signal },
    ),
  );

  const currency = company?.base_currency ?? "";
  const decimals = currencyDecimals[currency] ?? 2;
  const columns = useMemo<Column<OpenItem>[]>(
    () => [
      {
        id: "document",
        header: t("ledger.document"),
        cell: (item) => (
          <span className="whitespace-nowrap">
            {findDocType(meta, item.doc_type)?.label ?? item.doc_type}{" "}
            <span className="font-medium tabular-nums">{item.number ?? t("document.unnumbered")}</span>
          </span>
        ),
      },
      {
        id: "date",
        header: t("document.date"),
        cell: (item) => <span className="whitespace-nowrap">{formatCalendarDate(item.doc_date)}</span>,
      },
      {
        id: "due",
        header: t("open_items.due"),
        cell: (item) =>
          item.due_date ? <span className="whitespace-nowrap">{formatCalendarDate(item.due_date)}</span> : <Empty />,
      },
      {
        id: "discount",
        header: t("open_items.discount"),
        align: "end",
        // What may be kept back when paying by the discount date, as the item says.
        cell: (item) =>
          item.discount_amount && item.discount_date ? (
            <span className="whitespace-nowrap">
              <Amount value={item.discount_amount} decimals={decimals} />
              <span className="block text-xs text-muted-foreground">
                {t("open_items.until", { date: formatCalendarDate(item.discount_date) })}
              </span>
            </span>
          ) : null,
      },
      {
        id: "kind",
        header: t("open_items.kind"),
        cell: (item) => (
          <span className="flex gap-1">
            <Badge tone={item.kind === "payable" ? "warning" : "info"}>
              {t(item.kind === "payable" ? "open_items.payable" : "open_items.receivable")}
            </Badge>
            {/* A negative item is owed the other way: a credit note, or money paid on account. */}
            {item.amount.trim().startsWith("-") ? <Badge tone="neutral">{t("open_items.credit")}</Badge> : null}
          </span>
        ),
      },
      {
        id: "party",
        header: t("open_items.party"),
        className: "w-full",
        cell: (item) => <LinkLabel entity={item.party_type} keyField="id" value={item.party_id} />,
      },
      {
        id: "amount",
        header: t("report.amount"),
        align: "end",
        cell: (item) => <Amount value={item.amount} decimals={decimals} />,
      },
      {
        id: "settled",
        header: t("open_items.settled"),
        align: "end",
        cell: (item) => <Amount value={item.settled} decimals={decimals} />,
      },
      {
        id: "open",
        header: t("open_items.open"),
        align: "end",
        cell: (item) => (
          <span className="whitespace-nowrap">
            <Amount value={item.open} decimals={decimals} />
            {/* An item in another currency: what is open in its own currency, as the bank will see it. */}
            {item.currency && item.currency !== currency ? (
              <span className="block text-xs text-muted-foreground">
                {t("postings.in_currency", {
                  amount: formatDecimal(item.txn_open, { minFraction: currencyDecimals[item.currency] ?? 2 }),
                  currency: item.currency,
                })}
              </span>
            ) : null}
          </span>
        ),
      },
    ],
    [meta, decimals, currency, currencyDecimals],
  );

  // Paying: the items chosen, all of one party, settled by one draft payment.
  const navigate = useNavigate();
  const payCommand = findCommand(meta, "payment.draft");
  const canPay = payCommand !== undefined && canRun(meta, payCommand.name) && company !== null;
  const [chosen, setChosen] = useState<Map<string, OpenItem>>(() => new Map());
  const [bank, setBank] = useState<string | null>(null);
  const [paying, setPaying] = useState(false);
  const [payError, setPayError] = useState<ApiError | null>(null);
  const payKey = useRef<{ input: string; key: string }>({ input: "", key: newKey() });
  const payee = chosen.size > 0 ? [...chosen.values()][0] : undefined;
  // Chosen items whose early-payment discount can still be taken (dates compare as text: YYYY-MM-DD).
  const today = todayCalendarDate();
  const discounts = [...chosen.values()].filter(
    (item) =>
      item.discount_amount && item.discount_date && item.discount_date >= today && !isZeroDecimal(item.discount_amount),
  );
  // The bank account is offered as the payment's own field offers it (only bank and cash accounts).
  const bankField = findDocType(meta, payCommand?.creates)?.fields.find((field) => field.name === "bank_account_id");
  const bankFilters = bankField?.type.kind === "link" ? bankField.type.filters : undefined;
  // One credit and one debt of the same party can be set against each other without money moving.
  const applyCommand = findCommand(meta, "payment.apply_credit");
  const canApply = applyCommand !== undefined && canRun(meta, applyCommand.name);
  const chosenItems = [...chosen.values()];
  const credit = chosenItems.find((item) => item.open.trim().startsWith("-"));
  const debt = chosenItems.find((item) => !item.open.trim().startsWith("-"));
  const applicable = canApply && chosen.size === 2 && credit !== undefined && debt !== undefined;
  const [applying, setApplying] = useState(false);

  async function pay(): Promise<void> {
    if (!payCommand || !company || !bank || chosen.size === 0) return;
    const input = { company_id: company.id, open_item_ids: [...chosen.keys()], bank_account_id: bank };
    const text = JSON.stringify(input);
    if (payKey.current.input !== text) payKey.current = { input: text, key: newKey() };
    setPaying(true);
    setPayError(null);
    try {
      const draft = await api.run("payment.draft", input, { idempotencyKey: payKey.current.key });
      forgetDocuments();
      navigate(paths.doc(code, payCommand.creates ?? "payment", draft.id));
    } catch (cause) {
      setPayError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setPaying(false);
    }
  }

  if (!allowed) return <NotFoundPage />;
  const filtered = kind !== null || settled;

  return (
    <>
      <PageHeader title={t("report.open_items")} />
      <div className="mb-3 flex flex-wrap items-end gap-3">
        <div className="space-y-1">
          <Label htmlFor="open-kind">{t("open_items.kind")}</Label>
          <Select
            id="open-kind"
            className="w-60"
            value={kind ?? ""}
            onChange={(event) => url.set({ kind: event.target.value, party: null, page: null })}
          >
            <option value="">{t("open_items.both")}</option>
            <option value="payable">{t("open_items.payables")}</option>
            <option value="receivable">{t("open_items.receivables")}</option>
          </Select>
        </div>
        {kind && partyEntity ? (
          <div className="space-y-1">
            <Label htmlFor="open-party">{partyEntity.label}</Label>
            <Lookup
              id="open-party"
              className="w-72 max-w-full"
              entity={partyEntity.name}
              keyField="id"
              value={party || null}
              companyId={company?.id}
              allowCreate={false}
              onChange={(next) => url.set({ party: next, page: null })}
            />
          </div>
        ) : null}
        <label className="flex h-8 items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="size-4 accent-primary"
            checked={settled}
            onChange={(event) => url.set({ settled: event.target.checked ? "1" : null, page: null })}
          />
          {t("open_items.include_settled")}
        </label>
        <p className="ms-auto text-sm text-muted-foreground">
          {company ? t("report.amounts_in", { company: company.name, currency }) : null}
        </p>
      </div>
      {canPay ? (
        <div className="mb-3 flex flex-wrap items-end gap-3 rounded-md border bg-card p-3">
          <p className="min-w-0 flex-1 text-sm" aria-live="polite">
            {payee ? (
              <>
                <span className="font-medium">
                  <LinkLabel entity={payee.party_type} keyField="id" value={payee.party_id} />
                </span>
                {t("value.separator")}
                {t("open_items.chosen", { count: chosen.size })}
                {discounts.length > 0 ? (
                  <span className="mt-1 block text-success">
                    {discounts.map((item) => (
                      <span key={item.id} className="block">
                        {t("open_items.discount_open", {
                          number: item.number ?? "",
                          amount: formatDecimal(item.discount_amount ?? "0", { minFraction: decimals }),
                          date: formatCalendarDate(item.discount_date ?? ""),
                        })}
                      </span>
                    ))}
                  </span>
                ) : null}
              </>
            ) : (
              <span className="text-muted-foreground">{t("open_items.pay_hint")}</span>
            )}
          </p>
          <div className="space-y-1">
            <Label htmlFor="pay-bank">{t("open_items.bank")}</Label>
            <Lookup
              id="pay-bank"
              className="w-72 max-w-full"
              entity={ACCOUNT}
              keyField="id"
              filters={bankFilters}
              value={bank}
              companyId={company?.id}
              allowCreate={false}
              onChange={(next) => setBank(next)}
            />
          </div>
          {chosen.size > 0 ? (
            <Button variant="outline" onClick={() => setChosen(new Map())} disabled={paying}>
              {t("open_items.clear")}
            </Button>
          ) : null}
          {canApply ? (
            <Button
              variant="outline"
              onClick={() => setApplying(true)}
              disabled={!applicable || paying}
              title={applicable ? applyCommand?.description : t("open_items.apply_hint")}
            >
              {applyCommand?.label}
            </Button>
          ) : null}
          <Button onClick={() => void pay()} disabled={chosen.size === 0 || !bank} busy={paying}>
            {payCommand?.label ?? t("open_items.pay")}
          </Button>
        </div>
      ) : null}
      {applying && applyCommand && company && credit && debt ? (
        <ActionDialog
          action={toAction(applyCommand)}
          input={{ company_id: company.id, credit_item_id: credit.id, debt_item_id: debt.id }}
          subject={[credit.number, debt.number].filter(Boolean).join(t("value.separator"))}
          current={{}}
          fields={meta.document_header}
          docType={findDocType(meta, applyCommand.creates)}
          onClose={() => setApplying(false)}
          onDone={(result) => {
            setApplying(false);
            forgetDocuments();
            const id = typeof result === "object" && result !== null && !Array.isArray(result) ? result.id : null;
            if (typeof id === "string") navigate(paths.doc(code, applyCommand.creates ?? "payment", id));
          }}
        />
      ) : null}
      {payError ? (
        <Alert className="mb-3" title={t("open_items.pay_failed")}>
          {payError.message}
        </Alert>
      ) : null}
      <DataTable
        caption={t("report.open_items")}
        selection={
          canPay
            ? {
                selected: new Set(chosen.keys()),
                label: (item) => t("open_items.select", { number: item.number ?? item.id }),
                blocked: (item) =>
                  isZeroDecimal(item.open)
                    ? t("open_items.settled_already")
                    : payee && (payee.party_id !== item.party_id || payee.kind !== item.kind)
                      ? t("open_items.other_party")
                      : null,
                onToggle: (item, on) =>
                  setChosen((previous) => {
                    const next = new Map(previous);
                    if (on) next.set(item.id, item);
                    else next.delete(item.id);
                    return next;
                  }),
              }
            : undefined
        }
        columns={columns}
        rows={company ? list.data?.items : []}
        rowKey={(item) => item.id}
        rowHref={(item) =>
          item.doc_type && item.document_id && canDoc(meta, item.doc_type, "read")
            ? paths.doc(code, item.doc_type, item.document_id)
            : undefined
        }
        loading={list.loading}
        error={list.error}
        onRetry={list.reload}
        page={page}
        pageSize={PAGE_SIZE}
        total={list.data?.total ?? 0}
        onPage={(next) => url.set({ page: next === 1 ? null : String(next) })}
        empty={
          !company ? (
            <p>{t("report.needs_company")}</p>
          ) : filtered ? (
            <>
              <p>{t("open_items.no_match")}</p>
              <Button
                variant="outline"
                size="sm"
                onClick={() => url.set({ kind: null, party: null, settled: null, page: null })}
              >
                {t("list.clear_filters")}
              </Button>
            </>
          ) : (
            <p>{t("open_items.empty", { company: company.name })}</p>
          )
        }
        footer={
          list.data ? (
            <TableRow>
              <TableCell colSpan={canPay ? 9 : 8}>{t("open_items.total_open")}</TableCell>
              <TableCell className="text-end">
                <Amount value={list.data.total_open} decimals={decimals} />
              </TableCell>
            </TableRow>
          ) : null
        }
      />
    </>
  );
}

// An incoming e-invoice (XRechnung in UBL or CII, or a ZUGFeRD / Factur-X PDF):
// `einvoice.read` shows what it says and what it would become (the vendor,
// the purchase order, the order line and tax code of each line, the rules it
// breaks) before anything is created; `einvoice.import` then drafts the
// vendor invoice, which opens.

import { Upload } from "lucide-react";
import { useState } from "react";
import { Link, useNavigate } from "react-router";
import { ApiError, api } from "@/api/client";
import type { EinvoiceImportInput, EinvoiceReadOutput } from "@/api/generated";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Badge } from "@/components/badge";
import { Button } from "@/components/button";
import { Amount, Empty, LinkLabel } from "@/components/field-value";
import { Lookup } from "@/components/lookup";
import { PageHeader, Section } from "@/components/page";
import { Label } from "@/components/ui/label";
import { NativeSelect } from "@/components/ui/native-select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { t } from "@/i18n/t";
import { formatCalendarDate } from "@/lib/dates";
import { forgetDocuments } from "@/lib/doc-labels";
import { commandInputs, findCommand, findDocType } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { DocumentLinkLabel } from "./einvoice-parts";

export const EINVOICE_IMPORT = "einvoice.import";

/** A file as base64, which takes XML and PDF alike. */
async function base64Of(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let start = 0; start < bytes.length; start += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(start, start + 0x8000));
  }
  return btoa(binary);
}

export function EinvoiceImportPage() {
  const { meta, currencyDecimals } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const command = findCommand(meta, EINVOICE_IMPORT);
  const inputs = command ? commandInputs(command) : [];
  const input = (name: string) => inputs.find((candidate) => candidate.name === name);
  const invoices = findDocType(meta, command?.creates);

  const [file, setFile] = useState<File | null>(null);
  const [content, setContent] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [proposal, setProposal] = useState<EinvoiceReadOutput | null>(null);
  const [vendor, setVendor] = useState<string | null>(null);
  const [account, setAccount] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [key] = useState(newKey);

  async function read(chosen: File): Promise<void> {
    if (!company) return;
    setReading(true);
    setError(null);
    setProposal(null);
    try {
      const encoded = await base64Of(chosen);
      setContent(encoded);
      const answer = await api.run("einvoice.read", {
        company_id: company.id,
        content_base64: encoded,
      });
      setProposal(answer);
      setVendor(answer.vendor_id ?? answer.candidates[0]?.vendor_id ?? null);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setReading(false);
  }

  async function create(): Promise<void> {
    if (!company || !content || !proposal) return;
    setImporting(true);
    setError(null);
    const body: EinvoiceImportInput = { company_id: company.id, content_base64: content };
    if (vendor && vendor !== proposal.vendor_id) body.vendor_id = vendor;
    if (account) body.account_id = account;
    try {
      const doc = await api.run("einvoice.import", body, { idempotencyKey: key });
      forgetDocuments();
      navigate(paths.doc(code, doc.doc_type, doc.id));
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setImporting(false);
    }
  }

  if (!command) return null;
  const invoice = proposal?.invoice;
  const decimals = currencyDecimals[invoice?.currency ?? ""] ?? 2;
  const fatal = proposal?.findings.filter((finding) => finding.fatal) ?? [];
  const advice = proposal?.findings.filter((finding) => !finding.fatal) ?? [];
  const unmatched = proposal?.lines.some((line) => !line.order_line_id) ?? false;
  const blocked = !proposal || fatal.length > 0 || proposal.imported_as !== null || !vendor || (unmatched && !account);

  return (
    <>
      <PageHeader
        crumbs={invoices ? [{ label: invoices.plural, to: paths.docList(code, invoices.name) }] : []}
        title={command.label}
      />
      <div className="max-w-5xl space-y-5">
        <p className="max-w-form text-sm text-muted-foreground">{command.description}</p>
        <div className="max-w-form space-y-1 rounded-md border bg-card p-4">
          <Label htmlFor="einvoice-file">{t("einvoice.file")}</Label>
          <input
            id="einvoice-file"
            type="file"
            accept=".xml,.pdf,application/xml,text/xml,application/pdf"
            className="block w-full text-sm file:me-3 file:rounded-md file:border file:bg-card file:px-3 file:py-1 file:text-sm"
            onChange={(event) => {
              const chosen = event.target.files?.[0] ?? null;
              setFile(chosen);
              if (chosen) void read(chosen);
            }}
          />
          <p className="text-sm text-muted-foreground">{input("content_base64")?.description}</p>
          {reading ? <p className="text-sm text-muted-foreground">{t("einvoice.reading")}</p> : null}
        </div>

        {error ? (
          <Alert title={proposal ? t("einvoice.import_failed") : t("einvoice.read_failed")}>{error.message}</Alert>
        ) : null}

        {proposal && invoice ? (
          <>
            {proposal.imported_as ? (
              <Alert tone="info" title={t("einvoice.imported_before")}>
                <DocumentLinkLabel docType={command.creates ?? ""} id={proposal.imported_as} />
              </Alert>
            ) : null}
            {fatal.length > 0 ? (
              <Alert title={t("einvoice.breaks_rules", { count: fatal.length })}>
                <ul className="list-disc ps-4">
                  {fatal.map((finding) => (
                    <li key={`${finding.rule}|${finding.message}`}>
                      <code className="font-mono text-xs">{finding.rule}</code> {finding.message}
                    </li>
                  ))}
                </ul>
              </Alert>
            ) : null}

            <div className="grid gap-4 lg:grid-cols-2">
              <Section title={t("einvoice.invoice")}>
                <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-md border bg-card p-4 text-sm">
                  <dt className="text-muted-foreground">{t("einvoice.number")}</dt>
                  <dd className="font-medium">{invoice.number}</dd>
                  <dt className="text-muted-foreground">{t("einvoice.seller")}</dt>
                  <dd>
                    {invoice.seller.name}
                    {invoice.seller.vat_id ? (
                      <span className="ms-2 text-sm text-muted-foreground">{invoice.seller.vat_id}</span>
                    ) : null}
                  </dd>
                  <dt className="text-muted-foreground">{t("einvoice.issued")}</dt>
                  <dd>{formatCalendarDate(invoice.issue_date)}</dd>
                  <dt className="text-muted-foreground">{t("einvoice.due")}</dt>
                  <dd>{invoice.due_date ? formatCalendarDate(invoice.due_date) : <Empty />}</dd>
                  <dt className="text-muted-foreground">{t("einvoice.order_reference")}</dt>
                  <dd>{invoice.order_reference ?? <Empty />}</dd>
                  <dt className="text-muted-foreground">{t("einvoice.net")}</dt>
                  <dd>
                    <Amount value={invoice.totals.tax_exclusive} decimals={decimals} />{" "}
                    <span className="text-sm text-muted-foreground">{invoice.currency}</span>
                  </dd>
                  <dt className="text-muted-foreground">{t("einvoice.tax")}</dt>
                  <dd>
                    <Amount value={invoice.totals.tax_total} decimals={decimals} />
                  </dd>
                  <dt className="font-medium">{t("einvoice.payable")}</dt>
                  <dd className="font-semibold">
                    <Amount value={invoice.totals.payable} decimals={decimals} />{" "}
                    <span className="text-sm font-normal text-muted-foreground">{invoice.currency}</span>
                  </dd>
                  <dt className="text-muted-foreground">{t("einvoice.format")}</dt>
                  <dd className="text-sm text-muted-foreground">
                    {[proposal.syntax.toUpperCase(), proposal.container, invoice.specification].join(
                      t("value.separator"),
                    )}
                  </dd>
                </dl>
              </Section>

              <Section title={t("einvoice.becomes")}>
                <div className="space-y-3 rounded-md border bg-card p-4 text-sm">
                  <div className="space-y-1">
                    <Label htmlFor="einvoice-vendor">{input("vendor_id")?.label}</Label>
                    {proposal.vendor_id ? (
                      <p>
                        <LinkLabel entity="vendor" keyField="id" value={proposal.vendor_id} linkify />{" "}
                        <span className="text-sm text-muted-foreground">{t("einvoice.vendor_by_vat")}</span>
                      </p>
                    ) : proposal.candidates.length > 0 ? (
                      <>
                        <NativeSelect
                          className="w-full"
                          id="einvoice-vendor"
                          value={vendor ?? ""}
                          onChange={(event) => setVendor(event.target.value || null)}
                        >
                          {proposal.candidates.map((candidate) => (
                            <option key={candidate.vendor_id} value={candidate.vendor_id}>
                              {t("einvoice.candidate", {
                                code: candidate.code ?? "",
                                name: candidate.name ?? "",
                                by: candidate.matched_by,
                              })}
                            </option>
                          ))}
                        </NativeSelect>
                        <p className="text-sm text-muted-foreground">{t("einvoice.vendor_confirm")}</p>
                      </>
                    ) : (
                      <>
                        <Lookup
                          id="einvoice-vendor"
                          entity="vendor"
                          keyField="id"
                          value={vendor}
                          companyId={company?.id}
                          allowCreate={false}
                          onChange={setVendor}
                        />
                        <p className="text-sm text-muted-foreground">{t("einvoice.vendor_unknown")}</p>
                      </>
                    )}
                  </div>
                  <div>
                    <span className="text-muted-foreground">{t("einvoice.order")}</span>{" "}
                    {proposal.purchase_order_id ? (
                      <DocumentLinkLabel docType="purchase_order" id={proposal.purchase_order_id} />
                    ) : (
                      <span>{t("einvoice.no_order")}</span>
                    )}
                  </div>
                  {unmatched ? (
                    <div className="space-y-1">
                      <Label htmlFor="einvoice-account">{input("account_id")?.label}</Label>
                      <Lookup
                        id="einvoice-account"
                        entity="account"
                        keyField="id"
                        value={account}
                        companyId={company?.id}
                        allowCreate={false}
                        onChange={setAccount}
                      />
                      <p className="text-sm text-muted-foreground">{input("account_id")?.description}</p>
                    </div>
                  ) : null}
                </div>
              </Section>
            </div>

            <Section title={t("einvoice.lines")}>
              <div className="rounded-md border bg-card">
                <Table aria-label={t("einvoice.lines")}>
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("einvoice.line")}</TableHead>
                      <TableHead className="w-full">{t("einvoice.line_name")}</TableHead>
                      <TableHead className="text-end">{t("einvoice.quantity")}</TableHead>
                      <TableHead className="text-end">{t("einvoice.net")}</TableHead>
                      <TableHead>{t("einvoice.goes_to")}</TableHead>
                      <TableHead>{t("einvoice.tax_code")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {proposal.lines.map((line) => (
                      <TableRow key={line.line}>
                        <TableCell>{line.line}</TableCell>
                        <TableCell>{line.name}</TableCell>
                        <TableCell className="text-end tabular-nums">{line.quantity}</TableCell>
                        <TableCell className="text-end">
                          <Amount value={line.net_amount} decimals={decimals} />
                        </TableCell>
                        <TableCell className="whitespace-nowrap">
                          {line.order_line_id ? (
                            <Badge tone="success">{t("einvoice.order_line", { by: line.matched_by ?? "" })}</Badge>
                          ) : (
                            <Badge tone="warning">{t("einvoice.to_account")}</Badge>
                          )}
                        </TableCell>
                        <TableCell className="whitespace-nowrap">
                          {line.tax_code_id ? (
                            <LinkLabel entity="tax_code" keyField="id" value={line.tax_code_id} />
                          ) : (
                            <Empty />
                          )}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            </Section>

            {advice.length > 0 ? (
              <Section title={t("einvoice.advice")}>
                <ul className="list-disc space-y-0.5 ps-5 text-sm text-muted-foreground">
                  {advice.map((finding) => (
                    <li key={`${finding.rule}|${finding.message}`}>
                      <code className="font-mono text-xs">{finding.rule}</code> {finding.message}
                    </li>
                  ))}
                </ul>
              </Section>
            ) : null}

            <div className="flex items-center gap-3">
              <Button busy={importing} disabled={blocked || importing} onClick={() => void create()}>
                <Upload aria-hidden="true" />
                {t("einvoice.create_draft", { label: invoices?.label ?? "" })}
              </Button>
              {file ? <span className="text-sm text-muted-foreground">{file.name}</span> : null}
              {invoices ? (
                <Link className="ms-auto text-sm underline underline-offset-2" to={paths.docList(code, invoices.name)}>
                  {invoices.plural}
                </Link>
              ) : null}
            </div>
          </>
        ) : null}
      </div>
    </>
  );
}

// Importing a bank statement: the bank account, the file's format, and the file,
// read in the browser and sent as text to `bank_statement.import`. A CSV file is
// read with the account's saved column mapping (`bank_csv_mapping`), which is
// edited on its own record page; this page leads there. The server refuses a
// malformed file with the line it is about, which is shown as it says it.

import { Upload } from "lucide-react";
import { type FormEvent, useMemo, useRef, useState } from "react";
import { Link, useNavigate } from "react-router";
import { ApiError, api } from "@/api/client";
import type { EntityName } from "@/api/generated";
import type { JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Lookup } from "@/components/lookup";
import { MoneyInput } from "@/components/money-input";
import { PageHeader } from "@/components/page";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { t } from "@/i18n/t";
import { forgetDocuments } from "@/lib/doc-labels";
import { useLinkInfo } from "@/lib/link-labels";
import { canEntity, canRun, commandInputs, findCommand, findDocType, findEntity } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

export const BANK_IMPORT = "bank_statement.import";
const CSV_MAPPING: EntityName = "bank_csv_mapping";

export function BankImportPage() {
  const { meta } = useApp();
  const command = findCommand(meta, BANK_IMPORT);
  if (!command || !canRun(meta, command.name)) return <NotFoundPage />;
  return <BankImport />;
}

function BankImport() {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const navigate = useNavigate();
  const command = findCommand(meta, BANK_IMPORT);
  const inputs = useMemo(() => (command ? commandInputs(command) : []), [command]);
  const input = (name: string) => inputs.find((candidate) => candidate.name === name);
  const accountType = input("bank_account_id")?.fieldType;
  const formats = input("format")?.options ?? [];
  const statements = findDocType(meta, command?.creates);

  const [account, setAccount] = useState<string | null>(null);
  const [format, setFormat] = useState<string>(formats[0]?.value ?? "");
  const [file, setFile] = useState<File | null>(null);
  const [opening, setOpening] = useState("");
  // The opening balance is in the account's currency, or the company's where the account names none.
  const { currencyDecimals } = useApp();
  const accountInfo = useLinkInfo(accountType?.kind === "link" ? accountType.entity : null, "id", account);
  const accountCurrency = accountInfo?.record.currency;
  const currency = typeof accountCurrency === "string" && accountCurrency ? accountCurrency : company?.base_currency;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const key = useRef<{ input: string; key: string }>({ input: "", key: newKey() });

  // A CSV file is read with the account's saved mapping: find it, or offer to make one.
  const mappingEntity = findEntity(meta, CSV_MAPPING);
  const mapping = useAsync(
    format === "csv" && account && mappingEntity && canEntity(meta, CSV_MAPPING, "read") ? `mapping|${account}` : null,
    async (signal) => {
      const page = await api.run(
        "entity.list",
        { entity: CSV_MAPPING, filters: { bank_account_id: account ?? "" }, limit: 1 },
        { signal },
      );
      return page.items[0] ?? null;
    },
  );

  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (!command || !account || !file || !format) return;
    setBusy(true);
    setError(null);
    try {
      const content = await file.text();
      const body: JsonObject = { bank_account_id: account, format, content };
      if (opening.trim() !== "") body.opening_balance = opening.trim();
      const fingerprint = JSON.stringify(body);
      if (key.current.input !== fingerprint) key.current = { input: fingerprint, key: newKey() };
      const result = await api.runNamed(command.name, body, { idempotencyKey: key.current.key });
      forgetDocuments();
      const id = result && typeof result === "object" && !Array.isArray(result) ? result.id : null;
      if (typeof id === "string" && command.creates) navigate(paths.doc(code, command.creates, id));
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  const mappingNew = `${paths.entityNew(code, CSV_MAPPING)}?${new URLSearchParams({ bank_account_id: account ?? "" })}`;
  return (
    <>
      <PageHeader
        crumbs={statements ? [{ label: statements.plural, to: paths.docList(code, statements.name) }] : []}
        title={command?.label ?? BANK_IMPORT}
      />
      <form noValidate onSubmit={(event) => void submit(event)} className="w-full max-w-form space-y-4">
        <p className="text-sm text-muted-foreground">{command?.description}</p>
        {error ? (
          <Alert title={t("bank.import_failed")}>
            {error.line !== null ? <p className="font-medium">{t("bank.file_line", { line: error.line })}</p> : null}
            <p>{error.message}</p>
          </Alert>
        ) : null}
        <div className="grid gap-x-4 gap-y-3 rounded-md border bg-card p-4 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="import-account">{input("bank_account_id")?.label}</Label>
            <Lookup
              id="import-account"
              entity={accountType?.kind === "link" ? accountType.entity : "account"}
              keyField="id"
              filters={accountType?.kind === "link" ? accountType.filters : undefined}
              value={account}
              companyId={company?.id}
              required
              allowCreate={false}
              onChange={setAccount}
            />
          </div>
          <div className="space-y-1">
            <span className="text-sm font-medium" id="import-format-label">
              {input("format")?.label}
            </span>
            <div role="radiogroup" aria-labelledby="import-format-label" className="space-y-1">
              {formats.map((option) => (
                <label key={option.value} className="flex cursor-pointer items-start gap-2 text-sm">
                  <input
                    type="radio"
                    name="import-format"
                    value={option.value}
                    className="mt-0.5 size-4 accent-primary"
                    checked={format === option.value}
                    onChange={() => setFormat(option.value)}
                  />
                  <span>
                    <span className="font-medium">{option.value}</span>
                    {option.description ? (
                      <span className="block text-xs text-muted-foreground">{option.description}</span>
                    ) : null}
                  </span>
                </label>
              ))}
            </div>
          </div>
          <div className="space-y-1 sm:col-span-2">
            <Label htmlFor="import-file">{t("bank.file")}</Label>
            <input
              id="import-file"
              type="file"
              className="block w-full text-sm file:me-3 file:rounded-md file:border file:bg-card file:px-3 file:py-1 file:text-sm"
              onChange={(event) => setFile(event.target.files?.[0] ?? null)}
            />
            <p className="text-xs text-muted-foreground">{input("content")?.description}</p>
          </div>
          {format === "csv" ? (
            <>
              <div className="space-y-1">
                <Label htmlFor="import-opening">{input("opening_balance")?.label}</Label>
                <MoneyInput
                  id="import-opening"
                  value={opening}
                  onChange={setOpening}
                  minFraction={currency ? (currencyDecimals[currency] ?? 2) : 2}
                />
                <p className="text-xs text-muted-foreground">{input("opening_balance")?.description}</p>
              </div>
              <div className="space-y-1 text-sm">
                <span className="font-medium">{mappingEntity?.label}</span>
                {!account ? (
                  <p className="text-muted-foreground">{t("bank.choose_account_first")}</p>
                ) : mapping.data === undefined ? null : mapping.data ? (
                  <p>
                    <Link
                      to={paths.entityRecord(code, CSV_MAPPING, mapping.data.id)}
                      className="underline underline-offset-2 hover:no-underline"
                    >
                      {t("bank.edit_mapping")}
                    </Link>
                  </p>
                ) : (
                  <p>
                    {t("bank.no_mapping")}{" "}
                    <Link to={mappingNew} className="underline underline-offset-2 hover:no-underline">
                      {t("bank.new_mapping")}
                    </Link>
                  </p>
                )}
              </div>
            </>
          ) : null}
        </div>
        <Button type="submit" busy={busy} disabled={!account || !file || !format}>
          <Upload aria-hidden="true" />
          {command?.label}
        </Button>
      </form>
    </>
  );
}

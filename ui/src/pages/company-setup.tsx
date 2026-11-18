import { FormField } from "@/components/form-field";
// A new company, ready to work, from a template (`setup.templates`): its chart
// of accounts, fiscal years, warehouse, settings and tax codes, by one command
// (`company.setup`), all of it or nothing. The template is chosen from the
// list the server offers, the other inputs come from the command's schema,
// and the server's preview says what will be created before it is.

import { type FormEvent, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CompanySetupOutput } from "@/api/generated";
import type { Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { PageHeader, Section } from "@/components/page";
import { Input } from "@/components/ui/input";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { t } from "@/i18n/t";
import { COMPANY, canRun, commandInputs, findCommand } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { cn } from "@/lib/utils";
import { NotFoundPage } from "./not-found";

export const SETUP_COMMAND = "company.setup";

export function CompanySetupPage() {
  const { meta } = useApp();
  const command = findCommand(meta, SETUP_COMMAND);
  if (!command || !canRun(meta, command.name) || !canRun(meta, "setup.templates")) return <NotFoundPage />;
  return <Setup />;
}

function Setup() {
  const { meta, reloadCompanies } = useApp();
  const { code } = useCompany();
  const navigate = useNavigate();
  const command = findCommand(meta, SETUP_COMMAND);
  const templates = useAsync("setup-templates", (signal) => api.run("setup.templates", {}, { signal }));
  const inputs = useMemo(
    // What must be given first, then the rest, each group in the schema's order.
    () =>
      command
        ? commandInputs(command)
            .filter((input) => input.name !== "template")
            .sort((a, b) => (a.required === b.required ? 0 : a.required ? -1 : 1))
        : [],
    [command],
  );
  const [template, setTemplate] = useState<string | null>(null);
  const [values, setValues] = useState<Record<string, string>>(() => ({
    first_year: String(new Date().getFullYear()),
  }));
  const [preview, setPreview] = useState<SetupResult | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const key = useRef<{ input: string; key: string }>({ input: "", key: newKey() });

  function input(): JsonObject {
    const out: JsonObject = { template: template ?? "" };
    for (const property of inputs) {
      const value = (values[property.name] ?? "").trim();
      if (value === "") continue;
      out[property.name] = property.type === "integer" ? parseInt(value, 10) : value;
    }
    return out;
  }

  async function runPreview(event?: FormEvent): Promise<void> {
    event?.preventDefault();
    setPreviewing(true);
    setError(null);
    setPreview(null);
    try {
      const result = await api.preview(SETUP_COMMAND, input());
      if (isSetupResult(result)) setPreview(result);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setPreviewing(false);
  }

  async function setUp(): Promise<void> {
    const body = input();
    const text = JSON.stringify(body);
    if (key.current.input !== text) key.current = { input: text, key: newKey() };
    setRunning(true);
    setError(null);
    try {
      const result = await api.runNamed(SETUP_COMMAND, body, { idempotencyKey: key.current.key });
      reloadCompanies();
      navigate(isSetupResult(result) ? paths.home(result.company.code) : paths.entityList(code, COMPANY));
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
      setRunning(false);
    }
  }

  const missing = !template || inputs.some((property) => property.required && !(values[property.name] ?? "").trim());
  const chosen = templates.data?.items.find((candidate) => candidate.code === template);

  return (
    <>
      <PageHeader
        crumbs={[{ label: findEntityPlural(meta), to: paths.entityList(code, COMPANY) }]}
        title={command?.label ?? SETUP_COMMAND}
      />
      <p className="mb-4 max-w-form text-sm text-muted-foreground">{command?.description}</p>
      {error ? (
        <Alert className="mb-4" title={t("setup.failed")}>
          {error.message}
        </Alert>
      ) : null}
      <div className="flex flex-col gap-5 xl:flex-row xl:items-start">
        <form noValidate onSubmit={(event) => void runPreview(event)} className="w-full max-w-form shrink-0 space-y-5">
          <Section title={t("setup.template")}>
            <RadioGroup
              aria-label={t("setup.template")}
              className="gap-2"
              value={template ?? ""}
              onValueChange={(next) => {
                setTemplate(next);
                setPreview(null);
              }}
            >
              {templates.data === undefined ? (
                <Skeleton className="h-24 w-full" />
              ) : (
                templates.data.items.map((candidate) => (
                  <label
                    key={candidate.code}
                    htmlFor={`template-${candidate.code}`}
                    className={cn(
                      "flex cursor-pointer gap-3 rounded-md border bg-card p-3 text-sm",
                      template === candidate.code && "border-primary ring-1 ring-primary",
                    )}
                  >
                    <RadioGroupItem id={`template-${candidate.code}`} value={candidate.code} className="mt-0.5" />
                    <span className="space-y-1">
                      <span className="block font-medium">
                        {candidate.name}
                        <span className="ms-2 text-sm font-normal text-muted-foreground">
                          {t("setup.country_currency", { country: candidate.country, currency: candidate.currency })}
                        </span>
                      </span>
                      <span className="line-clamp-3 text-sm text-muted-foreground">{candidate.description}</span>
                    </span>
                  </label>
                ))
              )}
            </RadioGroup>
          </Section>
          <Section title={t("setup.company")}>
            <div className="grid gap-x-4 gap-y-3 rounded-md border bg-card p-4 sm:grid-cols-2">
              {inputs.map((property) => {
                const id = `setup-${property.name}`;
                const placeholder =
                  property.name === "base_currency" && chosen
                    ? chosen.currency
                    : property.type === "integer"
                      ? ""
                      : undefined;
                return (
                  <FormField
                    key={property.name}
                    id={id}
                    label={property.label}
                    required={property.required}
                    error={error?.field === property.name ? error.message : null}
                    hint={property.description}
                  >
                    <Input
                      id={id}
                      inputMode={property.type === "integer" ? "numeric" : undefined}
                      placeholder={placeholder}
                      value={values[property.name] ?? ""}
                      aria-invalid={error?.field === property.name ? true : undefined}
                      onChange={(event) => {
                        setValues((previous) => ({ ...previous, [property.name]: event.target.value }));
                        setPreview(null);
                      }}
                    />
                  </FormField>
                );
              })}
            </div>
          </Section>
          <div className="flex gap-2">
            <Button type="submit" variant="outline" busy={previewing} disabled={missing}>
              {t("setup.preview")}
            </Button>
            <Button busy={running} disabled={missing || preview === null} onClick={() => void setUp()}>
              {command?.label}
            </Button>
          </div>
        </form>
        <aside className="min-w-0 flex-1" aria-live="polite">
          {preview ? (
            <SetupPreview result={preview} />
          ) : (
            <p className="rounded-md border bg-card p-4 text-sm text-muted-foreground">{t("setup.preview_hint")}</p>
          )}
        </aside>
      </div>
    </>
  );
}

/** The command's answer, checked for what this page reads of it. */
type SetupResult = Omit<CompanySetupOutput, "company"> & { company: { code: string; name: string } };

function isSetupResult(value: Json): value is SetupResult & JsonObject {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const company = value.company;
  return (
    typeof value.accounts === "number" &&
    typeof company === "object" &&
    company !== null &&
    !Array.isArray(company) &&
    typeof company.code === "string" &&
    typeof company.name === "string" &&
    Array.isArray(value.fiscal_periods) &&
    Array.isArray(value.tax_codes) &&
    Array.isArray(value.notes) &&
    typeof value.roles === "object" &&
    value.roles !== null
  );
}

function findEntityPlural(meta: ReturnType<typeof useApp>["meta"]): string {
  return meta.entities.find((entity) => entity.name === COMPANY)?.plural ?? COMPANY;
}

/** What the preview says the setup will create. */
function SetupPreview({ result }: { result: SetupResult }) {
  return (
    <section className="space-y-3 rounded-md border bg-card p-4 text-sm">
      <h2 className="font-semibold">
        {t("setup.will_create", { name: result.company.name, code: result.company.code })}
      </h2>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
        <dt className="text-muted-foreground">{t("setup.accounts")}</dt>
        <dd className="tabular-nums">{result.accounts}</dd>
        <dt className="text-muted-foreground">{t("setup.fiscal_periods")}</dt>
        <dd>{result.fiscal_periods.join(", ")}</dd>
        <dt className="text-muted-foreground">{t("setup.tax_codes")}</dt>
        <dd>{result.tax_codes.join(", ") || t("value.none")}</dd>
        <dt className="text-muted-foreground">{t("setup.warehouse")}</dt>
        <dd>{result.warehouse_id ? t("value.yes") : t("value.no")}</dd>
      </dl>
      <div>
        <h3 className="mb-1 text-xs font-medium text-muted-foreground">{t("setup.account_roles")}</h3>
        <ul className="grid gap-x-4 sm:grid-cols-2">
          {Object.entries(result.roles).map(([role, account]) => (
            <li key={role} className="flex justify-between gap-2">
              <span className="text-muted-foreground">{role}</span>
              <span className="tabular-nums">{account.code}</span>
            </li>
          ))}
        </ul>
      </div>
      {result.notes.length > 0 ? (
        <Alert tone="info" title={t("setup.notes")}>
          <ul className="list-disc ps-5">
            {result.notes.map((note) => (
              <li key={note}>{note}</li>
            ))}
          </ul>
        </Alert>
      ) : null}
    </section>
  );
}

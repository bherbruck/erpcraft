// A new company, ready to work, from a template (`setup.templates`): its chart of accounts,
// fiscal years, warehouse, settings and tax codes, by one command (`company.setup`), all of it
// or nothing. The server's preview says what will be created before it is.

import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { Heading } from "@astryxdesign/core/Heading";
import { HStack } from "@astryxdesign/core/HStack";
import { MetadataList, MetadataListItem } from "@astryxdesign/core/MetadataList";
import { RadioList, RadioListItem } from "@astryxdesign/core/RadioList";
import { Skeleton } from "@astryxdesign/core/Skeleton";
import { Text } from "@astryxdesign/core/Text";
import { TextInput } from "@astryxdesign/core/TextInput";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CompanySetupOutput } from "@/api/generated";
import type { Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { Page } from "@/components/page";
import { t } from "@/i18n/t";
import { COMPANY, canRun, commandInputs, findCommand } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";
import { NotFoundPage } from "./not-found";

export const SETUP_COMMAND = "company.setup";

export default function CompanySetupPage() {
  const { meta } = useApp();
  const command = findCommand(meta, SETUP_COMMAND);
  if (!command || !canRun(meta, command.name) || !canRun(meta, "setup.templates")) return <NotFoundPage />;
  return <Setup />;
}

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
    Array.isArray(value.fiscal_periods) &&
    Array.isArray(value.tax_codes) &&
    Array.isArray(value.notes)
  );
}

function Setup() {
  const { meta, reloadCompanies } = useApp();
  const { code } = useCompany();
  const navigate = useNavigate();
  const command = findCommand(meta, SETUP_COMMAND);
  const templates = useAsync("setup-templates", (signal) => api.run("setup.templates", {}, { signal }));
  const inputs = useMemo(
    () => (command ? commandInputs(command).filter((input) => input.name !== "template").sort((a, b) => (a.required === b.required ? 0 : a.required ? -1 : 1)) : []),
    [command],
  );
  const [template, setTemplate] = useState<string | null>(null);
  const [values, setValues] = useState<Record<string, string>>(() => ({ first_year: String(new Date().getFullYear()) }));
  const [preview, setPreview] = useState<SetupResult | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const key = useRef<{ input: string; key: string }>({ input: "", key: newKey() });

  function input(): JsonObject {
    const out: JsonObject = { template: template ?? "" };
    for (const property of inputs) {
      const value = (values[property.name] ?? "").trim();
      if (value !== "") out[property.name] = property.type === "integer" ? parseInt(value, 10) : value;
    }
    return out;
  }

  async function runPreview(): Promise<void> {
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
    <Page back title={command?.label ?? SETUP_COMMAND} kicker={meta.entities.find((entity) => entity.name === COMPANY)?.plural}>
      <div className="two-columns">
        <VStack gap={4}>
          {command?.description ? <Text type="supporting">{command.description}</Text> : null}
          {error && !error.field ? <Banner status="error" title={t("setup.failed")} description={error.message} /> : null}
          <Card padding={4}>
            {templates.data === undefined ? (
              <Skeleton height={96} />
            ) : (
              <RadioList
                label={t("setup.template")}
                value={template ?? ""}
                onChange={(next) => {
                  setTemplate(next);
                  setPreview(null);
                }}
              >
                {templates.data.items.map((candidate) => (
                  <RadioListItem
                    key={candidate.code}
                    value={candidate.code}
                    align="start"
                    label={`${candidate.name} · ${t("setup.country_currency", { country: candidate.country, currency: candidate.currency })}`}
                    description={candidate.description}
                  />
                ))}
              </RadioList>
            )}
          </Card>
          <Card padding={4}>
            <VStack gap={3}>
              <Heading level={2}>{t("setup.company")}</Heading>
              <div className="form-grid">
                {inputs.map((property) => (
                  <TextInput
                    key={property.name}
                    label={property.label}
                    labelTooltip={property.description ?? undefined}
                    isRequired={property.required}
                    inputMode={property.type === "integer" ? "numeric" : undefined}
                    placeholder={property.name === "base_currency" && chosen ? chosen.currency : undefined}
                    value={values[property.name] ?? ""}
                    status={error?.field === property.name ? { type: "error", message: error.message } : undefined}
                    statusVariant="detached"
                    onChange={(next) => {
                      setValues((previous) => ({ ...previous, [property.name]: next }));
                      setPreview(null);
                    }}
                  />
                ))}
              </div>
            </VStack>
          </Card>
          <HStack gap={2}>
            <Button label={t("setup.preview")} isLoading={previewing} isDisabled={missing} onClick={() => void runPreview()} />
            <Button label={command?.label ?? SETUP_COMMAND} variant="primary" isLoading={running} isDisabled={missing || preview === null} onClick={() => void setUp()} />
          </HStack>
        </VStack>
        <div aria-live="polite">
          {preview ? (
            <Card padding={4}>
              <VStack gap={3}>
                <Heading level={2}>{t("setup.will_create", { name: preview.company.name, code: preview.company.code })}</Heading>
                <MetadataList>
                  <MetadataListItem label={t("setup.accounts")}>{String(preview.accounts)}</MetadataListItem>
                  <MetadataListItem label={t("setup.fiscal_periods")}>{preview.fiscal_periods.join(", ")}</MetadataListItem>
                  <MetadataListItem label={t("setup.tax_codes")}>{preview.tax_codes.join(", ") || t("value.none")}</MetadataListItem>
                  <MetadataListItem label={t("setup.warehouse")}>{preview.warehouse_id ? t("value.yes") : t("value.no")}</MetadataListItem>
                </MetadataList>
                <Text weight="semibold">{t("setup.account_roles")}</Text>
                <MetadataList columns="multi">
                  {Object.entries(preview.roles).map(([role, account]) => (
                    <MetadataListItem key={role} label={role}>
                      {account.code}
                    </MetadataListItem>
                  ))}
                </MetadataList>
                {preview.notes.length > 0 ? (
                  <Banner status="info" title={t("setup.notes")} collapsible={false}>
                    <VStack gap={1}>
                      {preview.notes.map((note) => (
                        <Text key={note}>{note}</Text>
                      ))}
                    </VStack>
                  </Banner>
                ) : null}
              </VStack>
            </Card>
          ) : (
            <Card padding={4} variant="muted">
              <Text type="supporting">{t("setup.preview_hint")}</Text>
            </Card>
          )}
        </div>
      </div>
    </Page>
  );
}

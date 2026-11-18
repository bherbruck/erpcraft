import { FieldGrid } from "@/components/field-grid";
// Any command the user may run, as a page of its own at /c/:company/run/:command: its inputs
// from the JSON Schema (a file is read in the browser and sent as base64), a preview where the
// command has one, and the answer. ../ui has hand-made pages for two of these (bank statement
// and e-invoice import); here every one gets the generic page.

import { Banner } from "@astryxdesign/core/Banner";
import { Button } from "@astryxdesign/core/Button";
import { Card } from "@astryxdesign/core/Card";
import { CodeBlock } from "@astryxdesign/core/CodeBlock";
import { FileInput } from "@astryxdesign/core/FileInput";
import { HStack } from "@astryxdesign/core/HStack";
import { Link } from "@astryxdesign/core/Link";
import { Text } from "@astryxdesign/core/Text";
import { VStack } from "@astryxdesign/core/VStack";
import { useMemo, useRef, useState } from "react";
import { useParams } from "react-router";
import { ApiError, api } from "@/api/client";
import type { CommandMeta, Json, JsonObject } from "@/api/types";
import { useApp, useCompany } from "@/app/context";
import { InputControl } from "@/components/actions";
import { Page } from "@/components/page";
import { t } from "@/i18n/t";
import { pendingApproval } from "@/lib/approval";
import { askable, canRun, commandInputs, findCommand, findDocType, type InputProperty } from "@/lib/meta";
import { paths } from "@/lib/paths";
import { newKey } from "@/lib/text";
import { NotFoundPage } from "./not-found";

export default function CommandPage() {
  const { meta } = useApp();
  const params = useParams();
  const command = findCommand(meta, params.command);
  if (!command || !canRun(meta, command.name)) return <NotFoundPage />;
  return <CommandForm key={command.name} command={command} />;
}

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const isFile = (input: InputProperty) => input.name.endsWith("_base64") || input.format === "byte";

async function base64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}

function CommandForm({ command }: { command: CommandMeta }) {
  const { meta } = useApp();
  const { code, company } = useCompany();
  const inputs = useMemo(() => commandInputs(command).filter((input) => input.name !== "company_id"), [command]);
  const takesCompany = commandInputs(command).some((input) => input.name === "company_id");
  const [values, setValues] = useState<Record<string, Json | undefined>>({});
  const [files, setFiles] = useState<Record<string, File | null>>({});
  const [busy, setBusy] = useState<"preview" | "run" | null>(null);
  const [result, setResult] = useState<{ preview: boolean; value: Json } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = useRef(newKey());

  async function input(): Promise<JsonObject> {
    const out: JsonObject = takesCompany && company ? { company_id: company.id } : {};
    for (const each of inputs) {
      const file = files[each.name];
      if (isFile(each) && file) out[each.name] = await base64(file);
      const value = values[each.name];
      if (value !== undefined && value !== null && value !== "") out[each.name] = value;
    }
    return out;
  }

  async function go(preview: boolean): Promise<void> {
    setBusy(preview ? "preview" : "run");
    setError(null);
    try {
      const body = await input();
      const value = preview ? await api.preview(command.name, body) : await api.runNamed(command.name, body, { idempotencyKey: key.current });
      setResult({ preview, value });
      if (!preview) key.current = newKey();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause.message : String(cause));
    }
    setBusy(null);
  }

  const created = result && !result.preview && command.creates && isObject(result.value) && typeof result.value.id === "string" ? result.value.id : null;
  const createdType = command.creates ? findDocType(meta, command.creates) : undefined;
  const waiting = result ? pendingApproval(result.value) : null;

  return (
    <Page title={command.label} meta={[command.name]} contentWidth={960}>
      <VStack gap={4} maxWidth={960}>
        {command.description ? <Text type="supporting">{command.description}</Text> : null}
        <Card padding={4}>
          <VStack gap={4}>
            <FieldGrid>
              {inputs.map((each) =>
                isFile(each) ? (
                  <FileInput
                    key={each.name}
                    label={each.label}
                    description={each.description ?? undefined}
                    isRequired={each.required}
                    value={files[each.name] ?? null}
                    onChange={(next) => setFiles((previous) => ({ ...previous, [each.name]: Array.isArray(next) ? (next[0] ?? null) : next }))}
                  />
                ) : askable(each) ? (
                  <InputControl key={each.name} input={each} value={values[each.name]} onChange={(v) => setValues((previous) => ({ ...previous, [each.name]: v }))} />
                ) : null,
              )}
            </FieldGrid>
            <HStack gap={2} hAlign="end">
              {!command.read_only ? (
                <Button label={t("command.preview")} isLoading={busy === "preview"} isDisabled={busy !== null} onClick={() => void go(true)} />
              ) : null}
              <Button label={command.label} variant={command.destructive ? "destructive" : "primary"} isLoading={busy === "run"} isDisabled={busy !== null} onClick={() => void go(false)} />
            </HStack>
          </VStack>
        </Card>
        {error ? <Banner status="error" title={t("preview.failed", { action: command.label })} description={error} /> : null}
        {waiting ? <Banner status="warning" title={t("approval.waiting")} description={waiting.message ?? undefined} endContent={waiting.id ? <Link href={paths.approval(code, waiting.id)}>{t("approval.open_request")}</Link> : undefined} /> : null}
        {created && command.creates ? (
          <Banner status="success" title={t("command.done")} endContent={<Link href={paths.doc(code, command.creates, created)}>{createdType?.label ?? command.creates}</Link>} />
        ) : null}
        {result && !waiting ? (
          <VStack gap={2}>
            <Text weight="semibold">{result.preview ? t("preview.heading") : t("command.result")}</Text>
            <CodeBlock code={JSON.stringify(result.value, null, 2)} language="json" />
          </VStack>
        ) : null}
      </VStack>
    </Page>
  );
}

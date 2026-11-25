// Actions whose answer is a file to hand on (a payment run's bank file), for every command on a record
// whose output schema is a file (`file_name`, `content_type`, `content`). The dialog asks for
// what the command takes besides the record, runs it, and the browser downloads the file.

import { useState } from "react";
import { ApiError, api } from "@/api/client";
import type { CommandMeta, Json, JsonObject, Meta } from "@/api/types";
import { useCompany } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { Dialog } from "@/components/dialog";
import { FormField } from "@/components/form-field";
import { t } from "@/i18n/t";
import { formatDecimal } from "@/lib/decimal";
import { canRun, commandInputs } from "@/lib/meta";
import { CommandInput, InputLabelText } from "./command-input";

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Whether a command answers with a file, by its output schema. */
export function answersWithFile(command: CommandMeta): boolean {
  const schema = isObject(command.output_schema) ? command.output_schema : {};
  const properties = isObject(schema.properties) ? schema.properties : {};
  return "file_name" in properties && "content" in properties && "content_type" in properties;
}

/** The commands on a record of this resource that make a file of it, which the user may run. */
export function fileActions(meta: Meta, resource: string): CommandMeta[] {
  return meta.commands.filter(
    (command) =>
      command.resource === resource && command.target && canRun(meta, command.name) && answersWithFile(command),
  );
}

function download(name: string, mediaType: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: mediaType }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** Asks for the command's inputs, runs it on the record and downloads the file it answers with. */
export function FileActionDialog({ command, id, onClose }: { command: CommandMeta; id: string; onClose: () => void }) {
  const { company } = useCompany();
  const asked = commandInputs(command).filter((input) => input.name !== "id");
  // A choice starts at its first option when the command needs one.
  const [values, setValues] = useState<Record<string, Json>>(() => {
    const out: Record<string, Json> = {};
    for (const input of asked) {
      const first = input.options?.[0]?.value;
      if (input.required && first !== undefined) out[input.name] = first;
    }
    return out;
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [done, setDone] = useState<JsonObject | null>(null);

  async function run(): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      const input: JsonObject = { id };
      for (const [name, value] of Object.entries(values)) if (value !== "" && value !== null) input[name] = value;
      const answer = await api.runNamed(command.name, input);
      if (!isObject(answer)) throw new ApiError(0, "error", t("error.unexpected"));
      download(String(answer.file_name), String(answer.content_type), String(answer.content));
      setDone(answer);
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  return (
    <Dialog
      open
      onClose={onClose}
      title={command.label}
      description={command.description}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {done ? t("action.close") : t("action.cancel")}
          </Button>
          <Button busy={busy} onClick={() => void run()}>
            {done ? t("file.download_again") : t("file.download")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        {asked.length > 0 ? (
          <div className="grid gap-4 sm:grid-cols-2">
            {asked.map((property) => {
              const fieldId = `file-${property.name}`;
              return (
                <FormField
                  key={property.name}
                  id={fieldId}
                  label={<InputLabelText property={property} />}
                  hint={property.description}
                >
                  <CommandInput
                    property={property}
                    id={fieldId}
                    value={values[property.name]}
                    onChange={(next) => setValues((previous) => ({ ...previous, [property.name]: next }))}
                    disabled={busy}
                    companyId={company?.id ?? null}
                  />
                </FormField>
              );
            })}
          </div>
        ) : null}
        {error ? <Alert title={t("file.refused")}>{error.message}</Alert> : null}
        {done ? (
          <Alert tone="info" title={t("file.made", { file: String(done.file_name) })}>
            {typeof done.count === "number" && typeof done.total === "string"
              ? t("file.made_detail", {
                  count: done.count,
                  total: formatDecimal(done.total, { minFraction: 2 }),
                  currency: typeof done.currency === "string" ? done.currency : "",
                })
              : null}
          </Alert>
        ) : null}
      </div>
    </Dialog>
  );
}

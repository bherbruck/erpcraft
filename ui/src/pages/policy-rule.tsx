// The record screen of a policy rule. It is the generic entity form with two
// fields replaced and a panel beside it: the policy point is chosen from the
// points the server publishes, and the panel says what that point decides and
// which facts a condition can use.

import type { PolicyMeta } from "@/api/types";
import { useApp } from "@/app/context";
import { Button } from "@/components/button";
import { NativeSelect } from "@/components/ui/native-select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { t } from "@/i18n/t";
import { enumLabel, findEntity, moduleLabel } from "@/lib/meta";
import type { FormCustomization } from "./entity-record";

function usePolicies(): PolicyMeta[] {
  return useApp().meta.policies ?? [];
}

function PointSelect({
  id,
  value,
  invalid,
  describedBy,
  onChange,
}: {
  id: string;
  value: string;
  invalid: boolean;
  describedBy: string | undefined;
  onChange: (value: string | null) => void;
}) {
  const { meta } = useApp();
  const policies = usePolicies();
  const modules = [...new Set(policies.map((policy) => policy.module))];
  const known = policies.some((policy) => policy.name === value);
  return (
    <NativeSelect
      className="w-full"
      id={id}
      value={value}
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      aria-required="true"
      onChange={(event) => onChange(event.target.value || null)}
    >
      {value === "" ? <option value="">{t("policy.choose")}</option> : null}
      {/* A rule written for a point this server no longer has is shown as it is, not silently changed. */}
      {value !== "" && !known ? <option value={value}>{t("policy.unknown", { point: value })}</option> : null}
      {modules.map((module) => (
        <optgroup key={module} label={moduleLabel(meta, module)}>
          {policies
            .filter((policy) => policy.module === module)
            .map((policy) => (
              <option key={policy.name} value={policy.name}>
                {policy.name}
              </option>
            ))}
        </optgroup>
      ))}
    </NativeSelect>
  );
}

function ConditionInput({
  id,
  value,
  point,
  invalid,
  describedBy,
  onChange,
}: {
  id: string;
  value: string;
  point: string;
  invalid: boolean;
  describedBy: string | undefined;
  onChange: (value: string | null) => void;
}) {
  const policy = usePolicies().find((candidate) => candidate.name === point);
  const example =
    policy?.default.condition ?? (policy?.facts[0] ? `facts.${policy.facts[0].name} > 0` : "facts.name > 0");
  const hintId = `${id}-hint`;
  return (
    <div className="space-y-1">
      <Textarea
        id={id}
        value={value}
        rows={3}
        spellCheck={false}
        aria-invalid={invalid || undefined}
        aria-describedby={[hintId, describedBy].filter(Boolean).join(" ")}
        className="min-h-16"
        onChange={(event) => onChange(event.target.value === "" ? null : event.target.value)}
      />
      <p id={hintId} className="text-sm text-muted-foreground">
        {t("policy.condition_hint", { example })}
      </p>
      {policy?.default.condition && value === "" ? (
        <Button variant="outline" size="sm" onClick={() => onChange(policy.default.condition)}>
          {t("policy.use_default")}
        </Button>
      ) : null}
    </div>
  );
}

function PolicyPanel({ point }: { point: string }) {
  const { meta } = useApp();
  const policy = usePolicies().find((candidate) => candidate.name === point);
  const effectField = findEntity(meta, "policy_rule")?.fields.find((field) => field.name === "effect");
  const effectWord = (value: string): string => (effectField ? enumLabel(effectField, value) : value).toLowerCase();
  if (!policy) {
    return <p className="rounded-md border bg-card p-4 text-sm text-muted-foreground">{t("policy.pick_first")}</p>;
  }
  const { effect, condition } = policy.default;
  return (
    <div className="space-y-4 rounded-md border bg-card p-4 text-sm">
      <section className="space-y-1">
        <h2 className="text-xs font-medium text-muted-foreground">{t("policy.decides")}</h2>
        <p className="font-mono text-xs">{policy.name}</p>
        <p>{policy.description}</p>
      </section>
      <section className="space-y-1">
        <h2 className="text-xs font-medium text-muted-foreground">{t("policy.facts")}</h2>
        <div className="rounded-md border">
          <Table aria-label={t("policy.facts")}>
            <TableHeader>
              <TableRow>
                <TableHead>{t("policy.fact")}</TableHead>
                <TableHead>{t("policy.fact_type")}</TableHead>
                <TableHead className="w-full">{t("policy.fact_meaning")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {policy.facts.map((fact) => (
                <TableRow key={fact.name}>
                  <TableCell className="whitespace-nowrap font-mono text-xs">{`facts.${fact.name}`}</TableCell>
                  <TableCell className="whitespace-nowrap text-muted-foreground">{fact.type}</TableCell>
                  <TableCell>{fact.description}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </section>
      <section className="space-y-1">
        <h2 className="text-xs font-medium text-muted-foreground">{t("policy.default")}</h2>
        <p>
          {condition
            ? t("policy.default_when", { effect: effectWord(effect), condition })
            : t("policy.default_always", { effect: effectWord(effect) })}
        </p>
      </section>
      <section className="space-y-1">
        <h2 className="text-xs font-medium text-muted-foreground">{t("policy.refusal")}</h2>
        <p>{policy.refusal}</p>
      </section>
    </div>
  );
}

const text = (value: unknown): string => (typeof value === "string" ? value : "");

// The fields that only an `approve` rule has: who approves, how many, how long a request waits.
const APPROVAL_FIELDS = new Set(["approver_role_id", "approver_permission", "approvals_needed", "approval_days"]);

export const policyRuleForm: FormCustomization = {
  hidden: (field, values) => APPROVAL_FIELDS.has(field.name) && values.effect !== "approve",
  wide: ["point", "condition", "message"],
  field: ({ field, id, value, values, invalid, describedBy, onChange }) => {
    if (field.name === "point") {
      return (
        <PointSelect id={id} value={text(value)} invalid={invalid} describedBy={describedBy} onChange={onChange} />
      );
    }
    if (field.name === "condition") {
      return (
        <ConditionInput
          id={id}
          value={text(value)}
          point={text(values.point)}
          invalid={invalid}
          describedBy={describedBy}
          onChange={onChange}
        />
      );
    }
    return undefined;
  },
  aside: (values) => <PolicyPanel point={text(values.point)} />,
};

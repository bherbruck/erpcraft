// The roles a user holds, on the user's page: each grant for a company or for
// all companies, taken away with `user.revoke_role`, and new ones granted with
// `user.assign_role`. Shown when the signed-in user may run those commands.

import { Plus } from "lucide-react";
import { useState } from "react";
import { ApiError, api } from "@/api/client";
import type { UserGetOutput } from "@/api/generated";
import { useApp } from "@/app/context";
import { Alert } from "@/components/alert";
import { Button } from "@/components/button";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { type Column, DataTable } from "@/components/data-table";
import { Lookup } from "@/components/lookup";
import { Section } from "@/components/page";
import { Label } from "@/components/ui/label";
import { t } from "@/i18n/t";
import { COMPANY, canRun, findCommand, ROLE } from "@/lib/meta";
import { newKey } from "@/lib/text";
import { useAsync } from "@/lib/use-async";

type Grant = UserGetOutput["roles"][number];

export function UserRoles({ userId }: { userId: string }) {
  const { meta } = useApp();
  const assign = findCommand(meta, "user.assign_role");
  const revoke = findCommand(meta, "user.revoke_role");
  const canAssign = assign !== undefined && canRun(meta, assign.name);
  const canRevoke = revoke !== undefined && canRun(meta, revoke.name);
  const user = useAsync(canRun(meta, "user.get") ? `user|${userId}` : null, (signal) =>
    api.run("user.get", { id: userId }, { signal }),
  );
  const [role, setRole] = useState<string | null>(null);
  const [company, setCompany] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [revoking, setRevoking] = useState<Grant | null>(null);

  async function run(work: () => Promise<unknown>): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await work();
      user.reload();
    } catch (cause) {
      setError(cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected")));
    }
    setBusy(false);
  }

  if (!user.data && !user.loading) return null;
  const columns: Column<Grant>[] = [
    { id: "role", header: t("users.role"), className: "w-full", cell: (grant) => grant.role_name },
    {
      id: "company",
      header: t("users.company"),
      cell: (grant) => <span className="whitespace-nowrap">{grant.company_code ?? t("users.all_companies")}</span>,
    },
    ...(canRevoke
      ? [
          {
            id: "revoke",
            header: t("sso.actions"),
            cell: (grant: Grant) => (
              <Button variant="ghost" size="sm" onClick={() => setRevoking(grant)}>
                {revoke?.label}
              </Button>
            ),
          },
        ]
      : []),
  ];

  return (
    <Section title={t("users.roles")}>
      {error ? <Alert title={t("users.role_failed")}>{error.message}</Alert> : null}
      <DataTable
        caption={t("users.roles")}
        columns={columns}
        rows={user.data?.roles}
        rowKey={(grant) => `${grant.role_id}|${grant.company_id ?? ""}`}
        loading={user.loading}
        error={user.error}
        onRetry={user.reload}
        empty={<p>{t("users.no_roles")}</p>}
      />
      {canAssign && assign ? (
        <div className="flex flex-wrap items-end gap-3 rounded-md border bg-card p-3">
          <div className="space-y-1">
            <Label htmlFor="grant-role">{t("users.role")}</Label>
            <Lookup
              id="grant-role"
              className="w-64"
              entity={ROLE}
              keyField="id"
              value={role}
              allowCreate={false}
              onChange={setRole}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="grant-company">{t("users.company")}</Label>
            <Lookup
              id="grant-company"
              className="w-64"
              entity={COMPANY}
              keyField="id"
              value={company}
              allowCreate={false}
              placeholder={t("users.all_companies")}
              onChange={setCompany}
            />
          </div>
          <Button
            busy={busy}
            disabled={!role}
            title={assign.description}
            onClick={() => {
              const input = { user_id: userId, role_id: role ?? "", company_id: company };
              void run(() => api.run("user.assign_role", input, { idempotencyKey: newKey() })).then(() => {
                setRole(null);
                setCompany(null);
              });
            }}
          >
            <Plus aria-hidden="true" />
            {assign.label}
          </Button>
        </div>
      ) : null}
      <ConfirmDialog
        open={revoking !== null}
        title={t("users.revoke_title", {
          role: revoking?.role_name ?? "",
          company: revoking?.company_code ?? t("users.all_companies"),
        })}
        description={revoke?.description ?? ""}
        confirmLabel={revoke?.label ?? t("action.delete")}
        busy={busy}
        error={null}
        onConfirm={() => {
          const grant = revoking;
          setRevoking(null);
          if (grant)
            void run(() =>
              api.run("user.revoke_role", { user_id: userId, role_id: grant.role_id, company_id: grant.company_id }),
            );
        }}
        onClose={() => setRevoking(null)}
      />
    </Section>
  );
}

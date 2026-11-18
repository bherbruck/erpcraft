import { Link } from "react-router";
import { useCompany } from "@/app/context";
import { Alert } from "@/components/ui/alert";
import { t } from "@/i18n/t";
import type { PendingApproval } from "@/lib/approval";
import { formatTimestamp } from "@/lib/dates";
import { paths } from "@/lib/paths";
import { ApprovalFacts } from "./approval-parts";
import { LinkLabel } from "./field-value";

/** Who has to approve, in words: a role, a permission, how many people. */
export function Approvers({ approval }: { approval: Pick<PendingApproval, "approvers"> }) {
  const { role_id, permission, approvals_needed } = approval.approvers;
  return (
    <span>
      {role_id ? (
        <>
          {t("approval.by_role")} <LinkLabel entity="role" keyField="id" value={role_id} />
        </>
      ) : null}
      {role_id && permission ? t("approval.or") : null}
      {permission ? (
        <>
          {t("approval.by_permission")} <code className="font-mono text-xs">{permission}</code>
        </>
      ) : null}
      {approvals_needed > 1 ? t("approval.people", { count: approvals_needed }) : null}
    </span>
  );
}

/**
 * A command that waits for approval: why, on which facts, and who decides. On a
 * preview nothing is asked yet; after running it, the request has an id to open.
 */
export function ApprovalNotice({ approval, preview }: { approval: PendingApproval; preview: boolean }) {
  const { code } = useCompany();
  return (
    <Alert tone="warning" title={preview ? t("approval.would_need") : t("approval.waiting")}>
      {approval.message ? <p>{approval.message}</p> : null}
      <p className="mt-1">
        {t("approval.decided_by")} <Approvers approval={approval} />
      </p>
      <div className="mt-2">
        <ApprovalFacts point={approval.point} facts={approval.facts} compact />
      </div>
      {!preview && approval.id ? (
        <p className="mt-2">
          <Link
            to={paths.approval(code, approval.id)}
            className="font-medium underline underline-offset-2 hover:no-underline"
          >
            {t("approval.open_request")}
          </Link>
          {approval.expires_at ? (
            <span className="text-muted-foreground">
              {" "}
              {t("approval.expires", { at: formatTimestamp(approval.expires_at) })}
            </span>
          ) : null}
        </p>
      ) : null}
    </Alert>
  );
}

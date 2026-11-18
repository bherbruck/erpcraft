// A command that needs approval does not run: it answers HTTP 202 with
// `{status: "pending_approval", approval_request: {...}}` (a preview that would
// need approval answers with the same object, `preview: true` and no id). No
// command's own result has `status: "pending_approval"`, so that is the test.

import type { Json, JsonObject } from "@/api/types";

export interface PendingApproval {
  /** Null on a preview: nothing was asked yet. */
  id: string | null;
  command: string;
  point: string;
  message: string | null;
  /** The facts the rule decided on, as a person reads them. */
  facts: Record<string, string>;
  approvers: { role_id: string | null; permission: string | null; approvals_needed: number };
  expires_at: string | null;
}

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const text = (value: Json | undefined): string | null => (typeof value === "string" ? value : null);

/** The approval a command's answer asks for, or null when the command ran. */
export function pendingApproval(answer: Json | undefined): PendingApproval | null {
  if (!isObject(answer) || answer.status !== "pending_approval" || !isObject(answer.approval_request)) return null;
  const request = answer.approval_request;
  const facts: Record<string, string> = {};
  if (isObject(request.facts)) for (const [name, value] of Object.entries(request.facts)) facts[name] = String(value);
  const approvers = isObject(request.approvers) ? request.approvers : {};
  return {
    id: text(request.id),
    command: text(request.command) ?? "",
    point: text(request.point) ?? "",
    message: text(request.message),
    facts,
    approvers: {
      role_id: text(approvers.role_id),
      permission: text(approvers.permission),
      approvals_needed: typeof approvers.approvals_needed === "number" ? approvers.approvals_needed : 1,
    },
    expires_at: text(request.expires_at),
  };
}

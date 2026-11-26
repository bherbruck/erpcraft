import { expect, test } from "bun:test";
import type { CommandMeta } from "@/api/types";
import { primaryAction, type ResourceAction } from "./meta";

function action(name: string, primaryWhen: object | null, destructive = false): ResourceAction {
  const command = { name: `payment_run.${name}`, primary_when: primaryWhen } as Partial<CommandMeta> as CommandMeta;
  return { action: name, label: name, command, inputs: [], destructive, creates: null };
}

const approve = action("approve", { states: ["draft"], empty: null, not_empty: "lines" });
const cancel = action("cancel", null, true);
const propose = action("propose", { states: ["draft"], empty: "lines", not_empty: null });
const actions = [approve, cancel, propose];

test("the registry's primary_when picks the next step by state and lines", () => {
  expect(primaryAction(actions, "draft", () => 0)).toBe(propose);
  expect(primaryAction(actions, "draft", () => 3)).toBe(approve);
});

test("without a matching hint, the first action that does not end or undo anything", () => {
  expect(primaryAction(actions, "approved", () => 3)).toBe(approve);
  expect(primaryAction([cancel, action("post", null)], "approved", () => 3)?.action).toBe("post");
});

import { Badge } from "@astryxdesign/core/Badge";
import type { DocTypeMeta } from "@/api/types";

type Tone = "neutral" | "info" | "success" | "warning" | "error";

function tone(name: string | undefined): Tone {
  switch (name) {
    case "info":
      return "info";
    case "success":
      return "success";
    case "warning":
      return "warning";
    case "danger":
      return "error";
    default:
      return "neutral";
  }
}

/** A document's status: always its word, the registry's tone as the color. */
export function StatusBadge({ status, docType }: { status: string; docType: DocTypeMeta | undefined }) {
  const state = docType?.states.find((candidate) => candidate.name === status);
  return <Badge variant={tone(state?.tone)} label={state?.label ?? status} />;
}

import { Badge } from "@astryxdesign/core/Badge";
import type { DocTypeMeta } from "@/api/types";
import { stateTone } from "@/lib/meta";

/** A status always has its word, never only a color. */
export function StatusBadge({ type, status }: { type: DocTypeMeta | undefined; status: string }) {
  const state = type?.states.find((s) => s.name === status);
  return <Badge variant={stateTone(state)} label={state?.label ?? status} />;
}

import type { DocTypeMeta, StateMeta } from "@/api/types";
import { Badge, type BadgeTone } from "@/components/badge";

// How each tone the registry gives a state is drawn. The word is always shown; the color only supports it.
const TONES: Record<StateMeta["tone"], BadgeTone> = {
  neutral: "neutral",
  info: "info",
  success: "success",
  warning: "warning",
  danger: "destructive",
};

export function StatusBadge({ status, docType }: { status: string; docType?: DocTypeMeta }) {
  const state = docType?.states.find((candidate) => candidate.name === status);
  return <Badge tone={state ? TONES[state.tone] : "neutral"}>{state?.label ?? status}</Badge>;
}

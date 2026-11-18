import { HStack } from "@astryxdesign/core/HStack";
import { StatusDot } from "@astryxdesign/core/StatusDot";
import { Text } from "@astryxdesign/core/Text";
import type { DocTypeMeta } from "@/api/types";

type Tone = "neutral" | "accent" | "success" | "warning" | "error";

function tone(name: string | undefined): Tone {
  switch (name) {
    case "info":
      return "accent";
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

/**
 * A document's status as Astryx shows status: a StatusDot with its word beside it (Badge is for
 * counts). The word carries the meaning; the colour only helps.
 */
export function StatusBadge({ status, docType }: { status: string; docType: DocTypeMeta | undefined }) {
  const state = docType?.states.find((candidate) => candidate.name === status);
  const label = state?.label ?? status;
  return (
    <HStack gap={1} vAlign="center">
      <StatusDot variant={tone(state?.tone)} label={label} />
      {/* The word takes the size of where it sits: a grid cell, a page header. */}
      <Text type="inherit">{label}</Text>
    </HStack>
  );
}

/** A state of the page itself ("Unsaved changes"), drawn the same way: a dot and its word. */
export function StateFlag({ tone: variant, label }: { tone: Tone; label: string }) {
  return (
    <HStack gap={1} vAlign="center">
      <StatusDot variant={variant} label={label} />
      <Text color="secondary">{label}</Text>
    </HStack>
  );
}

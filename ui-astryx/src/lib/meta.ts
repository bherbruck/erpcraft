import type { DocTypeMeta, EntityMeta, Meta, StateMeta } from "@/api/types";

export function docType(meta: Meta, name: string): DocTypeMeta | undefined {
  return meta.doc_types.find((d) => d.name === name);
}

export function entity(meta: Meta, name: string): EntityMeta | undefined {
  return meta.entities.find((e) => e.name === name);
}

export type BadgeTone = "neutral" | "info" | "success" | "warning" | "error";

export function stateTone(state: StateMeta | undefined): BadgeTone {
  switch (state?.tone) {
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

/** Decimals a currency shows. The spike knows the common ones; the real UI reads the currency record. */
export function currencyDecimals(code: string | null | undefined): number {
  if (!code) return 2;
  return ["JPY", "KRW", "VND", "CLP", "ISK"].includes(code) ? 0 : 2;
}

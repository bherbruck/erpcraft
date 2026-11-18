import { Grid, GridSpan } from "@astryxdesign/core/Grid";
import type { ReactNode } from "react";

/**
 * Form fields in columns that reflow with the width (Astryx Grid, auto-fill tracks of at least
 * 15rem). Columns are tight, rows a step more generous, so a field reads with its own label.
 */
export function FieldGrid({ children }: { children: ReactNode }) {
  return (
    <Grid columns={{ minWidth: 240, repeat: "fill" }} columnGap={4} rowGap={5} align="start">
      {children}
    </Grid>
  );
}

/** A field that takes two columns (a memo) or the whole row (JSON). */
export function FieldSpan({ wide, children }: { wide: "two" | "full"; children: ReactNode }) {
  return <GridSpan columns={wide === "full" ? "full" : 2}>{children}</GridSpan>;
}

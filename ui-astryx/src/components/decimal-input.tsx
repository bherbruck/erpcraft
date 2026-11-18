import { TextInput } from "@astryxdesign/core/TextInput";
import { type KeyboardEvent, useState } from "react";
import { t } from "@/i18n/t";
import { formatDecimal, isDecimal, isDecimalDraft } from "@/lib/decimal";

export interface DecimalInputProps {
  label: string;
  /** The value as a decimal string; never a JavaScript number. */
  value: string | null;
  onChange: (value: string | null) => void;
  /** Places the server stores; more is refused here before the server has to. */
  scale?: number | null;
  /** Decimals shown while the field is not being edited, such as a currency's two. */
  minFraction?: number;
  isLabelHidden?: boolean;
  isRequired?: boolean;
  isDisabled?: boolean;
  description?: string;
  labelTooltip?: string;
  size?: "sm" | "md";
  /** The server's words about this field. */
  error?: string | null;
  /** In a grid cell: the message sits behind the status icon so the row keeps its height. */
  isCompactStatus?: boolean;
  onKeyDown?: (event: KeyboardEvent<HTMLInputElement>) => void;
}

function places(value: string): number {
  return value.split(".")[1]?.length ?? 0;
}

/**
 * The one input for money, quantities, rates and percents. Astryx's NumberInput holds a
 * JavaScript number, which loses digits; this is its TextInput with the decimal keypad,
 * a string value, typing limited to what a decimal can be, and its own check on leaving.
 */
export function DecimalInput(props: DecimalInputProps) {
  const [focused, setFocused] = useState(false);
  const [touched, setTouched] = useState(false);
  const text = props.value ?? "";
  const shown = !focused && text !== "" && isDecimal(text) ? formatDecimal(text, { minFraction: props.minFraction ?? 0 }) : text;

  // Our own check, after the user leaves the field; the server's error wins when there is one.
  let own: string | null = null;
  if (touched && !focused && text !== "") {
    if (!isDecimal(text)) own = t("decimal.invalid");
    else if (props.scale != null && places(text) > props.scale) own = t("decimal.too_many_places", { places: props.scale });
  }
  const message = props.error ?? own;

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: tracks focus of the input inside, to format at rest
    <div
      className="num"
      onFocus={() => setFocused(true)}
      onBlur={() => {
        setFocused(false);
        setTouched(true);
      }}
    >
      <TextInput
        label={props.label}
        isLabelHidden={props.isLabelHidden}
        isRequired={props.isRequired}
        isDisabled={props.isDisabled}
        description={props.description}
        labelTooltip={props.labelTooltip}
        size={props.size}
        inputMode="decimal"
        autoComplete="off"
        hasSpellCheck={false}
        value={shown}
        onChange={(next) => {
          const cleaned = next.replace(/[\s,]/g, "");
          if (isDecimalDraft(cleaned)) props.onChange(cleaned === "" ? null : cleaned);
        }}
        onKeyDown={props.onKeyDown}
        status={message ? { type: "error", message } : undefined}
        statusVariant={props.isCompactStatus ? "tooltip" : "detached"}
        width="100%"
      />
    </div>
  );
}

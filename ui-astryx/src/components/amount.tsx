import { Text } from "@astryxdesign/core/Text";
import { formatDecimal } from "@/lib/decimal";
import { currencyDecimals } from "@/lib/meta";

/** An amount: tabular digits, the currency's decimals, and the currency code beside it. */
export function Amount({ value, currency, weight }: { value: string | null | undefined; currency?: string | null; weight?: "semibold" }) {
  if (value === null || value === undefined || value === "") return <Text color="secondary">—</Text>;
  const shown = formatDecimal(value, { minFraction: currencyDecimals(currency) });
  return (
    <Text hasTabularNumbers weight={weight}>
      {shown}
      {currency ? <Text type="supporting"> {currency}</Text> : null}
    </Text>
  );
}

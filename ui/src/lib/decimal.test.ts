import { expect, test } from "bun:test";
import { formatDecimal, isDecimalDraft, isZeroDecimal, parseLocalDecimal, sameDecimal, toLocalDraft } from "./decimal";

test("formats without rounding and without floating point", () => {
  expect(formatDecimal("50100.0000", { minFraction: 2, locale: "en-US" })).toBe("50,100.00");
  expect(formatDecimal("9.750000", { minFraction: 2, locale: "en-US" })).toBe("9.75");
  expect(formatDecimal("4.123456", { minFraction: 2, locale: "en-US" })).toBe("4.123456");
  expect(formatDecimal("60.000000", { locale: "en-US" })).toBe("60");
  expect(formatDecimal("-253.8000", { minFraction: 2, locale: "en-US" })).toBe("-253.80");
  expect(formatDecimal("0", { minFraction: 2, locale: "en-US" })).toBe("0.00");
  expect(formatDecimal("-0.00", { minFraction: 2, locale: "en-US" })).toBe("0.00");
});

test("keeps every digit of numbers a double cannot hold", () => {
  expect(formatDecimal("12345678901234567890.123456789", { locale: "en-US" })).toBe(
    "12,345,678,901,234,567,890.123456789",
  );
  expect(formatDecimal("0.1", { minFraction: 2, locale: "en-US" })).toBe("0.10");
});

test("uses the separators of the locale", () => {
  expect(formatDecimal("1234567.5", { minFraction: 2, locale: "de-DE" })).toBe("1.234.567,50");
});

test("leaves what is not a decimal alone", () => {
  expect(formatDecimal("abc")).toBe("abc");
});

test("compares by value", () => {
  expect(sameDecimal("1.50", "1.5")).toBe(true);
  expect(sameDecimal("25.000000", "25")).toBe(true);
  expect(sameDecimal("0", "-0.00")).toBe(true);
  expect(sameDecimal("1.5", "1.51")).toBe(false);
  expect(isZeroDecimal("0.0000")).toBe(true);
  expect(isZeroDecimal("0.0100")).toBe(false);
});

test("accepts what a person types on the way to a decimal", () => {
  for (const draft of ["", "-", "1", "1.", "-1.25", ".5"]) expect(isDecimalDraft(draft)).toBe(true);
  for (const draft of ["1,5", "1.2.3", "abc", "1e5"]) expect(isDecimalDraft(draft)).toBe(false);
});

test("reads decimals as people type them, exactly, in German and English", () => {
  expect(parseLocalDecimal("1.234,56", "de-DE")).toBe("1234.56");
  expect(parseLocalDecimal("1234,5", "de-DE")).toBe("1234.5");
  expect(parseLocalDecimal("-0,10", "de-DE")).toBe("-0.10");
  expect(parseLocalDecimal("1.234", "de-DE")).toBe("1234");
  expect(parseLocalDecimal("1.5", "de-DE")).toBe("1.5");
  expect(parseLocalDecimal("12345678901234567890,123456789", "de-DE")).toBe("12345678901234567890.123456789");
  expect(parseLocalDecimal("1,234.56", "en-US")).toBe("1234.56");
  expect(parseLocalDecimal("abc", "de-DE")).toBeNull();
  expect(toLocalDraft("9.750000", "de-DE")).toBe("9,750000");
});

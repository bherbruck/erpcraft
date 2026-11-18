import { describe, expect, test } from "bun:test";
import { parseUserDate, resolveRange, toDay } from "../src/core/dates";
import { compareDecimal, formatDecimal, parseUserDecimal, sumDecimals, toDecimal } from "../src/core/decimal";

describe("decimals", () => {
  test("compare by value, exactly", () => {
    expect(compareDecimal("10.5", "9.99")).toBe(1);
    expect(compareDecimal("-1", "0.001")).toBe(-1);
    expect(compareDecimal("1.50", "1.5")).toBe(0);
    expect(compareDecimal("12345678901234567890.01", "12345678901234567890.02")).toBe(-1);
  });

  test("sum without floats", () => {
    expect(sumDecimals(["0.1", "0.2"])).toBe("0.3");
    expect(sumDecimals(["10.00", "-2.50", "x"])).toBe("7.5");
    expect(sumDecimals([])).toBe("0");
  });

  test("canonical form", () => {
    expect(toDecimal("007.500")).toBe("7.5");
    expect(toDecimal(1e-7)).toBe("0.0000001");
    expect(toDecimal({ amount: "3.10", currency: "EUR" })).toBe("3.1");
    expect(toDecimal("abc")).toBeNull();
    expect(toDecimal("-0.00")).toBe("0");
  });

  test("what people type and paste", () => {
    expect(parseUserDecimal("1,234.50", "en-US")).toBe("1234.5");
    expect(parseUserDecimal("1.234,50", "de-DE")).toBe("1234.5");
    expect(parseUserDecimal("(12.00)", "en-US")).toBe("-12");
    expect(parseUserDecimal("€ 9.99", "en-US")).toBe("9.99");
    expect(parseUserDecimal("EUR 9.99", "en-US")).toBe("9.99");
    expect(parseUserDecimal("12-", "en-US")).toBe("-12");
    expect(parseUserDecimal("twelve", "en-US")).toBeNull();
  });

  test("format keeps every digit", () => {
    expect(formatDecimal("1234567.125", 2, "en-US")).toBe("1,234,567.125");
    expect(formatDecimal("5", 2, "en-US")).toBe("5.00");
  });
});

describe("dates", () => {
  test("typed and pasted dates", () => {
    expect(parseUserDate("2026-01-31")).toBe("2026-01-31");
    expect(parseUserDate("1/31/2026", "en-US")).toBe("2026-01-31");
    expect(parseUserDate("31.01.2026", "de-DE")).toBe("2026-01-31");
    expect(parseUserDate("31/01/26", "en-GB")).toBe("2026-01-31");
    expect(parseUserDate("2026-02-30")).toBeNull();
    expect(parseUserDate("46053")).toBe("2026-01-31"); // an Excel serial day
  });

  test("timestamps become days", () => {
    expect(toDay("2026-03-04")).toBe("2026-03-04");
    expect(toDay("nope")).toBeNull();
  });

  test("relative ranges", () => {
    const today = new Date(2026, 10, 18); // 18 November 2026
    expect(resolveRange("today", today)).toEqual({ from: "2026-11-18", to: "2026-11-18" });
    expect(resolveRange("thisMonth", today)).toEqual({ from: "2026-11-01", to: "2026-11-30" });
    expect(resolveRange("lastMonth", today)).toEqual({ from: "2026-10-01", to: "2026-10-31" });
    expect(resolveRange("thisQuarter", today)).toEqual({ from: "2026-10-01", to: "2026-12-31" });
    expect(resolveRange("thisYear", today)).toEqual({ from: "2026-01-01", to: "2026-12-31" });
    expect(resolveRange("lastMonth", new Date(2026, 0, 15))).toEqual({ from: "2025-12-01", to: "2025-12-31" });
  });
});

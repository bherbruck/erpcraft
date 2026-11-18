import { expect, test } from "bun:test";
import { formatCalendarDate } from "./dates";

test("a calendar date shows the day it names", () => {
  expect(formatCalendarDate("2026-01-01")).toContain("2026");
  expect(formatCalendarDate("2026-01-01")).toMatch(/\b1\b/);
  expect(formatCalendarDate("2026-12-31")).toMatch(/\b31\b/);
  expect(formatCalendarDate("not a date")).toBe("not a date");
});

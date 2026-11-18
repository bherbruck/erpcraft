// Two kinds of dates, kept apart:
// a calendar date ("2026-03-15") is the same day everywhere and is never passed
// through a time zone; a timestamp is an instant and is shown in the user's zone.

const CALENDAR = /^(\d{4})-(\d{2})-(\d{2})$/;

const calendarFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" });
const timestampFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

/**
 * A calendar date for display. The date is placed at midnight UTC and formatted
 * in UTC, so the user's own time zone cannot move it to another day.
 */
export function formatCalendarDate(value: string): string {
  const match = CALENDAR.exec(value);
  if (!match) return value;
  const [, year = "", month = "", day = ""] = match;
  const instant = Date.UTC(parseInt(year, 10), parseInt(month, 10) - 1, parseInt(day, 10));
  return calendarFormat.format(instant);
}

/** An instant, in the user's time zone. */
export function formatTimestamp(value: string): string {
  const instant = Date.parse(value);
  return Number.isNaN(instant) ? value : timestampFormat.format(instant);
}

/** Today's calendar date where the user is. */
export function todayCalendarDate(): string {
  const now = new Date();
  const pad = (n: number): string => String(n).padStart(2, "0");
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

/**
 * A calendar date as the local midnight a date picker shows it on, and back. The picker works
 * in local days, so a day chosen in it is the same day written as text; no time zone is applied.
 */
export function calendarDateToLocal(value: string): Date | undefined {
  const match = CALENDAR.exec(value);
  if (!match) return undefined;
  const [, year = "", month = "", day = ""] = match;
  return new Date(parseInt(year, 10), parseInt(month, 10) - 1, parseInt(day, 10));
}

export function localToCalendarDate(date: Date): string {
  const pad = (n: number): string => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

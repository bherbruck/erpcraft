// Deterministic demo data: the same rows on every load, so tests can rely on them.

import type { Option } from "../src";

export function random(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const CUSTOMERS: Option[] = [
  "Acme Corp",
  "Globex",
  "Initech",
  "Umbrella",
  "Hooli",
  "Stark Industries",
  "Wayne Enterprises",
  "Wonka Industries",
  "Tyrell Corp",
  "Cyberdyne",
  "Soylent",
  "Vandelay Industries",
].map((label, i) => ({ value: `c-${i + 1}`, label }));

export const STATES: Option[] = [
  { value: "draft", label: "Draft" },
  { value: "submitted", label: "Submitted" },
  { value: "posted", label: "Posted" },
  { value: "cancelled", label: "Cancelled" },
];

export const ITEMS: Option[] = [
  "Bolt M6",
  "Bolt M8",
  "Nut M6",
  "Washer 6mm",
  "Steel plate 2mm",
  "Copper wire 1.5",
  "Paint white 1L",
  "Pallet",
].map((label, i) => ({ value: `i-${i + 1}`, label }));

export interface Order {
  id: string;
  number: string;
  customer: string;
  status: string;
  doc_date: string;
  qty: string;
  amount: string;
  currency: string;
  memo: string | null;
  urgent: boolean;
}

const pad = (n: number, w = 2) => String(n).padStart(w, "0");

export function makeOrders(count: number, seed = 42): Order[] {
  const next = random(seed);
  const rows: Order[] = [];
  const memos = ["", "Rush", "Call before delivery", "Partial ok", "Back order", "Gift wrap", ""];
  for (let i = 0; i < count; i += 1) {
    const month = 1 + Math.floor(next() * 12);
    const day = 1 + Math.floor(next() * 28);
    const cents = Math.floor(next() * 10_000_00);
    const memo = memos[Math.floor(next() * memos.length)] ?? "";
    rows.push({
      id: `o-${i + 1}`,
      number: `SO-${pad(i + 1, 6)}`,
      customer: CUSTOMERS[Math.floor(next() * CUSTOMERS.length)]?.value ?? "c-1",
      status: STATES[Math.floor(next() * STATES.length)]?.value ?? "draft",
      doc_date: `2026-${pad(month)}-${pad(day)}`,
      qty: String(1 + Math.floor(next() * 500)),
      amount: `${Math.floor(cents / 100)}.${pad(cents % 100)}`,
      currency: "EUR",
      memo: memo === "" ? null : memo,
      urgent: next() < 0.2,
    });
  }
  return rows;
}

/** Customers as a server would find them: by name, after a short wait. */
export function searchCustomers(search: string, signal: AbortSignal): Promise<Option[]> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      const lower = search.trim().toLowerCase();
      resolve(CUSTOMERS.filter((c) => c.label.toLowerCase().includes(lower)));
    }, 120);
    signal.addEventListener("abort", () => {
      clearTimeout(timer);
      reject(new DOMException("aborted", "AbortError"));
    });
  });
}

/** "purchase_order" -> "Purchase order". For names the server gives no label for. */
export function humanize(name: string): string {
  const words = name.replaceAll(/[_.]+/g, " ").trim();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** A new idempotency key. */
export function newKey(): string {
  return crypto.randomUUID();
}

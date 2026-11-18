import { en, type MessageKey } from "./en";

export type { MessageKey };

/**
 * The UI's own text. Labels of fields, entities, document types and states come
 * from the server; everything else a person reads comes from here.
 * `{name}` in a message is replaced by `params.name`.
 */
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const template: string = en[key];
  if (!params) return template;
  return template.replaceAll(/\{(\w+)\}/g, (whole, name: string) => {
    const value = params[name];
    return value === undefined ? whole : String(value);
  });
}

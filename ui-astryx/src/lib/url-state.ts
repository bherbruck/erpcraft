import { useCallback } from "react";
import { useSearchParams } from "react-router";

/**
 * View state that lives in the URL (search text, sort, page, tab, filters), so
 * that Back, reload and a shared link all restore what the user saw.
 */
export function useUrlState(): {
  get: (name: string) => string;
  /** Set or (with `null` or "") remove parameters. `replace` keeps Back from stepping through keystrokes. */
  set: (changes: Record<string, string | null>, options?: { replace?: boolean }) => void;
} {
  const [params, setParams] = useSearchParams();
  const get = useCallback((name: string) => params.get(name) ?? "", [params]);
  const set = useCallback(
    (changes: Record<string, string | null>, options?: { replace?: boolean }) => {
      setParams(
        (previous) => {
          const next = new URLSearchParams(previous);
          for (const [name, value] of Object.entries(changes)) {
            if (value === null || value === "") next.delete(name);
            else next.set(name, value);
          }
          return next;
        },
        { replace: options?.replace ?? false },
      );
    },
    [setParams],
  );
  return { get, set };
}

/** The page number in the URL, starting at 1. */
export function pageFromUrl(value: string): number {
  const page = /^\d+$/.test(value) ? parseInt(value, 10) : 1;
  return Math.max(page, 1);
}

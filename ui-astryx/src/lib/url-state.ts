import { useSearchParams } from "react-router";

/** List view state (search, filter, sort, page) lives in the URL, so Back goes back and every view is a link. */
export function useListState(defaultSort: string) {
  const [params, setParams] = useSearchParams();
  const q = params.get("q") ?? "";
  const status = params.get("status") ?? "";
  const sort = params.get("sort") ?? defaultSort;
  const page = Math.max(1, Number.parseInt(params.get("page") ?? "1", 10) || 1);
  const size = Math.min(200, Math.max(10, Number.parseInt(params.get("size") ?? "25", 10) || 25));

  const update = (changes: Record<string, string | number | null>, replace = false) => {
    const next = new URLSearchParams(params);
    for (const [key, value] of Object.entries(changes)) {
      if (value === null || value === "") next.delete(key);
      else next.set(key, String(value));
    }
    setParams(next, { replace });
  };

  return {
    q,
    status,
    sort,
    page,
    size,
    // Typing replaces the history entry, so Back does not step through every keystroke.
    setQ: (value: string) => update({ q: value, page: null }, true),
    setStatus: (value: string) => update({ status: value, page: null }),
    setSort: (value: string) => update({ sort: value === defaultSort ? null : value, page: null }),
    setPage: (value: number) => update({ page: value === 1 ? null : value }),
    setSize: (value: number) => update({ size: value === 25 ? null : value, page: null }),
    clear: () => update({ q: null, status: null, page: null }),
    filtered: q !== "" || status !== "" || page > 1,
  };
}

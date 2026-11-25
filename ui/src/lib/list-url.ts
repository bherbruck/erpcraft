import { pageFromUrl, useUrlState } from "./url-state";

/** The URL's sort and paging, shared by the generated lists: the grid reports them, the URL keeps them. */
export function useListUrl(defaultSize = 25) {
  const url = useUrlState();
  // `sort=-doc_date,number`: the grid sorts by several columns, the server takes the same keys.
  const sort = url.get("sort").split(",").filter(Boolean);
  const page = pageFromUrl(url.get("page"));
  const size = /^\d+$/.test(url.get("size")) ? parseInt(url.get("size"), 10) || defaultSize : defaultSize;
  const setQuery = (next: { sort: string[]; page: number; size: number }): void => {
    const changes = {
      sort: next.sort.join(",") || null,
      page: next.page === 1 ? null : String(next.page),
      size: next.size === defaultSize ? null : String(next.size),
    };
    // The grid reports its state when it starts too; only a real change is a step in history.
    if (
      (changes.sort ?? "") === url.get("sort") &&
      (changes.page ?? "") === url.get("page") &&
      (changes.size ?? "") === url.get("size")
    )
      return;
    url.set(changes);
  };
  return { url, sort, page, size, setQuery };
}

import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "@/api/client";
import { t } from "@/i18n/t";

export interface AsyncState<T> {
  /** The last result. Kept while a reload runs, so the screen does not jump. */
  data: T | undefined;
  error: ApiError | null;
  loading: boolean;
  reload: () => void;
}

/**
 * Load something from the server and load it again whenever `key` changes.
 * Pass `null` as the key to load nothing.
 */
export function useAsync<T>(key: string | null, load: (signal: AbortSignal) => Promise<T>): AsyncState<T> {
  const [state, setState] = useState<{
    data: T | undefined;
    error: ApiError | null;
    loading: boolean;
    key: string | null;
  }>({
    data: undefined,
    error: null,
    loading: key !== null,
    key,
  });
  const [generation, setGeneration] = useState(0);
  const loadRef = useRef(load);
  loadRef.current = load;

  // biome-ignore lint/correctness/useExhaustiveDependencies: `generation` forces a reload
  useEffect(() => {
    if (key === null) {
      setState({ data: undefined, error: null, loading: false, key });
      return;
    }
    const controller = new AbortController();
    setState((previous) => ({
      data: previous.key === key ? previous.data : undefined,
      error: null,
      loading: true,
      key,
    }));
    // A read that never reached the server (a dropped connection) is tried once
    // more before the screen says so; reading twice changes nothing.
    const attempt = (): Promise<T> =>
      loadRef.current(controller.signal).catch(async (cause: unknown) => {
        if (!(cause instanceof ApiError) || cause.status !== 0 || controller.signal.aborted) throw cause;
        await new Promise((done) => setTimeout(done, 300));
        if (controller.signal.aborted) throw cause;
        return loadRef.current(controller.signal);
      });
    attempt().then(
      (data) => {
        if (!controller.signal.aborted) setState({ data, error: null, loading: false, key });
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return;
        const error = cause instanceof ApiError ? cause : new ApiError(0, "error", t("error.unexpected"));
        setState((previous) => ({ data: previous.data, error, loading: false, key }));
      },
    );
    return () => controller.abort();
  }, [key, generation]);

  const reload = useCallback(() => setGeneration((n) => n + 1), []);
  const current = state.key === key;
  return {
    data: current ? state.data : undefined,
    error: current ? state.error : null,
    loading: current ? state.loading : key !== null,
    reload,
  };
}

import { type DependencyList, useEffect, useState } from "react";

export type Async<T> =
  | { state: "loading"; data: T | null }
  | { state: "ready"; data: T }
  | { state: "error"; data: T | null; error: Error };

/** Runs a load whenever the dependencies change. Keeps the last data while reloading so nothing jumps. */
export function useAsync<T>(load: (signal: AbortSignal) => Promise<T>, deps: DependencyList): Async<T> & { reload: () => void } {
  const [result, setResult] = useState<Async<T>>({ state: "loading", data: null });
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const controller = new AbortController();
    setResult((previous) => ({ state: "loading", data: previous.data }));
    load(controller.signal).then(
      (data) => {
        if (!controller.signal.aborted) setResult({ state: "ready", data });
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        setResult((previous) => ({
          state: "error",
          data: previous.data,
          error: error instanceof Error ? error : new Error(String(error)),
        }));
      },
    );
    return () => controller.abort();
    // biome-ignore lint/correctness/useExhaustiveDependencies: the caller names the dependencies.
  }, [...deps, tick]);
  return { ...result, reload: () => setTick((n) => n + 1) };
}

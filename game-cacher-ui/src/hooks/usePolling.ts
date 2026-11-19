import { useCallback, useEffect, useRef, useState } from "react";
import { isAbortError } from "@/api/client";

export type PollingResult<T> = {
  /** Last successful result (kept while later polls fail). */
  data: T | undefined;
  /** Error from the most recent attempt, cleared on the next success. */
  error: Error | null;
  /** True until the first attempt (success or failure) completes. */
  loading: boolean;
  /** Fetch again now (and restart the interval). */
  refresh: () => void;
  /** Replace the data locally, e.g. with a mutation's response. */
  setData: (data: T) => void;
};

/**
 * Calls `fn` immediately and then every `intervalMs` (0 = once, no polling)
 * while `enabled`. Polls are skipped while the tab is hidden and resume on
 * focus. `fn` should be memoized with `useCallback`; changing it refetches.
 */
export function usePolling<T>(
  fn: (signal: AbortSignal) => Promise<T>,
  intervalMs: number,
  enabled = true,
): PollingResult<T> {
  const [data, setDataState] = useState<T | undefined>(undefined);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  // Track which fn the current data belongs to, so switching (e.g. to another
  // repo) shows a loading state instead of the previous item's data.
  const fnRef = useRef(fn);

  useEffect(() => {
    if (!enabled) return;

    if (fnRef.current !== fn) {
      fnRef.current = fn;
      setDataState(undefined);
      setError(null);
      setLoading(true);
    }

    let cancelled = false;
    let timer: number | undefined;
    let controller: AbortController | null = null;

    const schedule = () => {
      if (!cancelled && intervalMs > 0) timer = window.setTimeout(run, intervalMs);
    };

    const run = async () => {
      if (document.hidden && intervalMs > 0) {
        schedule();
        return;
      }
      controller = new AbortController();
      try {
        const result = await fn(controller.signal);
        if (cancelled) return;
        setDataState(result);
        setError(null);
      } catch (e) {
        if (cancelled || isAbortError(e)) return;
        setError(e instanceof Error ? e : new Error(String(e)));
      }
      setLoading(false);
      schedule();
    };

    const onVisible = () => {
      if (!document.hidden && intervalMs > 0) {
        window.clearTimeout(timer);
        controller?.abort();
        void run();
      }
    };

    void run();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      cancelled = true;
      controller?.abort();
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [fn, intervalMs, enabled, tick]);

  const refresh = useCallback(() => setTick((t) => t + 1), []);
  const setData = useCallback((value: T) => {
    setDataState(value);
    setError(null);
    setLoading(false);
  }, []);

  return { data, error, loading, refresh, setData };
}

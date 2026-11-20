import { useCallback, useEffect, useMemo, useState } from "react";
import { listRuns } from "@/api/client";
import type { UpdateRun } from "@/api/types";
import { usePolling, type PollingResult } from "./usePolling";

const RUNNING_POLL_MS = 2000;

/** `GET /api/runs`, polled every 2 s while a run is active and every `idleMs` otherwise. */
export function useRuns(idleMs: number): PollingResult<UpdateRun[]> & { active: UpdateRun[] } {
  const load = useCallback((signal: AbortSignal) => listRuns(signal), []);
  const [fast, setFast] = useState(false);
  const polling = usePolling(load, fast ? RUNNING_POLL_MS : idleMs);
  const active = useMemo(() => (polling.data ?? []).filter((r) => r.status === "running"), [polling.data]);
  const anyActive = active.length > 0;
  useEffect(() => setFast(anyActive), [anyActive]);
  return { ...polling, active };
}

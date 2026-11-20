import type { PlatformSummary, UpdateRun } from "@/api/types";

/** Seconds a run took (or has taken so far, while running); null for an incomplete run. */
export function runDurationSecs(run: UpdateRun, now: number = Date.now()): number | null {
  const end = run.finished ? Date.parse(run.finished) : run.status === "running" ? now : null;
  return end === null ? null : (end - Date.parse(run.started)) / 1000;
}

/** Display name of a platform id, falling back to the id. */
export function platformName(platforms: PlatformSummary[] | undefined, id: string): string {
  return platforms?.find((p) => p.id === id)?.name ?? id;
}

// Managed-update schedule rules, mirroring the server (game-cacher/src/schedule.rs).
// Imported by the mock API too, so keep this free of `@/` aliases.

export const TIME_RE = /^([01]\d|2[0-3]):[0-5]\d$/;
export const MIN_GAP_MINUTES = 5;
const DAY_MINUTES = 24 * 60;

/** Sorted and de-duplicated, the form the server stores. */
export function normalizeTimes(times: string[]): string[] {
  return [...new Set(times)].sort();
}

function toMinutes(t: string): number {
  return Number(t.slice(0, 2)) * 60 + Number(t.slice(3));
}

/** First pair of neighbouring times (also across midnight) under the minimum gap; expects valid, normalized times. */
export function tooClose(times: string[]): [string, string] | null {
  if (times.length < 2) return null;
  for (let i = 0; i < times.length; i++) {
    const [a, b] = [times[i], times[(i + 1) % times.length]];
    const gap = (toMinutes(b) - toMinutes(a) + DAY_MINUTES) % DAY_MINUTES;
    if (gap < MIN_GAP_MINUTES) return [a, b];
  }
  return null;
}

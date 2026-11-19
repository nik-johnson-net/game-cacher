const DASH = "—";

/** Rounds to about 3 significant figures ("18.6 GB", "4.68 GB", "235 GB"). */
function sig3(value: number): string {
  const decimals = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return value.toFixed(decimals).replace(/\.0+$|(\.\d*[1-9])0+$/, "$1");
}

const BYTE_UNITS = ["B", "kB", "MB", "GB", "TB", "PB"] as const;

/** Decimal (SI) byte formatting: 18_556_686_336 → "18.6 GB". */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes)) return DASH;
  if (bytes < 1000) return `${Math.round(bytes)} B`;
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < BYTE_UNITS.length - 1) {
    value /= 1000;
    unit++;
  }
  // Avoid "1000 MB": sig3 may round 999.6 up to 1000.
  if (Number(sig3(value)) >= 1000 && unit < BYTE_UNITS.length - 1) {
    value /= 1000;
    unit++;
  }
  return `${sig3(value)} ${BYTE_UNITS[unit]}`;
}

const compactNumber = new Intl.NumberFormat("en-US", { notation: "compact", maximumFractionDigits: 1 });

/** Compact counts: 1_234_567 → "1.2M". */
export function formatCount(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return DASH;
  return compactNumber.format(n);
}

/** Exact integer with separators: 98304 → "98,304". */
export function formatInt(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return DASH;
  return n.toLocaleString("en-US");
}

/** Relative time: "just now", "5m ago", "3h ago" (and "in 5m" for the future). */
export function formatRelativeTime(iso: string | null | undefined, now: number = Date.now()): string {
  if (!iso) return DASH;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return DASH;
  const diff = Math.round((now - t) / 1000);
  const abs = Math.abs(diff);
  let text: string;
  if (abs < 45) return "just now";
  if (abs < 3600) text = `${Math.round(abs / 60)}m`;
  else if (abs < 86400) text = `${Math.round(abs / 3600)}h`;
  else if (abs < 86400 * 30) text = `${Math.round(abs / 86400)}d`;
  else if (abs < 86400 * 365) text = `${Math.round(abs / (86400 * 30))}mo`;
  else text = `${Math.round(abs / (86400 * 365))}y`;
  return diff >= 0 ? `${text} ago` : `in ${text}`;
}

/** Absolute local timestamp, for tooltips: "2026-11-03 12:00:00". */
export function formatTimestamp(iso: string | null | undefined): string {
  if (!iso) return DASH;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return DASH;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** Durations: 45 → "45s", 192 → "3m 12s", 3720 → "1h 02m", 200000 → "2d 07h". */
export function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return DASH;
  const s = Math.round(seconds);
  if (s < 60) return `${s}s`;
  const pad = (n: number) => String(n).padStart(2, "0");
  if (s < 3600) return `${Math.floor(s / 60)}m ${pad(s % 60)}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${pad(Math.floor((s % 3600) / 60))}m`;
  return `${Math.floor(s / 86400)}d ${pad(Math.floor((s % 86400) / 3600))}h`;
}

/** Percentage 0..100. */
export function percent(done: number, total: number): number {
  if (!(total > 0)) return 0;
  return Math.max(0, Math.min(100, (done / total) * 100));
}

/** Cache hit ratio (hits / (hits + misses)) as 0..1, or null with no cacheable traffic. */
export function hitRatio(hits: number, misses: number): number | null {
  const total = hits + misses;
  return total > 0 ? hits / total : null;
}

/** "83%" or "—". */
export function formatRatio(ratio: number | null): string {
  return ratio === null ? DASH : `${Math.round(ratio * 100)}%`;
}

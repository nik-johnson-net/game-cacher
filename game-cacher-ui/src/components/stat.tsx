import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** Compact label/value tile used on the dashboard and platform pages. */
export function Stat({ label, value, hint, className }: { label: string; value: ReactNode; hint?: ReactNode; className?: string }) {
  return (
    <div className={cn("bg-bg-surface border border-border px-4 py-3 min-w-0", className)}>
      <div className="text-xs uppercase tracking-[0.5px] text-text-secondary">{label}</div>
      <div className="text-xl font-semibold text-text-primary tabular-nums mt-1 break-words">{value}</div>
      {hint && <div className="text-xs text-text-muted mt-0.5">{hint}</div>}
    </div>
  );
}

/** Inline `KEY value` row used inside cards. */
export function KeyValue({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex items-baseline gap-3 py-1.5 border-b border-border-muted last:border-b-0 text-sm">
      <dt className="w-36 shrink-0 text-xs uppercase tracking-[0.5px] text-text-secondary">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

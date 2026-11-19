import { cn } from "@/lib/utils";
import type { Tone } from "@/lib/status";

const fillTone: Record<Tone, string> = {
  ok: "bg-status-ok",
  warn: "bg-status-warn",
  info: "bg-status-info",
  active: "bg-accent",
  error: "bg-status-error",
  neutral: "bg-status-neutral",
};

interface ProgressBarProps {
  /** 0..100 */
  value: number;
  tone?: Tone;
  showLabel?: boolean;
  /** Width class for the track; defaults to the house 80px. */
  trackClassName?: string;
  className?: string;
  label?: string;
}

export function ProgressBar({ value, tone = "active", showLabel = true, trackClassName, className, label }: ProgressBarProps) {
  const clamped = Math.max(0, Math.min(100, value));
  const text = clamped >= 10 || clamped === 0 ? `${Math.floor(clamped)}%` : `${clamped.toFixed(1)}%`;
  return (
    <div className={cn("flex items-center gap-2", className)}>
      <div
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(clamped)}
        aria-label={label}
        className={cn("w-20 h-1.5 bg-bg-overlay rounded-sm overflow-hidden", trackClassName)}
      >
        <div className={cn("h-full rounded-sm transition-[width] duration-500", fillTone[tone])} style={{ width: `${clamped}%` }} />
      </div>
      {showLabel && <span className="text-xs text-text-secondary tabular-nums w-10 shrink-0">{text}</span>}
    </div>
  );
}

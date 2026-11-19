import { cn } from "@/lib/utils";
import type { StatusSpec, Tone } from "@/lib/status";

const toneClasses: Record<Tone, { dot: string; bg: string; text: string }> = {
  ok: { dot: "bg-status-ok", bg: "bg-status-ok-bg", text: "text-status-ok" },
  warn: { dot: "bg-status-warn", bg: "bg-status-warn-bg", text: "text-status-warn" },
  info: { dot: "bg-status-info", bg: "bg-status-info-bg", text: "text-status-info" },
  active: { dot: "bg-status-active", bg: "bg-status-active-bg", text: "text-status-active" },
  error: { dot: "bg-status-error", bg: "bg-status-error-bg", text: "text-status-error" },
  neutral: { dot: "bg-status-neutral", bg: "bg-status-neutral-bg", text: "text-status-neutral" },
};

interface StatusBadgeProps extends React.ComponentProps<"span"> {
  spec: StatusSpec;
}

/** Dot + label badge for a lifecycle state (see lib/status.ts for the mappings). */
export function StatusBadge({ spec, className, ...props }: StatusBadgeProps) {
  const t = toneClasses[spec.tone];
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-sm text-xs font-medium whitespace-nowrap",
        t.bg,
        t.text,
        className,
      )}
      {...props}
    >
      <span className={cn("size-1.5 rounded-full shrink-0", t.dot, spec.pulse && "animate-pulse-dot")} />
      {spec.label}
    </span>
  );
}

/** Text color class for a tone (for inline status text and icons). */
// eslint-disable-next-line react-refresh/only-export-components
export function toneText(tone: Tone): string {
  return toneClasses[tone].text;
}

import type * as React from "react";
import { LoaderCircle } from "lucide-react";
import { cn } from "@/lib/utils";

export function Skeleton({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("bg-bg-raised animate-pulse rounded-sm", className)} {...props} />;
}

export function Spinner({ className }: { className?: string }) {
  return <LoaderCircle aria-hidden className={cn("size-3.5 animate-spin text-accent", className)} />;
}

/** Centered spinner + label for whole-page or panel loading. */
export function Loading({ label = "Loading...", className }: { label?: string; className?: string }) {
  return (
    <div className={cn("flex items-center justify-center min-h-[160px]", className)} role="status">
      <div className="flex items-center gap-3 text-text-secondary">
        <Spinner className="size-5" />
        <span className="text-sm">{label}</span>
      </div>
    </div>
  );
}

/** Placeholder table rows while a list loads. */
export function SkeletonRows({ rows = 4, cols }: { rows?: number; cols: number }) {
  return (
    <>
      {Array.from({ length: rows }, (_, r) => (
        <tr key={r} className="odd:bg-bg-surface even:bg-bg-base border-b border-border-muted last:border-b-0">
          {Array.from({ length: cols }, (_, c) => (
            <td key={c} className="px-3 py-2.5">
              <Skeleton className={cn("h-3", c === 0 ? "w-48" : "w-16")} />
            </td>
          ))}
        </tr>
      ))}
    </>
  );
}

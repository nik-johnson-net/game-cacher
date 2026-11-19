import type * as React from "react";
import { cn } from "@/lib/utils";

// Plain HTML table primitives in the house style:
// border wrapper, bg-raised 11px uppercase header, zebra rows, hover bg-raised.

export function Table({ className, wrapperClassName, ...props }: React.ComponentProps<"table"> & { wrapperClassName?: string }) {
  return (
    <div className={cn("border border-border overflow-x-auto", wrapperClassName)}>
      <table className={cn("w-full border-collapse text-sm", className)} {...props} />
    </div>
  );
}

export function THead({ className, ...props }: React.ComponentProps<"thead">) {
  return <thead className={cn("bg-bg-raised", className)} {...props} />;
}

export function Th({ className, ...props }: React.ComponentProps<"th">) {
  return (
    <th
      scope="col"
      className={cn(
        "text-left text-xs font-semibold text-text-secondary uppercase tracking-[0.5px] px-3 py-2 border-b border-border whitespace-nowrap",
        className,
      )}
      {...props}
    />
  );
}

/** Body row with zebra striping (odd: surface, even: base) and hover highlight. */
export function Tr({ className, ...props }: React.ComponentProps<"tr">) {
  return (
    <tr
      className={cn(
        "odd:bg-bg-surface even:bg-bg-base hover:bg-bg-raised border-b border-border-muted last:border-b-0 transition-colors",
        className,
      )}
      {...props}
    />
  );
}

export function Td({ className, ...props }: React.ComponentProps<"td">) {
  return <td className={cn("px-3 py-2 text-sm text-text-primary align-middle", className)} {...props} />;
}

/** Single full-width cell for empty/loading rows. */
export function TableMessageRow({ colSpan, children }: { colSpan: number; children: React.ReactNode }) {
  return (
    <tr className="bg-bg-surface">
      <td colSpan={colSpan}>{children}</td>
    </tr>
  );
}

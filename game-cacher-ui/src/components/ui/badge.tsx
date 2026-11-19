import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

// Chips/labels (quant names, tags, "gated"). For lifecycle states use StatusBadge.
const badgeVariants = cva(
  "inline-flex items-center justify-center rounded-sm px-2 py-0.5 text-xs font-medium w-fit whitespace-nowrap shrink-0 [&>svg]:size-3 gap-1 [&>svg]:pointer-events-none overflow-hidden",
  {
    variants: {
      variant: {
        default: "bg-accent-muted text-accent border border-accent/20",
        secondary: "bg-bg-raised text-text-secondary border border-border",
        outline: "bg-transparent text-text-secondary border border-border",
        warn: "bg-status-warn-bg text-status-warn border border-warn-border",
        error: "bg-status-error-bg text-status-error border border-error-border",
        info: "bg-status-info-bg text-status-info border border-status-info/30",
        ok: "bg-status-ok-bg text-status-ok border border-ok-border",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  },
);

function Badge({ className, variant, ...props }: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span data-slot="badge" className={cn(badgeVariants({ variant }), className)} {...props} />;
}

// eslint-disable-next-line react-refresh/only-export-components
export { Badge, badgeVariants };

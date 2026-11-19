import * as React from "react";
import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const buttonVariants = cva(
  "inline-flex items-center justify-center gap-2 whitespace-nowrap text-xs font-medium rounded transition-colors disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-3.5 shrink-0 [&_svg]:shrink-0 outline-none focus-visible:ring-1 focus-visible:ring-accent cursor-pointer",
  {
    variants: {
      variant: {
        // Primary: accent bg, dark text
        default: "bg-accent text-on-accent border border-accent hover:bg-accent-hover hover:border-accent-hover",
        // Secondary: surface bg, primary text, border
        secondary: "bg-bg-surface text-text-primary border border-border hover:bg-bg-raised hover:border-border-strong",
        // Danger: surface bg, red text, error border
        danger: "bg-bg-surface text-status-error border border-error-border hover:bg-error-bg",
        // Ghost: transparent, secondary text
        ghost: "bg-transparent text-text-secondary border border-transparent hover:bg-bg-raised hover:text-text-primary",
        // Ghost danger: for row actions
        "ghost-danger": "bg-transparent text-text-secondary border border-transparent hover:bg-error-bg hover:text-status-error",
        // Inline text link
        link: "h-auto px-0 py-0 text-accent border-transparent bg-transparent hover:text-accent-hover hover:underline underline-offset-4",
      },
      size: {
        default: "h-8 px-4 py-2",
        sm: "h-7 px-3 py-1",
        xs: "h-6 px-2 py-0.5 gap-1 [&_svg:not([class*='size-'])]:size-3",
        icon: "size-8",
        "icon-sm": "size-6 [&_svg:not([class*='size-'])]:size-3.5",
      },
    },
    defaultVariants: {
      variant: "default",
      size: "default",
    },
  },
);

function Button({
  className,
  variant,
  size,
  asChild = false,
  type,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean;
  }) {
  const Comp = asChild ? Slot : "button";

  return (
    <Comp
      data-slot="button"
      // Default to type="button" so buttons inside forms don't submit by accident.
      type={asChild ? undefined : (type ?? "button")}
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  );
}

// eslint-disable-next-line react-refresh/only-export-components
export { Button, buttonVariants };

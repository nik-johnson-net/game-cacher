import type * as React from "react";
import { ExternalLink as ExternalLinkIcon } from "lucide-react";
import { cn } from "@/lib/utils";

interface ExternalLinkProps extends React.ComponentProps<"a"> {
  href: string;
  showIcon?: boolean;
}

/** Link that opens in a new tab, accent colored, with a small ↗ icon. */
export function ExternalLink({ href, showIcon = true, className, children, ...props }: ExternalLinkProps) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer noopener"
      className={cn("inline-flex items-center gap-1 text-accent hover:text-accent-hover hover:underline underline-offset-4 min-w-0", className)}
      {...props}
    >
      {children}
      {showIcon && <ExternalLinkIcon aria-hidden className="size-3 shrink-0 opacity-70" />}
    </a>
  );
}

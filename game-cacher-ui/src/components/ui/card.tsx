import type * as React from "react";
import { cn } from "@/lib/utils";

/** Sharp-cornered surface panel. Use CardHeader for a title bar, CardBody for padded content. */
function Card({ className, ...props }: React.ComponentProps<"section">) {
  return <section className={cn("bg-bg-surface border border-border", className)} {...props} />;
}

interface CardHeaderProps extends Omit<React.ComponentProps<"div">, "title"> {
  title: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
}

function CardHeader({ title, description, actions, className, ...props }: CardHeaderProps) {
  return (
    <div
      className={cn("flex flex-wrap items-center justify-between gap-2 px-4 py-2.5 border-b border-border", className)}
      {...props}
    >
      <div className="min-w-0">
        <h2 className="text-base font-semibold text-text-primary">{title}</h2>
        {description && <div className="text-xs text-text-secondary mt-0.5">{description}</div>}
      </div>
      {actions && <div className="flex items-center gap-2 shrink-0">{actions}</div>}
    </div>
  );
}

function CardBody({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("p-4", className)} {...props} />;
}

export { Card, CardHeader, CardBody };

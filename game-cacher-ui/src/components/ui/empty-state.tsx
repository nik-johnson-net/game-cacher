import type { ElementType, ReactNode } from "react";
import { cn } from "@/lib/utils";

interface EmptyStateProps {
  icon?: ElementType;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
  className?: string;
}

export function EmptyState({ icon: Icon, title, description, action, className }: EmptyStateProps) {
  return (
    <div className={cn("flex flex-col items-center justify-center py-8 px-4 text-center", className)}>
      {Icon && <Icon className="size-8 text-text-muted mb-3 opacity-50" />}
      <p className="text-lg font-medium text-text-primary mb-1">{title}</p>
      {description && <div className="text-sm text-text-secondary max-w-md">{description}</div>}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}

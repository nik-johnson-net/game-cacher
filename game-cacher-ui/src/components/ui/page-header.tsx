import type { ReactNode } from "react";
import { Link } from "react-router";
import { cn } from "@/lib/utils";

export interface BreadcrumbItem {
  label: string;
  href?: string;
}

export function Breadcrumbs({ items, className }: { items: BreadcrumbItem[]; className?: string }) {
  return (
    <nav aria-label="Breadcrumb" className={cn("flex items-center gap-1 text-xs text-text-muted mb-3", className)}>
      {items.map((item, index) => {
        const isLast = index === items.length - 1;
        return (
          <span key={index} className="flex items-center gap-1 min-w-0">
            {index > 0 && <span className="text-text-faint">/</span>}
            {isLast || !item.href ? (
              <span className="text-text-muted truncate" aria-current={isLast ? "page" : undefined}>
                {item.label}
              </span>
            ) : (
              <Link to={item.href} className="text-text-secondary hover:text-accent transition-colors">
                {item.label}
              </Link>
            )}
          </span>
        );
      })}
    </nav>
  );
}

interface PageHeaderProps {
  breadcrumbs?: BreadcrumbItem[];
  title: ReactNode;
  description?: ReactNode;
  status?: ReactNode;
  actions?: ReactNode;
  className?: string;
}

/** Every page starts with this: breadcrumbs, title (+ status), subtitle, right-aligned actions. */
export function PageHeader({ breadcrumbs, title, description, status, actions, className }: PageHeaderProps) {
  return (
    <div className={cn("mb-6", className)}>
      {breadcrumbs && breadcrumbs.length > 0 && <Breadcrumbs items={breadcrumbs} />}
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="flex-1 min-w-0">
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="text-2xl font-semibold text-text-primary break-all">{title}</h1>
            {status && <div className="shrink-0 flex items-center gap-2">{status}</div>}
          </div>
          {description && <div className="text-sm text-text-secondary mt-1">{description}</div>}
        </div>
        {actions && <div className="flex flex-wrap items-center gap-2 shrink-0">{actions}</div>}
      </div>
    </div>
  );
}

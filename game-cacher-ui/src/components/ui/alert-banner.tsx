import type { ReactNode } from "react";
import { CircleAlert, CircleCheck, Info, RefreshCw, TriangleAlert } from "lucide-react";
import { cn } from "@/lib/utils";
import { Button } from "./button";

type Variant = "success" | "error" | "warning" | "info";

const styles: Record<Variant, string> = {
  success: "bg-ok-bg border-status-ok text-status-ok",
  warning: "bg-warn-bg border-status-warn text-status-warn",
  error: "bg-error-bg border-status-error text-status-error",
  info: "bg-status-info-bg border-status-info text-status-info",
};

const icons = { success: CircleCheck, warning: TriangleAlert, error: CircleAlert, info: Info };

interface AlertBannerProps {
  variant: Variant;
  /** Main message; banner renders nothing when empty and there are no children. */
  message?: string | null;
  title?: string;
  children?: ReactNode;
  onRetry?: () => void;
  actions?: ReactNode;
  className?: string;
}

/** Inline banner with a 3px colored left border. Errors usually pass `onRetry`. */
export function AlertBanner({ variant, message, title, children, onRetry, actions, className }: AlertBannerProps) {
  if (!message && !children) return null;
  const Icon = icons[variant];
  return (
    <div
      role={variant === "error" ? "alert" : "status"}
      className={cn("flex items-start gap-2 border-l-[3px] px-3 py-2 text-xs", styles[variant], className)}
    >
      <Icon className="size-3.5 mt-px shrink-0" />
      <div className="flex-1 min-w-0 break-words">
        {title && <div className="font-semibold mb-0.5">{title}</div>}
        {message && <div className={cn(title && "text-text-secondary")}>{message}</div>}
        {children}
      </div>
      {(onRetry || actions) && (
        <div className="flex items-center gap-2 shrink-0 -my-0.5">
          {actions}
          {onRetry && (
            <Button variant="ghost" size="xs" onClick={onRetry}>
              <RefreshCw />
              Retry
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

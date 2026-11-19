import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { CircleAlert, CircleCheck, Info, X } from "lucide-react";
import { cn } from "@/lib/utils";

export type ToastVariant = "success" | "error" | "info";
type Toast = { id: number; variant: ToastVariant; message: ReactNode; title?: string };

type ToastApi = {
  push: (toast: { variant?: ToastVariant; title?: string; message: ReactNode; durationMs?: number }) => void;
};

const ToastContext = createContext<ToastApi | null>(null);

const variantStyle: Record<ToastVariant, string> = {
  success: "border-l-status-ok",
  error: "border-l-status-error",
  info: "border-l-status-info",
};
const variantIcon = { success: CircleCheck, error: CircleAlert, info: Info };
const variantIconColor: Record<ToastVariant, string> = {
  success: "text-status-ok",
  error: "text-status-error",
  info: "text-status-info",
};

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const seq = useRef(0);

  const dismiss = useCallback((id: number) => setToasts((ts) => ts.filter((t) => t.id !== id)), []);

  const push = useCallback<ToastApi["push"]>(
    ({ variant = "info", title, message, durationMs }) => {
      seq.current += 1;
      const id = seq.current;
      setToasts((ts) => [...ts.slice(-4), { id, variant, title, message }]);
      window.setTimeout(() => dismiss(id), durationMs ?? (variant === "error" ? 8000 : 5000));
    },
    [dismiss],
  );

  const api = useMemo(() => ({ push }), [push]);

  return (
    <ToastContext.Provider value={api}>
      {children}
      <div aria-live="polite" className="fixed bottom-4 right-4 z-[60] flex flex-col gap-2 w-[min(380px,calc(100vw-2rem))]">
        {toasts.map((t) => {
          const Icon = variantIcon[t.variant];
          return (
            <div
              key={t.id}
              role={t.variant === "error" ? "alert" : "status"}
              className={cn(
                "flex items-start gap-2 bg-bg-overlay border border-border border-l-[3px] px-3 py-2 shadow-lg animate-in fade-in-0 slide-in-from-bottom-2",
                variantStyle[t.variant],
              )}
            >
              <Icon className={cn("size-3.5 mt-0.5 shrink-0", variantIconColor[t.variant])} />
              <div className="flex-1 min-w-0 text-xs break-words">
                {t.title && <div className="font-semibold text-text-primary">{t.title}</div>}
                <div className="text-text-secondary">{t.message}</div>
              </div>
              <button
                type="button"
                onClick={() => dismiss(t.id)}
                aria-label="Dismiss notification"
                className="text-text-muted hover:text-text-primary cursor-pointer shrink-0"
              >
                <X className="size-3.5" />
              </button>
            </div>
          );
        })}
      </div>
    </ToastContext.Provider>
  );
}

// eslint-disable-next-line react-refresh/only-export-components
export function useToast(): ToastApi {
  const ctx = useContext(ToastContext);
  if (!ctx) throw new Error("useToast must be used inside <ToastProvider>");
  return ctx;
}

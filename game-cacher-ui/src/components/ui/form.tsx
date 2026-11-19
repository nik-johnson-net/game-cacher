import * as React from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";

const controlBase =
  "w-full min-w-0 bg-bg-base border border-border rounded-sm text-sm text-text-primary placeholder:text-text-faint transition-colors outline-none focus:border-accent focus:ring-1 focus:ring-accent/50 aria-invalid:border-status-error aria-invalid:ring-1 aria-invalid:ring-status-error/30 disabled:cursor-not-allowed disabled:opacity-50";

export function Input({ className, ...props }: React.ComponentProps<"input">) {
  return <input className={cn(controlBase, "h-8 px-3 py-1", className)} {...props} />;
}

export function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return <textarea className={cn(controlBase, "px-3 py-2 min-h-16", className)} {...props} />;
}

/** Native select with a lucide chevron (no hard-coded SVG colors). */
export function Select({ className, children, ...props }: React.ComponentProps<"select">) {
  return (
    <div className={cn("relative", className)}>
      <select className={cn(controlBase, "h-8 pl-3 pr-8 appearance-none cursor-pointer")} {...props}>
        {children}
      </select>
      <ChevronDown className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 size-3.5 text-text-secondary" />
    </div>
  );
}

export function Label({ className, ...props }: React.ComponentProps<"label">) {
  return (
    <label
      className={cn("block text-xs font-semibold text-text-secondary uppercase tracking-[0.5px] mb-1", className)}
      {...props}
    />
  );
}

export function FieldHint({ className, ...props }: React.ComponentProps<"p">) {
  return <p className={cn("text-xs text-text-muted mt-1", className)} {...props} />;
}

export function FieldError({ error, className }: { error?: string | null; className?: string }) {
  if (!error) return null;
  return (
    <p role="alert" className={cn("text-xs text-status-error mt-1", className)}>
      {error}
    </p>
  );
}

interface FieldProps {
  label: React.ReactNode;
  htmlFor?: string;
  hint?: React.ReactNode;
  error?: string | null;
  className?: string;
  children: React.ReactNode;
}

/** Label + control + hint/error, with the standard 16px group spacing handled by the parent. */
export function Field({ label, htmlFor, hint, error, className, children }: FieldProps) {
  return (
    <div className={className}>
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
      {error ? <FieldError error={error} /> : hint ? <FieldHint>{hint}</FieldHint> : null}
    </div>
  );
}

interface CheckboxFieldProps extends Omit<React.ComponentProps<"input">, "type" | "onChange"> {
  label: React.ReactNode;
  hint?: React.ReactNode;
  onCheckedChange: (checked: boolean) => void;
}

export function CheckboxField({ label, hint, onCheckedChange, className, id, ...props }: CheckboxFieldProps) {
  const autoId = React.useId();
  const inputId = id ?? autoId;
  return (
    <div className={cn("flex items-start gap-2", className)}>
      <input
        id={inputId}
        type="checkbox"
        className="mt-0.5 size-3.5 cursor-pointer"
        onChange={(e) => onCheckedChange(e.target.checked)}
        {...props}
      />
      <label htmlFor={inputId} className="text-sm text-text-primary cursor-pointer select-none">
        {label}
        {hint && <span className="block text-xs text-text-muted">{hint}</span>}
      </label>
    </div>
  );
}

interface SwitchProps {
  checked: boolean;
  onCheckedChange: (checked: boolean) => void;
  disabled?: boolean;
  "aria-label": string;
  className?: string;
}

/** Accessible toggle (role="switch"). */
export function Switch({ checked, onCheckedChange, disabled, className, ...props }: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={props["aria-label"]}
      disabled={disabled}
      onClick={() => onCheckedChange(!checked)}
      className={cn(
        "relative inline-flex h-4 w-7 shrink-0 items-center rounded-full border transition-colors cursor-pointer outline-none focus-visible:ring-1 focus-visible:ring-accent disabled:cursor-not-allowed disabled:opacity-50",
        checked ? "bg-accent-muted border-accent" : "bg-bg-base border-border",
        className,
      )}
    >
      <span
        className={cn(
          "inline-block size-2.5 rounded-full transition-transform",
          checked ? "translate-x-3.5 bg-accent" : "translate-x-0.5 bg-text-secondary",
        )}
      />
    </button>
  );
}

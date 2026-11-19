import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { cn } from "@/lib/utils";

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Clipboard API needs a secure context; fall back to execCommand.
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  }
}

interface CopyButtonProps {
  value: string;
  label?: string;
  className?: string;
}

/** Small icon button that copies `value` and briefly shows a check. */
export function CopyButton({ value, label = "Copy", className }: CopyButtonProps) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(false), 1500);
    return () => window.clearTimeout(t);
  }, [copied]);

  return (
    <button
      type="button"
      onClick={async (e) => {
        e.stopPropagation();
        if (await copyText(value)) setCopied(true);
      }}
      aria-label={copied ? "Copied" : `${label}: ${value}`}
      title={copied ? "Copied" : label}
      className={cn(
        "inline-flex items-center justify-center size-5 rounded-sm text-text-muted hover:text-text-primary hover:bg-bg-overlay transition-colors cursor-pointer outline-none focus-visible:ring-1 focus-visible:ring-accent shrink-0",
        copied && "text-status-ok hover:text-status-ok",
        className,
      )}
    >
      {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
    </button>
  );
}

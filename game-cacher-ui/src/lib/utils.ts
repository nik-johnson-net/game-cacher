import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Pulls the authorization code out of either a raw code or pasted `{"authorizationCode": "..."}` JSON. */
export function extractAuthCode(input: string): string {
  const trimmed = input.trim();
  if (trimmed.startsWith("{")) {
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (typeof parsed === "object" && parsed !== null) {
        const code = (parsed as Record<string, unknown>).authorizationCode;
        if (typeof code === "string") return code.trim();
      }
    } catch {
      // not JSON: fall through to the raw value
    }
  }
  return trimmed.replace(/^"|"$/g, "");
}

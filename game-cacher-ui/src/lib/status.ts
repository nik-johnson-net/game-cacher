// Maps every app state to one of six status tones (see DESIGN.md "Status tones").

export type Tone = "ok" | "warn" | "info" | "active" | "error" | "neutral";

export type StatusSpec = { tone: Tone; label: string; pulse?: boolean };

export function enabledSpec(enabled: boolean): StatusSpec {
  return enabled ? { tone: "ok", label: "enabled" } : { tone: "neutral", label: "disabled" };
}

export function onOffSpec(on: boolean): StatusSpec {
  return on ? { tone: "ok", label: "on" } : { tone: "neutral", label: "off" };
}

export function runningSpec(running: boolean): StatusSpec {
  return running ? { tone: "active", label: "running", pulse: true } : { tone: "neutral", label: "idle" };
}

export function loginSpec(loggedIn: boolean): StatusSpec {
  return loggedIn ? { tone: "ok", label: "signed in" } : { tone: "warn", label: "signed out" };
}

export function updatedSpec(updated: boolean, error: string | null): StatusSpec {
  if (error) return { tone: "error", label: "failed" };
  return updated ? { tone: "active", label: "updated" } : { tone: "neutral", label: "up to date" };
}

/** Hit ratio tone: green when most traffic is served from cache. */
export function hitRatioTone(ratio: number | null): Tone {
  if (ratio === null) return "neutral";
  if (ratio >= 0.7) return "ok";
  if (ratio >= 0.3) return "warn";
  return "error";
}

// Maps every app state to one of six status tones (see DESIGN.md "Status tones").

import type { GameStatus, RunPhase, RunStatus } from "@/api/types";

export type Tone = "ok" | "warn" | "info" | "active" | "error" | "neutral";

export type StatusSpec = { tone: Tone; label: string; pulse?: boolean };

export function enabledSpec(enabled: boolean): StatusSpec {
  return enabled ? { tone: "ok", label: "enabled" } : { tone: "neutral", label: "disabled" };
}

export function onOffSpec(on: boolean): StatusSpec {
  return on ? { tone: "ok", label: "on" } : { tone: "neutral", label: "off" };
}

export function loginSpec(loggedIn: boolean): StatusSpec {
  return loggedIn ? { tone: "ok", label: "signed in" } : { tone: "warn", label: "signed out" };
}

export function updatedSpec(updated: boolean, error: string | null): StatusSpec {
  if (error) return { tone: "error", label: "failed" };
  return updated ? { tone: "active", label: "updated" } : { tone: "neutral", label: "up to date" };
}

export function runStatusSpec(status: RunStatus, phase: RunPhase | null = null): StatusSpec {
  switch (status) {
    case "running":
      return { tone: "active", label: phase ?? "running", pulse: true };
    case "ok":
      return { tone: "ok", label: "completed" };
    case "error":
      return { tone: "error", label: "completed with errors" };
    case "cancelled":
      return { tone: "neutral", label: "cancelled" };
    case "incomplete":
      return { tone: "warn", label: "incomplete" };
  }
}

/** A game within a run; finished games show whether they changed. */
export function gameStatusSpec(status: GameStatus, updated: boolean, error: string | null): StatusSpec {
  switch (status) {
    case "pending":
      return { tone: "neutral", label: "pending" };
    case "checking":
      return { tone: "active", label: "checking", pulse: true };
    case "queued":
      return { tone: "info", label: "queued" };
    case "downloading":
      return { tone: "active", label: "downloading", pulse: true };
    case "cancelled":
      // Stopped before finishing: the run was cancelled or interrupted.
      return { tone: "neutral", label: "stopped" };
    case "done":
    case "failed":
      return updatedSpec(updated, error);
  }
}

/** Hit ratio tone: green when most traffic is served from cache. */
export function hitRatioTone(ratio: number | null): Tone {
  if (ratio === null) return "neutral";
  if (ratio >= 0.7) return "ok";
  if (ratio >= 0.3) return "warn";
  return "error";
}

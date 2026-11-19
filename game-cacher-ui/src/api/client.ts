// Small typed fetch client for the game-cacher REST API. One function per
// endpoint in docs/ARCHITECTURE.md "REST API (admin listener)".

import type {
  AppStatus,
  AuthStateResponse,
  ErrorBody,
  Game,
  LoginRequest,
  LoginResponse,
  ManagedStatus,
  PlatformSummary,
  PurgeResult,
  Settings,
} from "./types";

/** Thrown for every non-2xx response (and for network failures, with status 0). */
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }

  get isConflict(): boolean {
    return this.status === 409;
  }
}

export function isAbortError(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

/** Human-readable message for anything caught from the client. */
export function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  return String(e);
}

type RequestOptions = {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  body?: unknown;
  signal?: AbortSignal;
  /** Do not redirect to /login on 401 (used by the auth calls themselves). */
  noAuthRedirect?: boolean;
};

function isErrorBody(value: unknown): value is ErrorBody {
  return (
    typeof value === "object" &&
    value !== null &&
    "error" in value &&
    typeof (value as { error: unknown }).error === "string"
  );
}

async function errorFromResponse(res: Response): Promise<ApiError> {
  let message = "";
  try {
    const raw = await res.text();
    if (raw) {
      try {
        const parsed: unknown = JSON.parse(raw);
        message = isErrorBody(parsed) ? parsed.error : raw;
      } catch {
        message = raw;
      }
    }
  } catch {
    // ignore unreadable bodies
  }
  if (!message) message = `${res.status} ${res.statusText || "request failed"}`.trim();
  return new ApiError(res.status, message);
}

function redirectToLogin() {
  if (window.location.pathname !== "/login") window.location.assign("/login");
}

async function send(path: string, opts: RequestOptions): Promise<Response> {
  const headers: Record<string, string> = { Accept: "application/json" };
  let body: string | undefined;
  if (opts.body !== undefined) {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(opts.body);
  }
  let res: Response;
  try {
    res = await fetch(path, { method: opts.method ?? "GET", headers, body, signal: opts.signal });
  } catch (e) {
    if (isAbortError(e)) throw e;
    throw new ApiError(0, `Network error: ${errorMessage(e)}`);
  }
  if (res.status === 401 && !opts.noAuthRedirect) redirectToLogin();
  if (!res.ok) throw await errorFromResponse(res);
  return res;
}

/** JSON response. */
async function json<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const res = await send(path, opts);
  const raw = await res.text();
  if (!raw) throw new ApiError(res.status, `Empty response from ${path}`);
  try {
    return JSON.parse(raw) as T;
  } catch {
    throw new ApiError(res.status, `Invalid JSON from ${path}`);
  }
}

/** 204 / body ignored. */
async function empty(path: string, opts: RequestOptions = {}): Promise<void> {
  const res = await send(path, opts);
  if (res.status !== 204) await res.text().catch(() => "");
}

const seg = encodeURIComponent;

// ── Auth ───────────────────────────────────────────────────────────────

export function authStatus(signal?: AbortSignal): Promise<AuthStateResponse> {
  return json<AuthStateResponse>("/api/auth/status", { signal, noAuthRedirect: true });
}

export function login(password: string): Promise<void> {
  return empty("/api/auth/login", { method: "POST", body: { password }, noAuthRedirect: true });
}

export function logout(): Promise<void> {
  return empty("/api/auth/logout", { method: "POST", noAuthRedirect: true });
}

// ── Status / settings ──────────────────────────────────────────────────

export function getStatus(signal?: AbortSignal): Promise<AppStatus> {
  return json<AppStatus>("/api/status", { signal });
}

export function getSettings(signal?: AbortSignal): Promise<Settings> {
  return json<Settings>("/api/settings", { signal });
}

export function putSettings(settings: Settings): Promise<Settings> {
  return json<Settings>("/api/settings", { method: "PUT", body: settings });
}

// ── Platforms ──────────────────────────────────────────────────────────

export function listPlatforms(signal?: AbortSignal): Promise<PlatformSummary[]> {
  return json<PlatformSummary[]>("/api/platforms", { signal });
}

export function getPlatform(id: string, signal?: AbortSignal): Promise<PlatformSummary> {
  return json<PlatformSummary>(`/api/platforms/${seg(id)}`, { signal });
}

export function setPlatformEnabled(id: string, enabled: boolean): Promise<PlatformSummary> {
  return json<PlatformSummary>(`/api/platforms/${seg(id)}`, { method: "PUT", body: { enabled } });
}

export function purgePlatformCache(id: string): Promise<PurgeResult> {
  return json<PurgeResult>(`/api/platforms/${seg(id)}/cache`, { method: "DELETE" });
}

// ── Managed updates ────────────────────────────────────────────────────

export function getManaged(id: string, signal?: AbortSignal): Promise<ManagedStatus> {
  return json<ManagedStatus>(`/api/platforms/${seg(id)}/managed`, { signal });
}

export function managedLogin(id: string, req: LoginRequest): Promise<LoginResponse> {
  return json<LoginResponse>(`/api/platforms/${seg(id)}/managed/login`, { method: "POST", body: req });
}

export function managedLogout(id: string): Promise<void> {
  return empty(`/api/platforms/${seg(id)}/managed/logout`, { method: "POST" });
}

export function listManagedGames(id: string, signal?: AbortSignal): Promise<Game[]> {
  return json<Game[]>(`/api/platforms/${seg(id)}/managed/games`, { signal });
}

export function saveSelection(id: string, selected: string[]): Promise<ManagedStatus> {
  return json<ManagedStatus>(`/api/platforms/${seg(id)}/managed/selection`, { method: "PUT", body: { selected } });
}

export function runManaged(id: string): Promise<{ started: boolean }> {
  return json<{ started: boolean }>(`/api/platforms/${seg(id)}/managed/run`, { method: "POST" });
}

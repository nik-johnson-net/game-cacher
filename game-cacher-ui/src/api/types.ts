// Types copied from docs/ARCHITECTURE.md "REST API (admin listener)". Keep in sync; never change the contract from here.

export type ErrorBody = { error: string };

export type AuthStateResponse = { authenticated: boolean; auth_enabled: boolean };

export type StorageStatus = {
  backend: string;
  used_bytes: number;
  max_bytes: number | null;
  objects: number;
};

export type AppStatus = {
  version: string;
  uptime_secs: number;
  storage: StorageStatus;
  secrets_backend: string;
  dns: { enabled: boolean; cache_ips: string[]; forwarders: string[] };
  sni_enabled: boolean;
};

export type PlatformStats = {
  requests: number;
  hits: number;
  misses: number;
  bypass: number;
  bytes_from_cache: number;
  bytes_from_upstream: number;
  storage_bytes: number;
  objects: number;
};

export type PlatformSummary = {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  domains: string[];
  managed: boolean;
  stats: PlatformStats;
};

export type PurgeResult = { deleted_objects: number; freed_bytes: number };

export type LoginMethod = "none" | "qr" | "password" | "auth_code" | "manual_list";

export type AuthStatus = {
  logged_in: boolean;
  account: string | null;
  required: boolean;
  /** auth_code platforms: where to obtain the code (hint carried by ManagedStatus). */
  auth_url?: string | null;
};

export type UpdateReport = {
  game_id: string;
  name: string;
  previous_version: string | null;
  current_version: string | null;
  updated: boolean;
  bytes_downloaded: number;
  bytes_already_cached: number;
  files: number;
  error: string | null;
};

export type UpdateRun = {
  started: string;
  finished: string | null;
  games: UpdateReport[];
};

export type ManagedStatus = {
  auth: AuthStatus;
  login_methods: LoginMethod[];
  selected: string[];
  running: boolean;
  last_run: UpdateRun | null;
  next_run: string | null;
};

export type LoginRequest =
  | { method: "qr" }
  | { method: "qr_poll"; challenge_id: string }
  | { method: "password"; username: string; password: string }
  | { method: "guard"; challenge_id: string; code: string }
  | { method: "auth_code"; code: string };

export type GuardType = "email" | "device" | "device_confirmation";

export type LoginResponse =
  | { status: "logged_in"; account: string }
  | { status: "qr"; challenge_id: string; url: string }
  | { status: "pending"; challenge_id: string }
  | { status: "guard_required"; challenge_id: string; guard_type: GuardType }
  | { status: "failed"; message: string };

export type Game = {
  id: string;
  name: string;
  version: string | null;
  size_bytes: number | null;
  selected: boolean;
};

export type Settings = { update_interval_minutes: number };

// Dev-only mock of the game-cacher REST API, mounted as Vite dev-server
// middleware by `npm run dev:mock` (see vite.config.ts). In-memory and
// stateful: stats grow, platforms toggle, QR / guard / auth-code sign-in flows
// work, update runs go through listing → checking → downloading with live
// progress and can be cancelled. Never part of the production bundle.
//
// Set MOCK_AUTH=1 to require the admin password "admin" (exercises /login).

import type { IncomingMessage, ServerResponse } from "node:http";
import type { Connect, Plugin } from "vite";
import type {
  AppStatus,
  Game,
  LoginMethod,
  LoginRequest,
  LoginResponse,
  GameRun,
  ManagedStatus,
  PlatformSummary,
  UpdateRun,
  UpdateSchedule,
} from "../src/api/types";
import { MIN_GAP_MINUTES, TIME_RE, normalizeTimes, tooClose } from "../src/lib/schedule";

const GB = 1e9;
const MB = 1e6;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

class HttpError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

// ── State ──────────────────────────────────────────────────────────────

const AUTH_ENABLED = process.env.MOCK_AUTH === "1";
const PASSWORD = "admin";
const COOKIE = "gc_session=mock";
let updateSchedule: UpdateSchedule = { timezone: "UTC", times: ["02:00", "16:00"] };
const startedAt = Date.now() - 5 * 3600_000;

type Managed = {
  methods: LoginMethod[];
  required: boolean;
  authUrl?: string;
  loggedIn: boolean;
  account: string | null;
  selected: string[];
  active: ActiveRun | null;
  /** Finished runs, newest first. */
  history: UpdateRun[];
  nextRun: number | null;
  games: Omit<Game, "selected">[];
};

type Platform = PlatformSummary & { managedState?: Managed };

/** What a simulated run will do per game, fixed when it starts. */
type GamePlanSim = {
  id: string;
  name: string;
  previous: string | null;
  current: string | null;
  failed: boolean;
  toDownload: number;
  cached: number;
  objects: number;
  downloadMs: number;
};

type ActiveRun = { id: string; startedAt: number; cancelledAt: number | null; plans: GamePlanSim[] };

function games(prefix: string, names: [string, number][]): Omit<Game, "selected">[] {
  return names.map(([name, size], i) => ({
    id: `${prefix}${1000 + i * 17}`,
    name,
    version: i % 4 === 3 ? null : `build-${5000 + i * 31}`,
    size_bytes: i % 5 === 4 ? null : size,
  }));
}

function platform(
  id: string,
  name: string,
  description: string,
  domains: string[],
  stats: Partial<PlatformSummary["stats"]>,
  managedState?: Managed,
): Platform {
  return {
    id,
    name,
    description,
    enabled: true,
    domains,
    managed: managedState !== undefined,
    stats: {
      requests: 0,
      hits: 0,
      misses: 0,
      bypass: 0,
      bytes_from_cache: 0,
      bytes_from_upstream: 0,
      storage_bytes: 0,
      objects: 0,
      ...stats,
    },
    managedState,
  };
}

function managed(methods: LoginMethod[], over: Partial<Managed> & { gameList?: Managed["games"] } = {}): Managed {
  return {
    methods,
    required: !methods.includes("none") && !methods.includes("manual_list"),
    loggedIn: false,
    account: null,
    selected: [],
    active: null,
    history: [],
    nextRun: Date.now() + 40 * 60_000,
    games: over.gameList ?? [],
    ...over,
  };
}

const platforms: Platform[] = [
  platform(
    "steam",
    "Steam",
    "Valve's content delivery network (steamcontent.com and friends).",
    ["lancache.steamcontent.com", "cdn.cloudflare.steamstatic.com", "steamcontent.com", "content1.steampowered.com"],
    { requests: 18432, hits: 15210, misses: 3100, bypass: 122, bytes_from_cache: 912 * GB, bytes_from_upstream: 143 * GB, storage_bytes: 143 * GB, objects: 412_000 },
    managed(["qr", "password"], {
      gameList: games("app", [
        ["Counter-Strike 2", 33 * GB],
        ["Dota 2", 41 * GB],
        ["Team Fortress 2", 24 * GB],
        ["Portal 2", 12 * GB],
        ["Half-Life: Alyx", 66 * GB],
        ["Apex Legends", 56 * GB],
        ["Rust", 18 * GB],
        ["Left 4 Dead 2", 13 * GB],
      ]),
    }),
  ),
  platform(
    "epic",
    "Epic Games",
    "Epic Games Launcher and Fortnite downloads.",
    ["epicgames-download1.akamaized.net", "download.epicgames.com", "fastly-download.epicgames.com"],
    { requests: 2210, hits: 1500, misses: 690, bypass: 20, bytes_from_cache: 210 * GB, bytes_from_upstream: 88 * GB, storage_bytes: 88 * GB, objects: 90_113 },
    managed(["auth_code"], {
      authUrl: "https://www.epicgames.com/id/login?redirectUrl=https%3A%2F%2Fwww.epicgames.com%2Fid%2Fapi%2Fredirect",
      gameList: games("ep", [
        ["Fortnite", 31 * GB],
        ["Rocket League", 20 * GB],
        ["Alan Wake 2", 90 * GB],
        ["Control", 42 * GB],
      ]),
    }),
  ),
  platform(
    "blizzard",
    "Blizzard",
    "Battle.net downloads (Overwatch, Diablo, WoW).",
    ["level3.blizzard.com", "blzddist1-a.akamaihd.net", "us.cdn.blizzard.com"],
    { requests: 640, hits: 200, misses: 430, bypass: 10, bytes_from_cache: 45 * GB, bytes_from_upstream: 130 * GB, storage_bytes: 130 * GB, objects: 30_400 },
    managed(["none"], {
      gameList: games("bn", [
        ["World of Warcraft", 98 * GB],
        ["Diablo IV", 90 * GB],
        ["Overwatch 2", 55 * GB],
      ]),
    }),
  ),
  platform(
    "origin",
    "Origin / EA",
    "EA app and Origin downloads.",
    ["origin-a.akamaihd.net", "lvlt.cdn.ea.com"],
    { requests: 90, hits: 10, misses: 80, bypass: 0, bytes_from_cache: 2 * GB, bytes_from_upstream: 40 * GB, storage_bytes: 40 * GB, objects: 7_900 },
  ),
  platform(
    "windowsupdate",
    "Windows Update",
    "Microsoft update and Delivery Optimization downloads.",
    ["download.windowsupdate.com", "tlu.dl.delivery.mp.microsoft.com", "dl.delivery.mp.microsoft.com"],
    { requests: 5120, hits: 4800, misses: 300, bypass: 20, bytes_from_cache: 330 * GB, bytes_from_upstream: 24 * GB, storage_bytes: 24 * GB, objects: 12_000 },
    managed(["manual_list"], {
      required: false,
      selected: ["http://download.windowsupdate.com/c/msdownload/update/software/secu/2026/10/kb5044273.cab"],
    }),
  ),
];
platforms[4].enabled = false;

/** Past runs for the history page: two finished Blizzard runs and an interrupted one. */
function seedHistory() {
  const { m } = findManaged("blizzard");
  const past = (hoursAgo: number, games: string[], incomplete = false): UpdateRun => {
    const a = startRun({ ...m, selected: games });
    a.startedAt = Date.now() - hoursAgo * 3600_000;
    const run = simulate("blizzard", a, a.startedAt + (incomplete ? 4000 : 3600_000));
    if (!incomplete) return run;
    const stopped = run.games.map((g): GameRun =>
      g.status === "done" || g.status === "failed" ? g : { ...g, status: "cancelled", rate_bps: null, eta_secs: null },
    );
    return { ...run, games: stopped, status: "incomplete", phase: null, rate_bps: null, eta_secs: null };
  };
  const ids = m.games.map((g) => g.id);
  m.history = [past(3, ids.slice(0, 2), true), past(14, ids.slice(0, 5)), past(38, ids.slice(0, 2))];
}

/** qr/device challenges: poll count until they succeed. */
const challenges = new Map<string, { platform: string; polls: number; kind: "qr" | "device"; account: string }>();
let challengeSeq = 0;

// ── Helpers ────────────────────────────────────────────────────────────

function find(id: string): Platform {
  const p = platforms.find((x) => x.id === id);
  if (!p) throw new HttpError(404, `unknown platform ${id}`);
  return p;
}

function findManaged(id: string): { p: Platform; m: Managed } {
  const p = find(id);
  if (!p.managedState) throw new HttpError(404, `platform ${id} has no managed updates`);
  return { p, m: p.managedState };
}

function summary(p: Platform): PlatformSummary {
  const { managedState: _ignored, ...rest } = p;
  void _ignored;
  return rest;
}

/** Validates a PUT /api/settings body like the server: known timezone, HH:MM times 5+ minutes apart, sorted and unique. */
function parseSchedule(body: unknown): UpdateSchedule {
  const sched = isRecord(body) ? body.update_schedule : undefined;
  if (!isRecord(sched) || typeof sched.timezone !== "string" || !Array.isArray(sched.times)) {
    throw new HttpError(400, "update_schedule must have a timezone and times");
  }
  const { timezone, times } = sched;
  try {
    // Accepts legacy aliases (Asia/Calcutta) like the server, unlike Intl.supportedValuesOf.
    new Intl.DateTimeFormat("en", { timeZone: timezone });
  } catch {
    throw new HttpError(400, `unknown timezone "${timezone}"`);
  }
  for (const t of times) {
    if (typeof t !== "string" || !TIME_RE.test(t)) throw new HttpError(400, `invalid time ${JSON.stringify(t)}, expected HH:MM`);
  }
  const normalized = normalizeTimes(times as string[]);
  const close = tooClose(normalized);
  if (close) throw new HttpError(400, `times must be at least ${MIN_GAP_MINUTES} minutes apart (${close.join(" and ")})`);
  return { timezone, times: normalized };
}

/** Next scheduled run. Approximation: the mock reads the times as UTC whatever the timezone. */
function nextTrigger(): number | null {
  const now = new Date();
  const candidates = [0, 1].flatMap((day) =>
    updateSchedule.times.map((t) => {
      const [h, m] = t.split(":").map(Number);
      return Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() + day, h, m);
    }),
  );
  const future = candidates.filter((c) => c > now.getTime());
  return future.length ? Math.min(...future) : null;
}

// ── Simulated update runs ──────────────────────────────────────────────

const LIST_MS = 600;
const CHECK_MS = 900;
/** Simulated download speed. */
const RATE = 400 * MB;
const KEEP_RUNS = 10;
let runSeq = 0;

function startRun(m: Managed): ActiveRun {
  const plans = m.selected.map((sel, i): GamePlanSim => {
    const g = m.games.find((x) => x.id === sel);
    const size = g?.size_bytes ?? 2 * GB;
    const updated = i % 2 === 0;
    const toDownload = updated ? Math.round(size * 0.12) : 0;
    return {
      id: sel,
      name: g?.name ?? sel.split("/").pop() ?? sel,
      previous: g?.version ?? null,
      current: updated ? `build-${6000 + i}` : (g?.version ?? null),
      failed: i === 3,
      toDownload,
      cached: size - toDownload,
      objects: Math.max(1, Math.round(size / MB)),
      downloadMs: Math.min(20_000, Math.max(1500, (toDownload / RATE) * 1000)),
    };
  });
  return { id: `mock-${++runSeq}`, startedAt: Date.now(), cancelledAt: null, plans };
}

function gameRun(plan: GamePlanSim, over: Partial<GameRun>): GameRun {
  return {
    game_id: plan.id,
    name: plan.name,
    previous_version: null,
    current_version: null,
    updated: false,
    bytes_downloaded: 0,
    bytes_already_cached: 0,
    files: 0,
    error: null,
    status: "pending",
    bytes_to_download: null,
    objects: 0,
    objects_done: 0,
    rate_bps: null,
    eta_secs: null,
    ...over,
  };
}

/** The run's record at `now`, derived from its plans (deterministic). */
function simulate(platformId: string, a: ActiveRun, now: number): UpdateRun {
  const t = Math.min(now, a.cancelledAt ?? Infinity) - a.startedAt;
  const run: UpdateRun = {
    id: a.id,
    platform: platformId,
    started: new Date(a.startedAt).toISOString(),
    finished: null,
    status: "running",
    phase: "listing",
    bytes_to_download: 0,
    bytes_downloaded: 0,
    rate_bps: null,
    eta_secs: null,
    games: [],
  };
  if (t < LIST_MS) return finishIfCancelled(run, a);
  run.phase = "checking";
  let clock = LIST_MS;
  const checked = (p: GamePlanSim) =>
    gameRun(p, {
      status: "queued",
      previous_version: p.previous,
      current_version: p.current,
      bytes_to_download: p.toDownload,
      objects: p.objects,
    });
  run.games = a.plans.map((p) => {
    const g = t < clock ? gameRun(p, {}) : t < clock + CHECK_MS ? gameRun(p, { status: "checking" }) : checked(p);
    clock += CHECK_MS;
    return g;
  });
  if (t >= clock) {
    run.phase = "downloading";
    run.games = a.plans.map((p, i) => {
      const [start, end] = [clock, clock + p.downloadMs];
      clock = end;
      if (t < start) return run.games[i];
      if (t >= end) return finishedGame(p);
      const frac = (t - start) / p.downloadMs;
      const rate = Math.round(p.toDownload / (p.downloadMs / 1000));
      return gameRun(p, {
        ...checked(p),
        status: "downloading",
        bytes_downloaded: Math.round(p.toDownload * frac),
        bytes_already_cached: Math.round(p.cached * frac),
        objects_done: Math.round(p.objects * frac),
        rate_bps: rate,
        eta_secs: Math.ceil((end - t) / 1000),
      });
    });
    if (t >= clock) {
      run.status = run.games.some((g) => g.status === "failed") ? "error" : "ok";
      run.phase = null;
      run.finished = new Date(a.startedAt + clock).toISOString();
    }
  }
  run.bytes_to_download = run.games.reduce((s, g) => s + (g.bytes_to_download ?? 0), 0);
  run.bytes_downloaded = run.games.reduce((s, g) => s + g.bytes_downloaded, 0);
  const current = run.games.find((g) => g.status === "downloading");
  if (current) {
    run.rate_bps = current.rate_bps;
    const remaining = run.games
      .filter((g) => g.status === "queued" || g.status === "downloading")
      .reduce((s, g) => s + Math.max(0, (g.bytes_to_download ?? 0) - g.bytes_downloaded), 0);
    run.eta_secs = current.rate_bps ? Math.ceil(remaining / current.rate_bps) : null;
  }
  return finishIfCancelled(run, a);
}

function finishedGame(p: GamePlanSim): GameRun {
  return gameRun(p, {
    status: p.failed ? "failed" : "done",
    previous_version: p.previous,
    current_version: p.current,
    updated: p.toDownload > 0 || p.previous !== p.current,
    bytes_downloaded: p.failed ? Math.round(p.toDownload / 2) : p.toDownload,
    bytes_already_cached: p.cached,
    files: p.failed ? p.objects - 3 : p.objects,
    error: p.failed ? "depot 1002: 3 of 2100 objects failed: upstream timed out (504)" : null,
    bytes_to_download: p.toDownload,
    objects: p.objects,
    objects_done: p.objects,
  });
}

function finishIfCancelled(run: UpdateRun, a: ActiveRun): UpdateRun {
  if (a.cancelledAt === null || run.status !== "running") return run;
  run.games = run.games.map((g) =>
    g.status === "done" || g.status === "failed" ? g : { ...g, status: "cancelled", rate_bps: null, eta_secs: null },
  );
  if (run.games.length === 0) run.games = a.plans.map((p) => gameRun(p, { status: "cancelled" }));
  return { ...run, status: "cancelled", phase: null, rate_bps: null, eta_secs: null, finished: new Date(a.cancelledAt).toISOString() };
}

/** Moves a run that has finished into the history. */
function settleRun(m: Managed, p: Platform) {
  if (!m.active) return;
  const run = simulate(p.id, m.active, Date.now());
  if (run.status === "running") return;
  m.active = null;
  m.history = [run, ...m.history].slice(0, KEEP_RUNS);
  m.nextRun = nextTrigger();
  p.stats.storage_bytes += run.bytes_downloaded;
}

function runs(p: Platform, m: Managed): UpdateRun[] {
  settleRun(m, p);
  return m.active ? [simulate(p.id, m.active, Date.now()), ...m.history] : m.history;
}

function managedStatus(p: Platform, m: Managed): ManagedStatus {
  settleRun(m, p);
  return {
    auth: { logged_in: m.loggedIn, account: m.account, required: m.required, auth_url: m.authUrl ?? null },
    login_methods: m.methods,
    selected: m.selected,
    running: m.active !== null,
    last_run: runs(p, m)[0] ?? null,
    next_run: m.nextRun === null ? null : new Date(m.nextRun).toISOString(),
  };
}

function loggedIn(m: Managed, account: string): LoginResponse {
  m.loggedIn = true;
  m.account = account;
  // Games the user already had selected stay selected; seed a couple on first login.
  if (m.selected.length === 0) m.selected = m.games.slice(0, 2).map((g) => g.id);
  return { status: "logged_in", account };
}

function login(platformId: string, m: Managed, req: LoginRequest): LoginResponse {
  switch (req.method) {
    case "qr": {
      const id = `qr-${++challengeSeq}`;
      challenges.set(id, { platform: platformId, polls: 0, kind: "qr", account: "mock_qr_user" });
      return { status: "qr", challenge_id: id, url: `https://s.team/q/1/${id}?mock=1` };
    }
    case "qr_poll": {
      const c = challenges.get(req.challenge_id);
      if (!c || c.platform !== platformId) return { status: "failed", message: "challenge expired" };
      c.polls += 1;
      if (c.polls < 3) return { status: "pending", challenge_id: req.challenge_id };
      challenges.delete(req.challenge_id);
      return loggedIn(m, c.account);
    }
    case "password": {
      if (!req.username || req.password === "wrong") return { status: "failed", message: "invalid username or password" };
      const id = `pw-${++challengeSeq}`;
      // "phone…" → device confirmation, "totp…" → authenticator code, anything else → emailed code ("slow…" additionally answers the guard submit with pending).
      if (req.username.startsWith("totp")) {
        challenges.set(id, { platform: platformId, polls: 0, kind: "qr", account: req.username });
        return { status: "guard_required", challenge_id: id, guard_type: "device" };
      }
      if (req.username.startsWith("phone")) {
        challenges.set(id, { platform: platformId, polls: 0, kind: "device", account: req.username });
        return { status: "guard_required", challenge_id: id, guard_type: "device_confirmation" };
      }
      challenges.set(id, { platform: platformId, polls: 0, kind: "qr", account: req.username });
      return { status: "guard_required", challenge_id: id, guard_type: "email" };
    }
    case "guard": {
      const c = challenges.get(req.challenge_id);
      if (!c) return { status: "failed", message: "challenge expired" };
      if (req.code !== "12345") return { status: "failed", message: "wrong guard code (the mock accepts 12345)" };
      // "slow…" → Steam hasn't finished yet: answer pending, then succeed via qr_poll.
      if (c.account.startsWith("slow")) {
        c.polls = 0;
        return { status: "pending", challenge_id: req.challenge_id };
      }
      challenges.delete(req.challenge_id);
      return loggedIn(m, c.account);
    }
    case "auth_code": {
      if (!req.code || req.code === "bad") return { status: "failed", message: "authorization code rejected" };
      return loggedIn(m, "mock_epic_user");
    }
  }
}

// ── Routing ────────────────────────────────────────────────────────────

type Json = unknown;
type Handled = { status?: number; body?: Json };

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

async function route(method: string, path: string, body: unknown): Promise<Handled> {
  const parts = path.split("/").filter(Boolean).map(decodeURIComponent); // ["api", ...]

  if (path === "/api/auth/status") {
    return { body: { authenticated: authed, auth_enabled: AUTH_ENABLED } };
  }
  if (path === "/api/auth/login" && method === "POST") {
    if (!isRecord(body) || body.password !== PASSWORD) throw new HttpError(401, "invalid password");
    authed = true;
    return { status: 204 };
  }
  if (path === "/api/auth/logout" && method === "POST") {
    authed = false;
    return { status: 204 };
  }
  if (AUTH_ENABLED && !authed) throw new HttpError(401, "authentication required");

  await sleep(80);

  if (path === "/api/status") {
    const used = platforms.reduce((a, p) => a + p.stats.storage_bytes, 0);
    const status: AppStatus = {
      version: "0.1.0-mock",
      uptime_secs: Math.round((Date.now() - startedAt) / 1000),
      storage: { backend: "disk", used_bytes: used, max_bytes: 1000 * GB, objects: platforms.reduce((a, p) => a + p.stats.objects, 0) },
      secrets_backend: "storage",
      dns: { enabled: true, cache_ips: ["192.168.1.10"], forwarders: ["1.1.1.1", "9.9.9.9"] },
      sni_enabled: true,
    };
    return { body: status };
  }
  if (path === "/api/settings") {
    if (method === "PUT") updateSchedule = parseSchedule(body);
    return { body: { update_schedule: updateSchedule } };
  }
  if (path === "/api/runs") {
    const all = platforms.flatMap((p) => (p.managedState ? runs(p, p.managedState) : []));
    return { body: all.sort((a, b) => Date.parse(b.started) - Date.parse(a.started)) };
  }
  if (path === "/api/platforms") {
    for (const p of platforms) if (p.enabled) tick(p);
    return { body: platforms.map(summary) };
  }

  if (parts[0] === "api" && parts[1] === "platforms" && parts[2]) {
    const p = find(parts[2]);
    const rest = parts.slice(3).join("/");
    if (rest === "") {
      if (method === "PUT") {
        if (!isRecord(body) || typeof body.enabled !== "boolean") throw new HttpError(400, "expected {\"enabled\": bool}");
        p.enabled = body.enabled;
      } else if (p.enabled) tick(p);
      return { body: summary(p) };
    }
    if (rest === "cache" && method === "DELETE") {
      const res = { deleted_objects: p.stats.objects, freed_bytes: p.stats.storage_bytes };
      p.stats.objects = 0;
      p.stats.storage_bytes = 0;
      return { body: res };
    }
    return managedRoute(method, rest, p, body);
  }
  throw new HttpError(404, `no route for ${method} ${path}`);
}

function managedRoute(method: string, rest: string, p: Platform, body: unknown): Handled {
  const { m } = findManaged(p.id);
  switch (`${method} ${rest}`) {
    case "GET managed":
      return { body: managedStatus(p, m) };
    case "POST managed/login": {
      if (!isRecord(body) || typeof body.method !== "string") throw new HttpError(400, "missing login method");
      return { body: login(p.id, m, body as LoginRequest) };
    }
    case "POST managed/logout":
      m.loggedIn = false;
      m.account = null;
      return { status: 204 };
    case "GET managed/games": {
      if (m.required && !m.loggedIn) throw new HttpError(403, "sign in first");
      const out: Game[] = m.games.map((g) => ({ ...g, selected: m.selected.includes(g.id) }));
      return { body: out };
    }
    case "PUT managed/selection": {
      const sel = isRecord(body) ? body.selected : undefined;
      if (!Array.isArray(sel) || !sel.every((x) => typeof x === "string")) throw new HttpError(400, "expected {\"selected\": [string]}");
      m.selected = sel;
      return { body: managedStatus(p, m) };
    }
    case "POST managed/run": {
      settleRun(m, p);
      if (m.active) throw new HttpError(409, "an update run is already in progress");
      if (!p.enabled) throw new HttpError(409, `${p.id} is disabled`);
      m.active = startRun(m);
      return { status: 202, body: { started: true } };
    }
    case "POST managed/run/cancel": {
      settleRun(m, p);
      if (!m.active || m.active.cancelledAt !== null) throw new HttpError(409, "no update run is in progress");
      m.active.cancelledAt = Date.now();
      return { status: 202, body: { cancelling: true } };
    }
    default:
      throw new HttpError(404, `no route for ${method} /api/platforms/${p.id}/${rest}`);
  }
}

/** Traffic keeps flowing on enabled platforms. */
function tick(p: Platform) {
  const reqs = Math.floor(Math.random() * 6);
  const hit = Math.floor(reqs * 0.8);
  p.stats.requests += reqs;
  p.stats.hits += hit;
  p.stats.misses += reqs - hit;
  p.stats.bytes_from_cache += hit * 80 * MB;
  p.stats.bytes_from_upstream += (reqs - hit) * 80 * MB;
  p.stats.storage_bytes += (reqs - hit) * 80 * MB;
  p.stats.objects += (reqs - hit) * 3;
}

let authed = !AUTH_ENABLED;

function readBody(req: IncomingMessage): Promise<unknown> {
  return new Promise((resolve, reject) => {
    let raw = "";
    req.on("data", (c: Buffer) => (raw += c.toString()));
    req.on("end", () => {
      if (!raw) return resolve(undefined);
      try {
        resolve(JSON.parse(raw));
      } catch {
        reject(new HttpError(400, "invalid JSON body"));
      }
    });
    req.on("error", reject);
  });
}

function send(res: ServerResponse, status: number, body?: Json) {
  if (body === undefined) {
    res.statusCode = status;
    res.end();
    return;
  }
  res.statusCode = status;
  res.setHeader("Content-Type", "application/json");
  res.end(JSON.stringify(body));
}

export function mockApi(): Plugin {
  const handler: Connect.NextHandleFunction = (req, res, next) => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (url.pathname === "/healthz") return send(res, 200, undefined);
    if (url.pathname === "/metrics") {
      res.setHeader("Content-Type", "application/openmetrics-text");
      res.end("# TYPE gamecacher_requests counter\ngamecacher_requests_total{platform=\"steam\",result=\"hit\"} 15210\n# EOF\n");
      return;
    }
    if (!url.pathname.startsWith("/api/")) return next();
    void (async () => {
      try {
        const body = await readBody(req);
        const out = await route(req.method ?? "GET", url.pathname, body);
        if (url.pathname === "/api/auth/login") res.setHeader("Set-Cookie", `${COOKIE}; Path=/`);
        send(res, out.status ?? 200, out.body);
      } catch (e) {
        if (e instanceof HttpError) send(res, e.status, { error: e.message });
        else send(res, 500, { error: e instanceof Error ? e.message : String(e) });
      }
    })();
  };
  return {
    name: "game-cacher-mock-api",
    configureServer(server) {
      server.middlewares.use(handler);
    },
  };
}

// Last: seeding uses the run simulation's constants declared above.
seedHistory();

# Game Cacher — Architecture & API Contract

This is the authoritative architecture and REST contract. Update it in the
same change as any JSON shape, storage-layout or config change.

## Decisions (from requirements clarification, 2026-11-19)

- Managed updates: **Steam, Epic, Blizzard** fully. Windows Update: prefill a
  user-entered list of URLs only. Origin/EA: passive caching only.
- Steam login: QR code **and** username/password + Steam Guard, in the web UI.
  Refresh token kept in the secrets store.
- Admin UI/API protected by a single admin password (`GC_ADMIN_PASSWORD`);
  session cookie for the UI, `Authorization: Bearer <password>` for scripts.
  `/metrics` and `/healthz` stay open.
- Eviction: max size + LRU, tracked in an index file in storage.
- No database: settings file + per-platform state files + storage layout.

## Processes / listeners (single binary `game-cacher`)

| Listener | Default | Purpose |
|---|---|---|
| Cache HTTP | `0.0.0.0:80` | Transparent caching proxy for redirected CDN hosts (`cache/`) |
| SNI | `0.0.0.0:443` | TLS passthrough by SNI to real upstream (`sni.rs`) |
| DNS | `0.0.0.0:53` udp+tcp | Override platform domains → cache IP; forward the rest (`dns.rs`) |
| Admin | `0.0.0.0:8080` | Web UI, `/api/*`, `/metrics`, `/healthz` (`api/`) |

## Crate layout

```
Cargo.toml                 workspace (members: game-cacher)
game-cacher/src/
  main.rs                  thin: parse Config, init logging, app::run
  lib.rs                   module decls
  config.rs                clap Config (all flags have env vars)
  app.rs                   builds AppState, spawns listeners, shutdown
  state.rs                 AppState (Arc'd shared handles)
  error.rs                 ApiError / ApiResult (JSON {"error": ".."})
  storage/{mod,disk,s3,memory}.rs     Storage trait + impls
  secrets/{mod,storage,vault}.rs      SecretStore trait + impls
  settings.rs              SettingsStore: settings.json + state/<platform>.json
  upstream.rs              Upstream DNS resolver (hickory) + reqwest client factory
  cache/{mod,key,engine,proxy,index}.rs  caching engine, HTTP proxy handler, LRU index/eviction
  dns.rs                   DNS server
  sni.rs                   SNI passthrough
  metrics.rs               prometheus-client registry + typed metric families
  platforms/{mod,domains,steam/,epic/,blizzard/,origin,windows_update}.rs
  updates.rs               managed-update scheduler + job runner
  api/{mod,auth,platforms,managed,stats,settings,cache}.rs
game-cacher/tests/         integration tests (real listeners + reqwest)
game-cacher-ui/            React 19 + Vite + Tailwind 4
docker/Dockerfile          multi-stage UBI10 image
```

## Configuration (clap, every flag has an env var)

| Flag | Env | Default |
|---|---|---|
| `--http-listen` | `GC_HTTP_LISTEN` | `0.0.0.0:80` |
| `--https-listen` | `GC_HTTPS_LISTEN` | `0.0.0.0:443` |
| `--dns-listen` | `GC_DNS_LISTEN` | `0.0.0.0:53` |
| `--admin-listen` | `GC_ADMIN_LISTEN` | `0.0.0.0:8080` |
| `--disable-dns` / `--disable-sni` | `GC_DISABLE_DNS` / `GC_DISABLE_SNI` | false |
| `--cache-ip` (repeatable / comma) | `GC_CACHE_IP` | required when DNS enabled |
| `--upstream-dns` (comma) | `GC_UPSTREAM_DNS` | `1.1.1.1,8.8.8.8` |
| `--dns-forward` (comma, optional) | `GC_DNS_FORWARD` | unset → REFUSED for non-overridden names |
| `--storage` `disk\|s3` | `GC_STORAGE` | `disk` |
| `--storage-path` | `GC_STORAGE_PATH` | `/data` |
| `--s3-bucket` / `--s3-endpoint` / `--s3-region` / `--s3-prefix` / `--s3-path-style` | `GC_S3_*` | — / — / `us-east-1` / "" / false |
| S3 credentials | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | env only |
| `--secrets` `storage\|vault` | `GC_SECRETS` | `storage` |
| secrets encryption key (storage backend) | `GC_SECRETS_KEY` | env only; required for `storage` backend |
| `--vault-addr` / `--vault-mount` / `--vault-path` | `GC_VAULT_ADDR` / `GC_VAULT_MOUNT` / `GC_VAULT_PATH` | — / `secret` / `game-cacher` |
| Vault auth | `VAULT_TOKEN`, or `VAULT_ROLE_ID`+`VAULT_SECRET_ID` (AppRole) | env only |
| `--cache-max-size` (e.g. `2TB`, `500GiB`) | `GC_CACHE_MAX_SIZE` | unset → unbounded |
| `--slice-size` | `GC_SLICE_SIZE` | `1MiB` |
| `--ui-path` | `GC_UI_PATH` | `/opt/game-cacher/ui` |
| admin password | `GC_ADMIN_PASSWORD` | env only; required unless `--disable-auth` |
| `--disable-auth` | `GC_DISABLE_AUTH` | false |
| `--instance-id` (loop detection) | `GC_INSTANCE_ID` | hostname |
| hidden: `--upstream-http-port` | `GC_UPSTREAM_HTTP_PORT` | 80 (tests point it at a local server) |
| log filter | `LOG` | `info` |

## Storage layout (Storage trait; disk root or S3 bucket/prefix)

```
settings.json                              SettingsStore (UI-editable settings)
state/<platform>.json                      managed-update state per platform (owned by the platform)
state/<platform>.runs.json                 last managed-update run (UpdateManager)
index/lru.json                             LRU index snapshot (object → size, last_access)
secrets/<name>.enc                         storage-backed secrets (ChaCha20-Poly1305, key from GC_SECRETS_KEY)
cache/<identifier>/<h[0..2]>/<h[2..4]>/<h>/meta.json   object metadata
cache/<identifier>/<h[0..2]>/<h[2..4]>/<h>/<slice>     slice bodies (decimal slice index)
```

`h` = hex sha256 of `<identifier><uri path>` (query string excluded, like
lancache, so CDN tokens don't fragment the cache). `identifier` = platform id
(`steam`, `epic`, `blizzard`, `origin`, `windowsupdate`).

`meta.json`: `{ "identifier", "host", "path", "total_size", "slice_size",
"content_type", "created": rfc3339 }`. `slice_size` is per object so changing
`--slice-size` never corrupts existing entries.

### settings.json

```json
{
  "platforms": { "steam": { "enabled": true }, "...": {} },
  "managed": {
    "steam":    { "selected": ["730", "570"] },
    "epic":     { "selected": ["Fortnite"] },
    "blizzard": { "selected": ["wow", "pro"] },
    "windowsupdate": { "selected": ["http://download.windowsupdate.com/.../x.cab"] }
  },
  "update_interval_minutes": 360
}
```
Unknown/missing platforms default to enabled; missing `managed` entries default to empty.

## Cache engine semantics (modelled on lancache/monolithic)

1. **Identifier**: first platform whose `matches_user_agent(ua)` is true (Steam:
   `Valve/Steam HTTP Client 1.0`), else first platform whose domain list
   matches the Host (`*.x.com` matches subdomains only, not the apex). No
   match → plain proxy, not cached.
2. Platform **disabled** → plain proxy, not cached (DNS also stops overriding it).
3. Only `GET`/`HEAD` with upstream 200/206 are cached. Other methods/statuses are
   proxied verbatim. `?nocache=...` bypasses the cache. `Platform::cache_policy(path)`
   can force bypass (e.g. Windows Update `authrootstl.cab|pinrulesstl.cab|disallowedcertstl.cab`).
4. **Slicing**: objects are fetched upstream in fixed `slice_size` ranges
   (`Range: bytes=a-b`); each slice is a separate storage object. Client
   `Range` requests (single range) are served from slices with 206 +
   `Content-Range`. If upstream ignores Range (returns 200 to a ranged request), the
   body is streamed through and split into slices as it arrives.
5. **Coalescing**: concurrent misses on the same slice share one upstream fetch.
6. **Redirects** (301/302/307/308) from upstream are followed server-side and
   never cached; the final body is stored under the original key.
7. **Loop detection**: requests carrying `X-GameCacher-Processed-By: <instance-id>`
   get 508. Upstream requests carry that header. Responses carry `X-GameCacher-Status: HIT|MISS|BYPASS`.
8. Platform `hide_etag()` (Blizzard) strips `ETag` from responses.
9. Upstream resolution uses `--upstream-dns` (never our own DNS); if the
   resolved address is one of `--cache-ip`, the request fails with 508.
10. LRU index: every slice read/write touches the object's `last_access` in
    memory; snapshot to `index/lru.json` every 60s and on shutdown; rebuilt by
    listing `cache/` if missing. When `total > max_size`, evict whole objects
    oldest-first down to 90% of max.

## Platforms

Trait `Platform` (`platforms/mod.rs`): `id`, `name`, `description`, `domains`,
`matches_user_agent`, `cache_policy`, `hide_etag`, `managed() -> Option<&dyn ManagedUpdates>`.
Domain lists come from uklans/cache-domains (`platforms/domains.rs`).

`ManagedUpdates`: `login_methods`, `auth_status`, `login(step)`, `logout`,
`list_games`, `update_game(game_id) -> UpdateReport`. Platforms fetch content
through the `Prefill` handle, which runs the request through the cache
engine in-process (with the platform identifier), so later LAN clients hit.

| Platform | Login | Games list | Update check | Prefill |
|---|---|---|---|---|
| Steam | QR or password+Guard (CM via steam-vent), refresh token in secrets | licenses → PICS | PICS buildid / manifest gid per depot | manifest → `/depot/<id>/chunk/<sha>` |
| Epic | auth code paste (legendary flow), refresh token in secrets | launcher assets | buildVersion | manifest → ChunksV4 paths |
| Blizzard | none | fixed product catalog (wow, wow_classic, pro, d3, hs, s2, hero, fenris, ...) | `versions` BuildConfig hash | build/cdn config, encoding, archive indexes + archives |
| Windows Update | none | user-entered URLs | always (HEAD/size) | GET each URL |
| Origin | passive only | — | — | — |

## REST API (admin listener)

All `/api/*` except `POST /api/auth/login` and `GET /api/auth/status` require
auth (cookie `gc_session` or `Authorization: Bearer <GC_ADMIN_PASSWORD>`).
Errors: `{"error": "message"}` with appropriate status.

| Method | Path | Body / Response |
|---|---|---|
| GET | `/healthz` | `ok` |
| GET | `/metrics` | OpenMetrics text |
| POST | `/api/auth/login` | `{"password"}` → 204 + Set-Cookie; 401 bad password |
| POST | `/api/auth/logout` | 204, clears cookie |
| GET | `/api/auth/status` | `{"authenticated": bool, "auth_enabled": bool}` |
| GET | `/api/status` | `{"version","uptime_secs","storage":{"backend","used_bytes","max_bytes","objects"},"secrets_backend","dns":{"enabled","cache_ips":[..],"forwarders":[..]},"sni_enabled"}` |
| GET | `/api/platforms` | `[PlatformSummary]` |
| GET | `/api/platforms/{id}` | `PlatformSummary` |
| PUT | `/api/platforms/{id}` | `{"enabled": bool}` → `PlatformSummary` |
| DELETE | `/api/platforms/{id}/cache` | purge platform cache → `{"deleted_objects","freed_bytes"}` |
| GET | `/api/platforms/{id}/managed` | `ManagedStatus` (404 if platform has no managed updates) |
| POST | `/api/platforms/{id}/managed/login` | `LoginRequest` → `LoginResponse` |
| POST | `/api/platforms/{id}/managed/logout` | 204 |
| GET | `/api/platforms/{id}/managed/games` | `[Game]` |
| PUT | `/api/platforms/{id}/managed/selection` | `{"selected":[game_id]}` → `ManagedStatus` |
| POST | `/api/platforms/{id}/managed/run` | start update run now → 202 `{"started": true}` (409 if running) |
| GET | `/api/settings` | `{"update_interval_minutes"}` |
| PUT | `/api/settings` | `{"update_interval_minutes"}` → same |

```
PlatformSummary = { id, name, description, enabled, domains: [str], managed: bool,
  stats: { requests, hits, misses, bypass, bytes_from_cache, bytes_from_upstream,
           storage_bytes, objects } }
ManagedStatus = { auth: AuthStatus, login_methods: [LoginMethod], selected: [str],
  running: bool, last_run: UpdateRun | null, next_run: rfc3339 | null }
AuthStatus = { logged_in: bool, account: str | null, required: bool, auth_url?: str }
LoginMethod = "none" | "qr" | "password" | "auth_code" | "manual_list"
LoginRequest = {"method":"qr"} | {"method":"qr_poll","challenge_id"} |
  {"method":"password","username","password"} | {"method":"guard","challenge_id","code"} |
  {"method":"auth_code","code"}
LoginResponse = {"status":"logged_in","account"} | {"status":"qr","challenge_id","url"} |
  {"status":"pending","challenge_id"} | {"status":"guard_required","challenge_id","guard_type":"email"|"device"|"device_confirmation"} |
  {"status":"failed","message"}
  (device_confirmation: the UI polls with qr_poll + challenge_id; a qr_poll may return a new "qr" url when Steam rotates it)
AuthStatus.auth_url (optional): where to obtain an auth code (Epic)
Game = { id, name, version: str | null, size_bytes: u64 | null, selected: bool }
UpdateRun = { started, finished: rfc3339 | null, games: [UpdateReport] }
UpdateReport = { game_id, name, previous_version, current_version, updated: bool,
  bytes_downloaded, bytes_already_cached, files, error: str | null }
```

## Metrics (OpenMetrics, prefix `gamecacher_`)

- `requests_total{platform,result=hit|miss|bypass}` counter
- `bytes_served_total{platform,source=cache|upstream}` counter
- `upstream_errors_total{platform}` counter
- `storage_used_bytes{platform}` / `storage_objects{platform}` gauge, `storage_max_bytes` gauge
- `evictions_total{platform}` counter
- `dns_queries_total{result=override|forward|refused|error}` counter
- `sni_connections_total{result=ok|error|loop}` counter, `sni_active_connections` gauge
- `managed_update_runs_total{platform,result=ok|error}`, `managed_update_bytes_total{platform}` counters

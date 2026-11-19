# Implementation Plan

Lead agent scaffolds shared interfaces; small, file-disjoint tasks go to
cheaper subagents working in isolated git worktrees, merged by the lead.
Contract: `docs/ARCHITECTURE.md`.

## Phase 0 — Scaffold (lead)
- Cargo workspace, all dependencies declared up front (subagents shouldn't need to touch Cargo.toml).
- `config.rs`, `error.rs`, `state.rs`, trait definitions: `Storage`, `SecretStore`,
  `Platform`, `ManagedUpdates`, `Prefill`; `MemoryStorage` for tests; module stubs.
- CLAUDE.md with conventions; git init + first commit.

## Phase 1 — Foundations (parallel subagents, worktrees)
| Task | Files | Tests |
|---|---|---|
| T1 Storage: disk + S3 (object_store) | `storage/disk.rs`, `storage/s3.rs` | unit (tempdir), S3 against MinIO when `GC_TEST_S3_ENDPOINT` set |
| T2 Secrets: storage-backed (encrypted) + Vault KV v2 (token/AppRole) | `secrets/*` | unit + wiremock |
| T3 DNS server + SNI passthrough + upstream resolver | `dns.rs`, `sni.rs`, `upstream.rs` | UDP/TCP DNS integration, SNI splice integration |
| T4 Metrics + settings store + platform domain matching | `metrics.rs`, `settings.rs`, `platforms/{mod,domains,origin,windows_update}.rs` (passive parts) | unit |
| T5 UI scaffold: theme, layout, login, API client, dashboard skeleton | `game-cacher-ui/` | tsc/eslint/build |

## Phase 2 — Core (parallel)
| Task | Files |
|---|---|
| T6 Cache engine: key, slicing, coalescing, redirects, loop detection, bypass, Range serving, `Prefill` impl | `cache/{key,engine,proxy}.rs`, `tests/cache_proxy.rs` |
| T7 LRU index + eviction | `cache/index.rs` |
| T8 Admin API + auth + scheduler + app wiring | `api/*`, `updates.rs`, `app.rs`, `tests/api.rs` |

## Phase 3 — Managed updates (parallel)
| Task | Files |
|---|---|
| T9 Blizzard TACT (no auth) | `platforms/blizzard/` (wiremock-backed tests) |
| T10 Epic (auth code, assets, manifest, chunks) | `platforms/epic/` |
| T11 Steam (CM login QR/password+Guard, licenses, PICS, manifests, chunks) | `platforms/steam/` |
| T12 Windows Update manual URL list | `platforms/windows_update.rs` |

## Phase 4 — Integration & packaging
- T13 UI pages: Dashboard, Platforms, Platform detail (managed updates: login flows incl. QR, game selection, run), Settings.
- T14 Dockerfile (UBI10 multi-stage like llm-manager), `dev/compose.yaml` (with MinIO + Vault dev), README.
- T15 End-to-end integration tests over all HTTP endpoints; `make check`.

## Phase 5 — Review loop
- Review subagent compares DESIGN.md (+ clarified decisions) to the implementation and lists divergences.
- Fix, re-review, until clean.

## Status (2026-11-19)
All phases complete. Four review rounds against DESIGN.md + clarified decisions:
round 1 (1 HIGH, 3 MEDIUM), round 2 (2 MEDIUM), round 3 (2 MEDIUM), round 4
(LOW only) — all fixed. Known accepted limitations are listed in README
"Platform notes" and docs/ARCHITECTURE.md.

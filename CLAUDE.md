# CLAUDE.md

## Overview

Game Cacher is a lancache-style caching proxy for game platforms (Steam, Epic,
Blizzard, Origin, Windows Update) with DNS overrides, SNI passthrough,
disk/S3 storage, secrets (storage or Vault), OpenMetrics, a React web UI and
managed updates (prefill) for Steam, Epic and Blizzard.

- `docs/ARCHITECTURE.md` — authoritative architecture, storage layout, config
  and REST contract. Read it before changing any JSON shape, storage key or
  flag, and update it in the same change.
- `docs/PLAN.md` — implementation plan / task breakdown.

## Layout

- `game-cacher/` — the Rust crate (lib + thin `main.rs`). One module per concern:
  `config`, `storage/`, `secrets/`, `settings`, `upstream`, `cache/`, `dns`,
  `sni`, `metrics`, `platforms/`, `updates`, `api/`, `app`.
- `game-cacher/tests/` — integration tests against real listeners (reqwest).
- `game-cacher-ui/` — React 19 + Vite + Tailwind 4 (see `game-cacher-ui/CLAUDE.md`).
- `docker/Dockerfile` — final artifact.

## Conventions

- anyhow for errors (thiserror only for errors that must be downcast), `log`
  macros (not tracing), clap derive with env vars, modest comments.
- Unit tests live in the same file as the code; HTTP mocks via wiremock;
  temp dirs via tempfile. Integration tests in `game-cacher/tests/`.
- Test public functions with real logic (parsing, branching, validation, error
  handling), both success and failure modes. Skip trivial getters.
- Keep functions under ~50 lines; modules expose a small public API.
- Never call real external services in tests.

## Commands

- `make check` — fmt check, clippy, tests, UI lint + build.
- `cargo test -p game-cacher` — Rust tests only.

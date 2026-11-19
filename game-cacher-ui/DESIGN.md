# game-cacher UI design

Authoritative design reference for the game-cacher web UI. It reuses the house
style of llm-manager-ui (dark terminal aesthetic) with game-cacher's states.
The API contract is `../docs/ARCHITECTURE.md` "REST API (admin listener)".

## Philosophy

- **Dark terminal aesthetic**: JetBrains Mono everywhere, near-black surfaces, one cyan-green accent.
- **Dense and functional**: 12px table/body text, compact padding, no decoration.
- **Color for status only**: surfaces are monochrome; color means state (see "Status tones").
- **Sharp geometry**: cards/tables have no radius; 4px buttons, 2px badges/inputs.
- **Operator UX**: the common path (enable platform, sign in, pick games, run) is one click per step; details on demand.

## Tokens (`src/index.css` `@theme`)

Copied verbatim from llm-manager-ui (minus the memory-estimate segments).

| Group | Tokens |
|---|---|
| Backgrounds | `bg-base #0d1117` page · `bg-surface #161b22` cards, sidebar, odd rows · `bg-raised #1c2128` headers, hover, active nav · `bg-overlay #21262d` dialogs, tooltips, bar tracks · `bg-scrim` modal backdrop |
| Borders | `border #30363d` · `border-muted #21262d` (between rows) · `border-strong #484f58` (hover) |
| Text | `text-primary #e6edf3` · `text-secondary #8b949e` labels · `text-muted #6e7681` hints · `text-faint #484f58` placeholders, separators |
| Accent | `accent #00d4aa` · `accent-hover #00f0c0` · `accent-muted` (15 %) · `on-accent` |
| Status | `status-{ok,warn,info,active,error,neutral}` + `-bg` (12 %) |
| Semantic | `warn-bg/-border`, `error-bg/-border`, `ok-bg/-border` (banners) |

Typography: `text-xs 11px` badges/labels · `text-sm 12px` tables, inputs, nav · `text-base 13px` card titles · `text-lg 15px` section titles · `text-xl 18px` · `text-2xl 22px` page titles. Weights 400 body, 500 buttons/badges, 600 headings/labels, 700 logo.

## Status tones

All state → color mapping lives in `src/lib/status.ts`; render with `StatusBadge` (dot + label, 2px radius, tinted bg). `pulse` animates the dot for in-progress states.

| Tone | Color | Used for |
|---|---|---|
| ok | green | platform enabled, DNS/SNI on, signed in, hit ratio >= 70 % |
| warn | amber | signed out (login required), hit ratio 30–70 % |
| info | blue | informational chips |
| active | cyan | run in progress (pulse), game updated in last run |
| error | red | failed game update, hit ratio < 30 % |
| neutral | gray | platform disabled, idle, up to date, no traffic yet |

## Components (`src/components/ui/`)

- **Sidebar** (`app-sidebar.tsx`): 200px, `bg-surface`, logo "GC" (accent, 18px bold) over "game cacher" (11px uppercase, 3px tracking). Nav: Dashboard, Platforms; footer: Settings, Metrics (opens `/metrics` in a new tab), Sign out (only when auth is enabled). 3px accent left border when active.
- **PageHeader**: breadcrumbs → title 22px semibold + status → 12px description; actions right-aligned.
- **Button**: `default`, `secondary`, `danger`, `ghost`, `ghost-danger`, `link`; sizes `default` 32px, `sm` 28px, `xs` 24px, `icon`, `icon-sm`. Default `type="button"`.
- **Card / CardHeader / CardBody**, **Table** (`bg-raised` 11px uppercase header, zebra rows), **StatusBadge** vs **Badge** (chips: domains, "managed updates", versions), **ProgressBar**, **AlertBanner**, **ConfirmDeleteDialog**, **Form** (`Input`, `Textarea`, `Select`, `Field`, `CheckboxField`, `Switch`), **ExternalLink**, **CopyButton**, **EmptyState**, **Skeleton / Loading / Spinner**, **Toasts**.
- **Stat / KeyValue** (`components/stat.tsx`): dashboard tiles (label, 18px value, hint) and `KEY value` rows.

## Formatting (`src/lib/format.ts`)

Bytes are decimal (`18.6 GB`, `858 MB`, ~3 significant figures). Counts `1,234`/compact `1.2M`. Hit ratio `83%` = hits / (hits + misses), `—` with no traffic. Relative times `5m ago` / `in 5m` with the absolute timestamp in `title`. Durations `3m 12s`, `2d 07h`.

## Pages

### Login `/login`
Centered card with the logo and an admin-password field. Wrong password shows an inline error. Skipped entirely when `auth_enabled` is false; an authenticated visit redirects to `/`.

### Dashboard `/`
Polls `/api/status` and `/api/platforms` every 5 s. Totals row (requests, hit ratio, bandwidth saved = bytes from cache, fetched upstream). Storage card (used vs max bar, "unbounded" when `max_bytes` is null; amber > 85 %, red > 95 %; object count, backends). Services card (DNS on/off, cache IPs, forwarders, SNI, uptime). Per-platform table: enabled badge, requests, hit ratio, from cache, from upstream, storage.

### Platforms `/platforms`
Table: name (link) + description, enabled badge, enable/disable `Switch` (`PUT /api/platforms/{id}`), "managed updates" badge, storage.

### Platform detail `/platforms/:id`
Header with enabled badge, managed badge and "Purge cache" (confirm dialog; toast reports freed bytes). Stat tiles, domains card. If `managed`, the Managed updates section:
- **Sign-in** card by `login_methods`: `qr` shows "Sign in with QR" → QR code of the returned `url` + 2 s `qr_poll`; `password` shows username/password, then a guard code field (`email` code, or `device` authenticator code) or "approve on your phone" with `qr_poll` polling (`device_confirmation`); `auth_code` links to `auth.auth_url` and takes the code (raw, or pasted `{"authorizationCode": "..."}`); `none` says "No sign-in required". Signed in: account + Sign out.
- **Update URLs** (`manual_list`, Windows Update): textarea, one URL per line, saved as the selection.
- **Games** (shown when signed in or auth not required): checkbox, name, id, version, size; filter box; Save selection (enabled only with unsaved changes).
- **Update runs**: Run now (409 → "already running" toast), running badge, next run time, last-run table (game, previous → current version, result badge, downloaded, already cached, files, error). Polls every 2 s while running.

### Settings `/settings`
Update interval (minutes) form (`GET/PUT /api/settings`) and a read-only note that storage, secrets and DNS come from env vars / flags.

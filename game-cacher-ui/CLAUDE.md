# OVERVIEW

- React SPA for game-cacher. Built with `npm run build` (`tsc -b && vite build`) into `dist/`; the Docker image copies it next to the binary and the Rust admin listener serves it at `/` with SPA fallback to `index.html`.
- Pages live in `src/pages/`, page-specific components in `src/components/<area>/` (`managed/` for managed updates), shared primitives in `src/components/ui/`.
- The UI talks only to the REST API in `../docs/ARCHITECTURE.md` "REST API (admin listener)" — that section is the contract. Types in `src/api/types.ts` are copied from it; keep them in sync and never change the contract from here.
- Dev server: `npm run dev` proxies `/api`, `/metrics`, `/healthz` to the Rust admin listener on `http://127.0.0.1:8080`.
- No backend handy? `npm run dev:mock` serves a stateful in-memory mock of `/api` from `mock/` (platform toggles, QR / password+guard / auth-code sign-in flows, update runs). `MOCK_AUTH=1 npm run dev:mock` also requires the admin password `admin`. It is a Vite dev-server plugin, never part of the production bundle. Update `mock/` when the contract changes.

# UI DEVELOPMENT

All UI changes follow `DESIGN.md` (tokens, components, status mappings, page layouts). Read it first.

## Stack
- React 19 + TypeScript (strict, `erasableSyntaxOnly`) + Vite 7
- Tailwind CSS 4: tokens in the `@theme` block of `src/index.css`. **Never hard-code colors** — use token classes (`bg-bg-surface`, `text-status-ok`, `border-error-border`, …).
- React Router 7 (`createBrowserRouter` in `src/main.tsx`), lucide-react icons
- Radix primitives only where needed: alert-dialog, tooltip, slot. Everything else is plain HTML.
- Plain HTML tables via `components/ui/table.tsx` (no table library)
- `qrcode.react` renders the managed-login QR code (colors via CSS variables)
- JetBrains Mono everywhere, dark only

## Auth
- `AuthProvider` (`src/context/auth.tsx`) reads `GET /api/auth/status` on boot. `Layout` redirects to `/login` when `auth_enabled && !authenticated`; with auth disabled the login page is skipped.
- `src/api/client.ts` redirects to `/login` on any `401` (except the auth calls themselves). Sign out lives in the sidebar footer.

## Data fetching
- `src/api/client.ts`: one typed function per endpoint. Non-2xx throws `ApiError` (`status` + the server's `{"error"}` message; status 0 = network error). Handles JSON and `204`. No `any`.
- `usePolling(fn, intervalMs, enabled)` (`src/hooks/usePolling.ts`) for everything: `intervalMs = 0` loads once. Memoize `fn` with `useCallback`; changing it refetches. Use `setData` to apply a mutation's response.
- Dashboard/platform lists poll every 5 s; managed status polls every 2 s while a run is active, 15 s otherwise. QR / device-approval sign-in polls `qr_poll` every 2 s from `auth-panel.tsx`.
- Toasts: `useToast().push({ variant, title, message })`.

## Conventions
- Every page starts with `PageHeader` (breadcrumbs, title, description, actions).
- Load errors: `AlertBanner variant="error"` with `onRetry`; keep stale data visible when a poll fails.
- Loading: `SkeletonRows` inside tables, `Skeleton` blocks, or `Loading` for whole panels.
- Destructive actions (purge cache) use `ConfirmDeleteDialog`.
- State → color goes through `src/lib/status.ts` (spec functions) and `StatusBadge`. Add new states there, not inline.
- Formatting only via `src/lib/format.ts` (decimal bytes, compact counts, relative time, durations, hit ratio).
- Buttons default to `type="button"`; interactive elements must be keyboard reachable.
- Extract a component once it has its own state/API calls or passes ~50 lines of JSX.

## Verification
From `game-cacher-ui/`: `npx tsc -b`, `npx eslint .` and `npx vite build` must all pass cleanly. To eyeball pages without the backend, run `npm run dev:mock`.

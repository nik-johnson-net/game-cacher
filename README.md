# Game Cacher

A LAN cache for game downloads. Clients use Game Cacher as their DNS server. It
answers the CDN hostnames of Steam, Epic Games, Blizzard, EA/Origin and
Windows Update with its own IP. It then serves those downloads from a sliced
HTTP cache kept on disk or in S3, and passes HTTPS through untouched by SNI.
For Steam, Epic and Blizzard it can also **pre-download updates** for games you
select, so the first LAN client to update already gets a cache hit.

It is modelled on [lancache](https://lancache.net) and ships as a single Rust
binary with a web UI, packaged as a Docker image.

## Features

- **DNS**: overrides the domains of enabled platforms (from
  [uklans/cache-domains](https://github.com/uklans/cache-domains)). Other names
  are forwarded to `GC_DNS_FORWARD`, or refused if that is unset.
- **HTTP cache**: content is stored in 1 MiB slices with lancache-style keys
  (platform + path, query string ignored). It handles Range requests, merges
  concurrent misses into one upstream fetch, follows redirects itself, detects
  request loops, and evicts by LRU above `GC_CACHE_MAX_SIZE`.
- **HTTPS SNI passthrough** on port 443, so hosts that serve both HTTP and
  HTTPS keep working.
- **Storage**: a local directory or any S3-compatible bucket. There is no
  database: settings live in `settings.json` in the same storage.
- **Secrets**: platform login tokens are stored encrypted in storage
  (`GC_SECRETS_KEY`), or in HashiCorp Vault KV v2.
- **Metrics**: OpenMetrics at `/metrics`, covering requests, hit/miss, bytes
  served by source, storage per platform, evictions, DNS, SNI and update runs.
- **Web UI**: a dashboard, platform enable/disable, per-platform cache purge,
  and managed updates (Steam QR or password + Steam Guard login, Epic
  authorization code, Blizzard needs no login, Windows Update takes a list of
  URLs).

## Quick start

```sh
docker build -f docker/Dockerfile -t game-cacher:dev .
docker run -d --name game-cacher \
  -p 53:53/udp -p 53:53/tcp -p 80:80 -p 443:443 -p 8080:8080 \
  -v /srv/game-cache:/data \
  -e GC_CACHE_IP=192.168.1.10 \
  -e GC_DNS_FORWARD=192.168.1.1 \
  -e GC_ADMIN_PASSWORD=change-me \
  -e GC_SECRETS_KEY="$(openssl rand -hex 32)" \
  -e GC_CACHE_MAX_SIZE=2TB \
  game-cacher:dev
```

Then:

1. Point your LAN's DHCP DNS server at `192.168.1.10`.
2. Open `http://192.168.1.10:8080` and sign in with the admin password.

`dev/compose.yaml` runs a full local stack with S3 (SeaweedFS) and Vault
storage on non-privileged ports.

## Configuration

Every flag has an environment variable. Run `game-cacher --help` for the full
list; `docs/ARCHITECTURE.md` has the table. The most common ones:

| Env | Default | Purpose |
|---|---|---|
| `GC_CACHE_IP` | — (required with DNS) | IP(s) returned for overridden domains |
| `GC_UPSTREAM_DNS` | `1.1.1.1,8.8.8.8` | Real DNS used to reach CDNs (never this cache) |
| `GC_DNS_FORWARD` | — | Where to forward other DNS queries |
| `GC_STORAGE` | `disk` | `disk` (`GC_STORAGE_PATH`) or `s3` (`GC_S3_BUCKET`, `GC_S3_ENDPOINT`, `GC_S3_REGION`, `GC_S3_PREFIX`, `GC_S3_PATH_STYLE`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`) |
| `GC_SECRETS` | `storage` | `storage` (needs `GC_SECRETS_KEY`) or `vault` (`GC_VAULT_ADDR`, `GC_VAULT_MOUNT`, `GC_VAULT_PATH`, `VAULT_TOKEN` or `VAULT_ROLE_ID`+`VAULT_SECRET_ID`) |
| `GC_CACHE_MAX_SIZE` | unbounded | e.g. `2TB`, `500GiB` |
| `GC_SLICE_SIZE` | `1MiB` | Cache slice size for new objects |
| `GC_ADMIN_PASSWORD` | — (required) | Web UI / API password (or `GC_DISABLE_AUTH=true`) |
| `GC_DISABLE_DNS` / `GC_DISABLE_SNI` | false | Turn off DNS or SNI if you run your own |
| `GC_ALLOW_PRIVATE_UPSTREAM` | false | Allow upstream hosts that resolve to private/loopback addresses (e.g. a local mirror). Off by default so the cache can't be used to reach into your LAN |
| `LOG` | `info` | Log filter (`debug`, `game_cacher=debug`, ...) |

The API accepts `Authorization: Bearer $GC_ADMIN_PASSWORD` for scripting.

## Platform notes

- **Steam**: the Steam client switches to HTTP downloads once
  `lancache.steamcontent.com` resolves to the cache. Requests are recognised by
  the Steam user agent whatever the CDN host is.
- **Epic Games**: chunk downloads are cached. To set up managed updates, sign
  in with the authorization code from the link shown in the UI.
- **Blizzard**: no login is needed. Managed updates prefetch each selected
  product's archives and the encoding, install, download and size files.
  Loose files are not prefetched yet.
- **EA app / Origin**: passive caching only, since there is no open download
  protocol to drive pre-downloads.
- **Windows Update**: passive caching. Specific update files can be prefetched
  by entering their URLs. Certificate trust lists are never cached. Turn off
  Delivery Optimization peer-to-peer on clients for best results.

## Development

```sh
make check   # fmt, clippy, tests, UI lint + build
cargo test   # Rust unit + integration tests
cd game-cacher-ui && npm run dev:mock   # UI against a mock API
```

See `CLAUDE.md` for conventions and `docs/ARCHITECTURE.md` for the design.

# Game Cacher

Game Cacher is a service for caching and managing e-distributed video games from popular platforms like Steam. It supports managed updates, DNS redirects, S3-compatible storage, and HTTPS SNI.

## Features

* DNS Redirects. Serves DNS to override domains used by supported platforms to itself.
* DNS Forwarding. Can be configured to forward DNS to another server if the requested domain does not match any redirected domains.
* HTTPS SNI support. A domain can be overridden for HTTP traffic, and HTTPS traffic to the same domain can be passed through.
* Multiple storage options: Choose between a folder on disk, or S3-compatible storage.
* Metrics support. Expose OpenMetrics compatible endpoint, providing live data on cache hits, storage usage, requests, and more.
* Web UI Dashboard. Display information on supported platforms, usage, storage, and enable/disable specific platforms.
* Managed Updates. Supported platforms include a Web UI component to log in (if required) to platforms, list available games, and select games to manage updates for. Selected games will automatically be verified against cached data to pre-download updates.
* Vault support. Secrets like login information can be stored in secure storage like Hashicorp Vault.

## Implementation

Implement Game Cacher using Rust. For the UI, see themeing from rack-director or llm-manager. Use axum for HTTP service support.

It is important to write full integration tests against HTTP endpoints.

Platforms make a great abstraction. So does the storage interface (Disk or S3) and secrets interface (storage interface or Vault).

See if you can design without needing a separate database, relying on the storage layout and a dedicated file to store settings. 

Settings like what storage interface to use and specific details, what secrets interface to use, should be passed on the command line or ENV variables. Any logins for those should be loaded by ENV vars.

The final artifact should be a Docker image.

## Platforms to support

* Steam
* Epic Games
* Blizzard
* Origin
* Windows Updates

## Prior Art

Look at https://github.com/lancachenet/monolithic

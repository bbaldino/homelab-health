# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/bbaldino/homelab-health/releases/tag/v0.1.0) - 2026-07-31

### Added

- add unraid check (GraphQL array/disk/parity/free-space health)
- configurable sample retention (HEALTH_SAMPLE_RETENTION_DAYS, default 7)
- UI uptime timeline + history panel
- history and uptime API endpoints
- scheduler records samples + transitions and prunes
- pure compute_uptime over transitions
- history tables + record/query/prune in store
- schema-driven add/edit/delete/run monitor UI
- web UI scaffold + typed API client + status dashboard
- embed and serve the web UI via rust-embed with SPA fallback
- add secret flag to config-field schema (mask token inputs)
- add music-assistant WebSocket check
- add json-health check type and register as builtin
- main binary, seed.sh, run docs
- status endpoints and run-now
- axum API for monitors CRUD and check-types
- add Scheduler with debounce and periodic loop
- add Debounce hysteresis
- add MonitorStatus read models to store
- store update/delete, serde derives, file-db pool
- add env-based bootstrap config
- add Registry::with_builtins
- add SQLite store for monitors and current status
- add frigate-camera check with per-camera components
- add tcp check type
- add http check type
- add CheckType trait, config schema, and registry
- add CheckReport and component rollup
- add Status enum with severity rank

### Fixed

- uptime percent over observed time (exclude unknown/no-data)
- clamp retention floor, drop zero-width uptime segments, test cascade
- dockerignore ui/node_modules, omit null config fields, 404 unmatched api paths
- ensure ui/dist exists at compile time via build.rs
- make migration idempotent so the daemon is restart-safe
- create sqlite file if missing and use try_get to avoid panics
- remove panic path in frigate check and cover empty-cameras case
- enforce Component message invariant and cover rollup branches

### Other

- pin release-plz action to v0.5 (v0 tag does not exist)
- add release-plz for automated version bumps and tagging
- adopt canonical publish-image workflow
- Replace continuous uptime bar with status-page-style bucket bars
- Add history/timeline (Plan 4) plan + spec decision
- web UI dev and build workflow
- build the web UI in a Node stage and embed it in the image
- gitignore playwright verification artifacts
- Add web UI (Plan 3) implementation plan
- add Dockerfile and GHCR publish workflow
- gitignore sqlite wal/shm runtime files
- Add json-health check plan and spec decision
- graceful shutdown, explicit WAL/busy_timeout, lower check timeout, drop save_status unwrap
- Add Plan 2a implementation plan (runnable daemon: scheduler, API, main)
- Add Plan 2 design decisions to spec (API-only monitors, env bootstrap config, 2a/2b split)
- deny unknown config fields and drop unused thiserror
- Add /health endpoint contract for custom services
- Add core engine implementation plan (plan 1 of 3)
- Add homelab health monitor design spec

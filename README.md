# homelab-health

Self-hosted service health monitor.

## Run locally

```bash
cargo run
# env (optional): HEALTH_BIND=0.0.0.0:8080  HEALTH_DB=health.db
./seed.sh                      # seed a few monitors (edit hosts first)
curl -s localhost:8080/api/v1/status | jq
```

Monitors are managed through the web UI or the API (`/api/v1/monitors`); the
SQLite DB is internal app state. See `docs/health-endpoint-contract.md` for the
service-side `/health` contract.

## Web UI

The UI (Preact + TypeScript, in `ui/`) is embedded in the binary via rust-embed
and served at `/` — a live status dashboard plus schema-driven add/edit/delete.

- **Dev (hot reload):** run the backend (`cargo run`) and, separately,
  `npm --prefix ui run dev` — Vite serves on :5173 and proxies `/api` to :8080.
- **Prod build:** `npm --prefix ui run build` then `cargo build --release`
  (release embeds `ui/dist`; the Docker image does both in one build).
- Without a UI build, the backend still serves the JSON API and a minimal
  fallback page at `/`.

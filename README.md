# homelab-health

Self-hosted service health monitor.

## Run locally

```bash
cargo run
# env (optional): HEALTH_BIND=0.0.0.0:8080  HEALTH_DB=health.db
./seed.sh                      # seed a few monitors (edit hosts first)
curl -s localhost:8080/api/v1/status | jq
```

Monitors are managed only through the API (`/api/v1/monitors`); the SQLite DB
is internal app state. See `docs/health-endpoint-contract.md` for the
service-side `/health` contract.

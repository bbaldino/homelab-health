#!/usr/bin/env bash
# Seed a few monitors via the API for live testing. Override BASE / targets as needed.
set -euo pipefail
BASE="${BASE:-http://localhost:8080}"

post() {
  curl -sS -X POST "$BASE/api/v1/monitors" \
    -H 'content-type: application/json' \
    -d "$1" | jq -c . || echo "  (is the daemon running? is jq installed?)"
}

# HTTP check against Plex web UI (adjust host).
post '{"name":"Plex","type_id":"http","interval_secs":30,"config":{"url":"http://plex.lan:32400/web/index.html"}}'

# TCP check against a service port (adjust host/port).
post '{"name":"Unraid SMB","type_id":"tcp","interval_secs":30,"config":{"host":"unraid.lan","port":445}}'

# Frigate per-camera check (adjust base_url).
post '{"name":"Frigate","type_id":"frigate-camera","interval_secs":60,"config":{"base_url":"http://frigate.lan:5000"}}'

echo "Seeded. Now: curl -s $BASE/api/v1/status | jq"

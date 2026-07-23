# syntax=docker/dockerfile:1

# ---- ui builder ----
FROM node:22-slim AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

# ---- builder ----
FROM rust:1-bookworm AS builder
# aws-lc-rs (pulled in by rustls) builds vendored C with CMake.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY migrations ./migrations
# The built UI must be present before compiling (rust-embed embeds ui/dist).
COPY --from=ui /ui/dist ./ui/dist
RUN cargo build --release --locked

# ---- runtime ----
FROM debian:bookworm-slim
# ca-certificates so http/json-health checks can validate HTTPS targets.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/homelab-health /usr/local/bin/homelab-health
# Bind on all interfaces inside the container; the host port is mapped by the
# runtime (Unraid/compose). DB lives on the /data volume so it survives restarts.
ENV HEALTH_BIND=0.0.0.0:8080 \
    HEALTH_DB=/data/health.db \
    RUST_LOG=info
VOLUME ["/data"]
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/homelab-health"]

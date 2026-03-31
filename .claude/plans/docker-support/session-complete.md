# Phase 7: Docker Support — Session Complete

## What Was Done

Full Docker containerization of the Coordin8 stack.

### Files Created/Modified
- `Dockerfile.djinn` — multi-stage Rust build; installs protoc v28.3 from GitHub release binary; runtime on debian:bookworm-slim; exposes 9001/9002/9003
- `Dockerfile.greeter` — multi-stage Go build; runtime on debian:bookworm-slim; exposes 50051
- `docker-compose.yml` — djinn + greeter services on `coordin8-net` bridge; healthcheck on 9001; greeter `depends_on: djinn: condition: service_healthy`; proxy range 9100-9200 exposed
- `.dockerignore` — excludes target/, node_modules/, build/, .claude/
- `djinn/crates/coordin8-proxy/src/manager.rs` — added `ProxyConfig` with `bind_host`, `port_range`, `from_env()`; `bind_listener()` scans port range sequentially
- `djinn/crates/coordin8-djinn/src/main.rs` — `ProxyConfig::from_env()` wired in
- `examples/hello-coordin8/greeter_service/main.go` — `ADVERTISE_HOST` env var so Djinn proxy routes to `greeter:50051` not `localhost:50051`

### Key Decisions
- `rust:latest` required (local Rust is 1.93.1; Cargo.lock v4 won't parse with 1.77)
- protoc installed from official GitHub release (apt version too old/missing well-known types)
- `PROXY_BIND_HOST=0.0.0.0` + fixed range `9100-9200` required for proxy ports to be reachable from host
- `restart: on-failure` on greeter handles DNS race at startup (Djinn not yet resolvable when greeter first tries to connect)
- `ADVERTISE_HOST=greeter` on greeter service — Djinn proxy forwards to this name on the Docker bridge

### Verified Working
All three language clients (`go run`, `./gradlew run`, `npx ts-node`) confirmed returning `Hello, Docker!` against the containerized stack.

### Git State
- `03c39d3` Phase 7: Docker support
- `7856d0f` Remove committed Java .class files, add bin/ to .gitignore

## Blockers / Surprises
- Cargo.lock v4 format blocked by older rust image — must use `rust:latest`
- protoc apt package missing well-known types — binary install from GitHub release
- Proxy ports not reachable from host until port range + 0.0.0.0 binding added
- Docker group membership change required `newgrp docker` in user terminal (can't run from Claude)

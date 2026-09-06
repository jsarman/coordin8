# Hardening Roadmap — Session 1 Completion Notes

**Status:** Items 1, 2, and 4 complete and merged to `main`, 2026-09-05/06 — via #20 (item 1), #21 (item 2), #22 (item 4), and a corrective consolidation PR #23. #21 and #22 were opened as PRs stacked on non-`main` branches (`worktree-boot-order-analysis`, `feat/per-service-docker`); merging them landed the content on those branches, not `main` — #23 (base: `main`, head: `feat/split-mode-dynamo`) brought everything over in one clean PR once the mismatch was caught. Lesson captured in memory as `feedback_stacked_pr_base_branches`. All four stacked branches deleted post-merge.

## Item 1 — Flexible boot order + health checks

Full design and live-verification detail in `boot-order-health-design.md` and `decisions.md`. Summary: confirmed boot-order independence in the "won't crash" sense already existed (retry-forever discovery everywhere), but four of six split-mode services blocked their entire serve loop behind that discovery and had no real health signal. Added the standard gRPC Health Checking Protocol (`tonic-health`) to all six services, and restructured `EventMgr`/`Space`/`TxnMgr`/`Proxy` to serve immediately against a new `PendingLeasing`/`PendingCapabilityResolver` (`coordin8-bootstrap`), resolving the real dependency in a background task that installs it and flips health `NotServing` → `Serving`. New `Error::Unavailable` (`coordin8-core`) surfaces as `Status::unavailable` at every affected RPC boundary. Verified live: server up immediately with dependency missing, health `NOT_SERVING`, a real RPC fails in ~0.02s with a clear message, then flips to `SERVING` and succeeds the instant Registry/LeaseMgr appear — no restart anywhere.

## Item 2 — Docker: one container per service

New file: `docker-compose.split.yml`. Runs all six split-mode services as separate containers, reusing the existing `Dockerfile.djinn` image — its `ENTRYPOINT ["djinn"]` already accepts the subcommand as `command:` in compose, so no new Dockerfiles were needed at all.

### New: self-contained healthcheck CLI

Added `djinn healthcheck --addr <target>` (`coordin8-djinn/src/main.rs`, `services.rs`) — calls the standard gRPC Health Checking Protocol's `Check` RPC via `tonic_health::pb::health_client::HealthClient`, exits 0 on `Serving`, 1 otherwise, bounded by an internal 3s timeout. Lets Docker's own `healthcheck:` directive stay self-contained on the same binary already in the image, instead of needing to bundle a separate `grpc_health_probe`. Skips the tracing-subscriber init for this subcommand specifically (it runs every few seconds under Docker, signals via exit code not logs). Verified directly: exit 0 against a running Registry, exit 1 with a clear message against an unreachable address.

### Deliberately no `depends_on`

The compose file intentionally omits `depends_on` between the six services — this is the real-world proof of item 1, not just a unit test. Verified live: `docker compose up` starts all six with no ordering guarantee, and all six still reach Docker's `healthy` state.

### Functional validation against real examples

Brought the stack up, then ran two existing examples against it unmodified (same ports as the monolith):
- **hello-coordin8** — Go greeter service registered with the split LeaseMgr/Registry containers; the client resolved it through the split Proxy and got a reply. Full round trip.
- **market-watch** — full 5/5 EventMgr sequence (3-event mailbox drain on reconnect, then 2 live) against the split EventMgr container.

### Bug found and fixed along the way

The client (a host-run `go run` process, not containerized) couldn't be reached by the containerized Proxy at first — `rpc error: ... Unavailable ... error reading server preface: EOF`. Root cause: `proxy` needs `extra_hosts: host.docker.internal:host-gateway` to dial back to host-run services, and the existing bundled `docker-compose.yml` only had that on the monolith `djinn` service. Added it specifically to `proxy` in the split compose (it's the container actually making the outbound call), and set `ADVERTISE_HOST=host.docker.internal` when running the greeter locally so its registered address was reachable from inside the Proxy container.

### Also hit: a Docker networking glitch (unrelated to any of this)

First `docker compose up` attempt on the split file left every container with `"Networks": {}` — genuinely unattached, despite the compose network being defined and `docker compose ps` reporting them as running. DNS resolution between containers failed completely as a result. A full `docker compose down` + `docker network prune -f` + fresh `up` resolved it cleanly on the next attempt; network attachment was then correct immediately. Plausibly related to the Docker Desktop reinstall earlier in the session (`space-race-txn-failsafe/session-1-complete.md`) — noting here in case it recurs, since it isn't a fix, just a workaround that happened to work.

### Scope note: single instance only

Uses `COORDIN8_PROVIDER=local` (in-memory) implicitly — each container's data is private to itself. Running more than one instance of any service isn't meaningful yet (state wouldn't be shared across instances); that's blocked on item 4 (DynamoDB split-mode wiring), which is next.

## Item 4 — Persistent backing store outside bundled mode

Precisely scoped during the earlier `space-race-txn-failsafe` session: `run_all()` already branched on `COORDIN8_PROVIDER` (dynamo vs local); every split-mode function hardcoded `Arc::new(InMemory*Store::new())` directly, no branch at all.

Fix: extracted the provider-selection logic into five shared helpers — `lease_store_from_env()`, `registry_store_from_env()`, `event_store_from_env()`, `txn_store_from_env()`, `space_store_from_env()` (`services.rs`) — and refactored `run_all()` to use them instead of its own inline match (same behavior, removes duplication). Every split-mode function now calls the matching helper. `run_registry_on_listener`'s own private lease bookkeeping (used for its entries' TTLs, separate from the shared LeaseMgr service) is included too. `run_proxy` needed nothing (stateless).

### Verified live, definitively — not just "doesn't error"

Brought up MiniStack + deployed the DynamoDB tables via the existing `infra/dynamodb-tables.cfn.yml` (same template the bundled `docker-compose.yml`'s `cfn-init` step already uses). Ran `djinn registry` standalone with `COORDIN8_PROVIDER=dynamo` pointed at MiniStack, registered a test entry via the `coordin8` CLI, confirmed it was queryable. Then `kill -9`'d the process — a real crash, not a graceful shutdown — and started a fresh process with identical config.

**The entry was still there.** Confirmed via `coordin8 registry lookup` returning the exact same `capability_id` and attributes. This is genuine persistence surviving a process crash, backed by real (emulated) DynamoDB — the thing item 4 was actually about, not just confirming the code compiles and constructs without erroring.

### Note found along the way (not a bug, just a naming trap)

The `coordin8` CLI's global `--host` flag (Djinn host to connect to) collides by name with `registry register`'s own `--host` flag (the transport host being registered) — the subcommand's local flag shadows the global one, and the CLI hardcodes standard ports (`<host>:9002` for Registry, etc.) rather than accepting a custom port. Had to run the test Registry on the standard port 9002 instead of a scratch port for the CLI to reach it. Not something this session touched or needs to fix — just a trap to remember for next time a CLI-driven test needs a non-standard port.

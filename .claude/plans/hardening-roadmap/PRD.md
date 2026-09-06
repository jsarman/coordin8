# Hardening Roadmap — PRD

> **Status: In progress.** Items 1, 2, and 4 are COMPLETE and merged to `main` (2026-09-05/06, via #20, #21, #22, and a corrective consolidation PR #23 — #21/#22 were stacked on non-`main` branches, so merging them didn't land on `main` until #23; see `feedback_stacked_pr_base_branches` lesson). See `session-1-complete.md` for the full implementation writeup. Started right after `.claude/plans/space-race-txn-failsafe/` (merged 2026-09-05). Docker-centric and near-term — a precursor to, not a duplicate of, `.claude/plans/phase3-cloud-topology/` (which is the full AWS-serverless endgame). Where they overlap, this PRD cross-references rather than repeats.

## Goal

Make Coordin8 resilient to real-world operational conditions — services starting in any order, running one-per-container, cross-platform, with persistent storage available outside the bundled monolith, and secured over the wire — without yet reaching for the full Lambda/Fargate/SNS topology in `phase3-cloud-topology`.

## Motivation

CLAUDE.md currently documents boot order as "strict and load-bearing... non-negotiable." That's today's implementation, not the target. Split mode already lets each service run as its own process/subcommand, but several gaps stop that from being a real deployable, containerized system yet — found and scoped during the `space-race-txn-failsafe` validation session.

## Items

### 1. Flexible boot order / graceful degraded health — ✅ DONE

Full design + implementation in `boot-order-health-design.md` and `decisions.md` (this folder). Summary:

- Confirmed (code + live test) that boot-order independence in the "won't crash" sense already existed — `retry_forever` backs all cross-service discovery. The real gap was that four of six split-mode services blocked their entire gRPC serve loop behind that discovery, and there was no real health signal (a bare TCP probe reports "up" the whole time regardless).
- Added the standard gRPC Health Checking Protocol (`tonic-health`) to all six split-mode services.
- Restructured EventMgr/Space/TxnMgr/Proxy to construct immediately against a `PendingLeasing`/`PendingCapabilityResolver` (new types in `coordin8-bootstrap`) and serve right away; a background task resolves the real dependency, installs it, and flips health `NotServing` → `Serving`. New `Error::Unavailable` (`coordin8-core`) surfaces as `Status::unavailable` at every affected RPC boundary instead of the generic `internal` bucket.
- Verified live end-to-end: server up immediately with dependency missing, health `NOT_SERVING`, a real RPC fails in ~0.02s with a clear "waiting on dependency: X" message (not a hang), then flips to `SERVING` and succeeds the instant Registry/LeaseMgr come up — no restart needed anywhere.

`phase3-cloud-topology/resilience-plan.md:134`'s note that "the *inter-service* boot order relaxes because they're independent processes" (in that plan's DynamoDB+SNS context) is the same relaxation achieved here, but for Docker-Compose-level deployment, ahead of any AWS work.

### 2. Docker: one container per service — ✅ DONE

`docker-compose.split.yml` (repo root) runs all six split-mode services (`registry/lease/event/space/txn/proxy`), each its own container, reusing the existing `Dockerfile.djinn` image (its `ENTRYPOINT ["djinn"]` already accepts the subcommand as `command:`). No new Dockerfiles needed.

- Added a self-contained `djinn healthcheck --addr <target>` CLI subcommand (calls the standard gRPC Health Checking Protocol's `Check` RPC, exits 0/1) so container healthchecks don't need a separately-bundled `grpc_health_probe` binary.
- **Deliberately omits `depends_on`** — this is the real-world proof of item 1's boot-order independence, not just a unit test. `docker compose up` starts all six with no ordering guarantee.
- Verified live: all six reach `healthy` with no `depends_on` at all. Functionally validated against two real examples end-to-end — hello-coordin8 (Go greeter service registers, client resolves through split Proxy/Registry/LeaseMgr, gets a reply) and market-watch (5/5 EventMgr mailbox-drain + live sequence against the split EventMgr container).
- One fix needed along the way: `proxy` needs `extra_hosts: host.docker.internal:host-gateway` to forward to host-run services (examples running via `go run`, not containerized) — the existing bundled `docker-compose.yml` only had this on the monolith `djinn` service; split mode needs it specifically on `proxy`, since that's the container actually dialing out.
- Uses `COORDIN8_PROVIDER=local` (in-memory) implicitly — each container's data is private to itself. Running more than one instance of any service isn't meaningful yet; that needs item 4 (DynamoDB split-mode wiring) first, so state is actually shared.

### 3. Fix hardcoded OS-specifics

A few not-yet-enumerated hardcoded OS-specific bits break cross-platform use. Needs an audit pass to enumerate before scoping further.

### 4. Persistent backing store outside bundled mode — ✅ DONE

Extracted the provider-selection logic `run_all()` already had into five shared helpers (`lease_store_from_env()` etc., `services.rs`) and refactored `run_all()` to use them (same behavior, less duplication). Every split-mode function now calls the matching helper instead of hardcoding `Arc::new(InMemory*Store::new())` — `COORDIN8_PROVIDER=dynamo` works identically in split mode and bundled mode now. `run_registry_on_listener`'s own private lease bookkeeping (its entries' TTLs) is included too, so Registry entries also survive a restart under the dynamo provider. `run_proxy` needed nothing (stateless).

**Verified live, definitively:** brought up MiniStack + deployed the DynamoDB tables (existing `infra/dynamodb-tables.cfn.yml`, same CFN template the bundled compose already uses), ran `djinn registry` standalone with `COORDIN8_PROVIDER=dynamo`, registered a test entry via the `coordin8` CLI, confirmed it was queryable, then `kill -9`'d the process and started a fresh one with identical config. **The entry was still there** — genuine persistence across a process crash/restart, not just "constructs without erroring."

Matches the master PRD's existing "DynamoDB/MiniStack provider-swap test — Gap" line under Djinn Split Mode (`.claude/plans/PRD.md`), now resolved.

### 5. Space watch durability (tracked separately as issue #17)

Not really a new roadmap item so much as a known consequence of item 4/broadcast design: Space's `watch()`/`Notify()` is Jini-spec-faithful best-effort (confirmed against the actual Jini Distributed Events spec during validation) — no guaranteed or retroactive delivery. `settlement-engine` (auction-house example) relies on it for something that needs durability and its README oversells that. Real fix is architectural: route auction-expiry watching through EventMgr's durable subscribe instead (the tier the design napkin already designates for this, `coordin8-design-napkin.md:143-155`) — or, longer-term, externalize the broadcast entirely per `phase3-cloud-topology/resilience-plan.md`'s "Broadcast Problem" section, which already lists `space_expiry_tx` as one of the five channels needing this. See [issue #17](https://github.com/jsarman/coordin8/issues/17) and `.claude/plans/space-race-txn-failsafe/session-1-complete.md` for the full trace.

### 6. gRPC security via JWT

Add JWT auth to the gRPC surface, plus a broader platform security discussion — not covered anywhere in existing plans. Open questions: token issuance/rotation, which services validate tokens (every service vs. gateway/proxy only), service-to-service vs. client-to-service auth, mTLS vs. JWT-over-TLS, interaction with Registry self-registration (does a service need a valid identity before it's allowed to register?).

## Non-Goals

- Full AWS serverless topology (Lambda/Fargate/SNS/SQS/DynamoDB Streams) — that's `phase3-cloud-topology`, comes after this.
- Rewriting Space's `watch()` to be durable in-place — item 5's real fix is routing through EventMgr or, longer-term, the broadcast-externalization plan already scoped in `phase3-cloud-topology/resilience-plan.md`.

# Hardening Roadmap — PRD

> **Status: Not started.** Planned as the next initiative after `.claude/plans/space-race-txn-failsafe/` (merged 2026-09-05). Docker-centric and near-term — a precursor to, not a duplicate of, `.claude/plans/phase3-cloud-topology/` (which is the full AWS-serverless endgame). Where they overlap, this PRD cross-references rather than repeats.

## Goal

Make Coordin8 resilient to real-world operational conditions — services starting in any order, running one-per-container, cross-platform, with persistent storage available outside the bundled monolith, and secured over the wire — without yet reaching for the full Lambda/Fargate/SNS topology in `phase3-cloud-topology`.

## Motivation

CLAUDE.md currently documents boot order as "strict and load-bearing... non-negotiable." That's today's implementation, not the target. Split mode already lets each service run as its own process/subcommand, but several gaps stop that from being a real deployable, containerized system yet — found and scoped during the `space-race-txn-failsafe` validation session.

## Items

### 1. Flexible boot order / graceful degraded health

Any service should be able to start in any order. A service missing a dependency (Registry or LeaseMgr down) should still report itself healthy to the container host/orchestrator — "alive, waiting on dependency X" — not get restart-looped. Today's health check is a bare TCP probe on `:9001` (CLAUDE.md), which can't express that distinction.

`phase3-cloud-topology/resilience-plan.md:134` already notes "the *inter-service* boot order relaxes because they're independent processes" as a consequence of that plan's DynamoDB+SNS topology — this item is the same relaxation, but for Docker-Compose-level deployment, ahead of any AWS work.

### 2. Docker: one container per service

Split mode exists as `djinn` subcommands (`lease/registry/event/space/txn/proxy`, `.claude/plans/djinn-split/PRD.md`, done 2026-04-09) but each needs to become its own deployable container/image, composable independently — not just a locally-run subcommand. Builds toward, but is a smaller step than, `phase3-cloud-topology`'s Fargate task definitions.

### 3. Fix hardcoded OS-specifics

A few not-yet-enumerated hardcoded OS-specific bits break cross-platform use. Needs an audit pass to enumerate before scoping further.

### 4. Persistent backing store outside bundled mode

**Scoped precisely during the validation session.** The trait abstraction (`LeaseStore`/`RegistryStore`/`EventStore`/`TxnStore`/`SpaceStore` in `coordin8-core`, implemented by both `providers/local` and `providers/dynamo`) is solid. The gap is purely in `djinn/crates/coordin8-djinn/src/services.rs`: `run_all()` branches on `COORDIN8_PROVIDER` (dynamo vs local, lines 63-118); every split-mode function — `run_registry_on_listener` (:319,:321), `run_lease_on_listener_with_shutdown` (:430), `run_event_on_listener` (:562), `run_space_on_listener` (:694), `run_txn_on_listener` (:847) — hardcodes `Arc::new(InMemory*Store::new())` directly, no branch at all. `run_proxy` needs nothing (stateless). Fix is mechanical: thread the same match arm into each split-mode function.

Matches the master PRD's existing "DynamoDB/MiniStack provider-swap test — Gap" line under Djinn Split Mode (`.claude/plans/PRD.md`), now with exact fix locations.

### 5. Space watch durability (tracked separately as issue #17)

Not really a new roadmap item so much as a known consequence of item 4/broadcast design: Space's `watch()`/`Notify()` is Jini-spec-faithful best-effort (confirmed against the actual Jini Distributed Events spec during validation) — no guaranteed or retroactive delivery. `settlement-engine` (auction-house example) relies on it for something that needs durability and its README oversells that. Real fix is architectural: route auction-expiry watching through EventMgr's durable subscribe instead (the tier the design napkin already designates for this, `coordin8-design-napkin.md:143-155`) — or, longer-term, externalize the broadcast entirely per `phase3-cloud-topology/resilience-plan.md`'s "Broadcast Problem" section, which already lists `space_expiry_tx` as one of the five channels needing this. See [issue #17](https://github.com/jsarman/coordin8/issues/17) and `.claude/plans/space-race-txn-failsafe/session-1-complete.md` for the full trace.

### 6. gRPC security via JWT

Add JWT auth to the gRPC surface, plus a broader platform security discussion — not covered anywhere in existing plans. Open questions: token issuance/rotation, which services validate tokens (every service vs. gateway/proxy only), service-to-service vs. client-to-service auth, mTLS vs. JWT-over-TLS, interaction with Registry self-registration (does a service need a valid identity before it's allowed to register?).

## Non-Goals

- Full AWS serverless topology (Lambda/Fargate/SNS/SQS/DynamoDB Streams) — that's `phase3-cloud-topology`, comes after this.
- Rewriting Space's `watch()` to be durable in-place — item 5's real fix is routing through EventMgr or, longer-term, the broadcast-externalization plan already scoped in `phase3-cloud-topology/resilience-plan.md`.

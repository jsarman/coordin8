# Space Race + Txn Vote Fail-Safe — Session 1 Completion Notes

**Merged to main:** 2026-09-05 (PR [#16](https://github.com/jsarman/coordin8/pull/16), plus two follow-on PRs surfaced during validation: [#18](https://github.com/jsarman/coordin8/pull/18) CI toolchain pin, [#19](https://github.com/jsarman/coordin8/pull/19) local tooling sync)

## What shipped

The patch itself (`djinn/crates/coordin8-space/src/manager.rs`, `djinn/crates/coordin8-txn/src/manager.rs`) — see `PRD.md`. The bulk of this session was validating it thoroughly before merge, which surfaced three unrelated but real findings along the way. Documented here because the validation process is as much "the journey" as the patch.

## Baseline: full stack cycle before touching anything

Session started mid-flight (prior context lost). Recovered by: no other Claude session running (`ListAgents`), found a live `tmux` session `coordin8` (windows: `main`, `djinn`, `greeter-go`) already up from earlier in the day. Stopped it cleanly (greeter released its lease, Djinn exited on SIGINT), verified ports 9001-9006/50051 clear, restarted. `mise r test` (Rust + Go SDK) and all four examples (hello-coordin8 x3 SDKs, market-watch, double-entry) run clean pre-patch — this became the regression baseline.

## Patch applied, retested

Applied cleanly (`git apply --check` first). Rebuilt, reran the same full suite — identical pass results, no regression. hello-coordin8/market-watch/double-entry rerun live against the patched local Djinn.

## Docker Desktop was broken — full detour to fix it

auction-house (the fourth example, Docker Compose, not yet exercised) needs Docker. `docker compose up --build` failed: `permission denied ... docker API`. Root cause: `/var/run/docker.sock` symlinked to `/Users/angiesarman/.docker/run/docker.sock` — a *different* macOS user account's Docker Desktop, version 4.78.0, installed manually (not brew-tracked).

- Ran Docker's own uninstaller (`/Applications/Docker.app/Contents/MacOS/uninstall`) — failed with `operation not permitted` on `~/Library/Containers/com.docker.docker/...`. Root cause: **Full Disk Access (TCC)** not granted to the calling app (first VS Code, since that's what hosts this session's shell; then Terminal.app, since the user ran a manual cleanup command from there). Both needed the Full Disk Access grant + full app restart before file removal worked.
- Verified full cleanup (no `/Applications/Docker.app`, no CLI, no privileged helper, no LaunchAgents) but found two root-level leftovers: `/Library/LaunchDaemons/com.docker.socket.plist` (a daemon that just re-`ln -sf`'d the stale symlink at every boot) and the `~/Library/Containers/com.docker.docker` sandbox dir. Removed both.
- Reinstalled via `brew install --cask docker-desktop` (4.89.0) instead of the vendor installer, so it's brew-tracked for future upgrades. Hit one more snag: brew needed `sudo mkdir -p /usr/local/cli-plugins` on first run (no interactive password available in this shell) — user ran that once manually, then the brew install succeeded.
- Launched, first-run setup done interactively by the user (privileged helper password prompt). Confirmed working under the right account (`docker info` — engine 29.7.2, no more `/var/run/docker.sock` dependency on the old symlink scheme).

## auction-house: full live walkthrough

Built (from the patched working tree — this doubles as a test of the patch in the containerized path) and brought up via `docker compose up --build`. All four services (`djinn`, `auction-service`, `settlement-engine`, `auction-board`) up, Djinn healthy.

Walked the user through the browser at `localhost:3000`: created an auction via curl, user placed a bid via the board UI, watched the lease expire, watched Settlement Engine react and the board flip to "SOLD" live via SSE — the full "absence is a signal" flow working end-to-end, screenshotted and confirmed.

### Audit-trail check surfaced a real (separate) durability gap

`coordin8 space contents --match type=sale` returned nothing at first — needed to build the CLI (`cli/cmd/coordin8/`, not yet built) first. Once built, still returned nothing for the original sale. Traced through:

1. Ruled out TTL=0 being mishandled as "expire immediately" — confirmed `LEASE_FOREVER = 0` in `coordin8-core/src/lease.rs` correctly negotiates to `MAX_LEASE_TTL` (default 3600s) when no `MAX_LEASE_TTL=forever` env var is set.
2. But the Djinn container had genuinely been up **6 hours** (this whole Docker Desktop detour took a while in wall-clock time) — so the original "permanent" sale tuple actually had expired, capped at the default 1-hour `MAX_LEASE_TTL`, since auction-house's `docker-compose.yml` never sets `MAX_LEASE_TTL=forever`. Minor, separate finding: the settlement-engine code comment ("TTL 0 means FOREVER") is only true if that env var is set, which it isn't here. Not filed as its own issue yet — worth a follow-up.
3. Confirmed the CLI itself works fine (write/read round-trip test tuple) and Djinn never restarted (`RestartCount=0`) — ruled out a connectivity or data-loss bug.
4. Created a fresh short-TTL auction, bid, and confirmed the audit trail command works correctly within the live window — 2 sale tuples returned including one from the "Bigg Pappah" auction the user ran independently.

### Settlement-engine restart durability test — found the real gap

Per auction-house's README ("kill the settlement engine... settlement happens on restart — expiry events are durable"): stopped `settlement-engine`, created a short auction, let it fully expire while the watcher was down, restarted `settlement-engine`. **It never caught the missed expiry** — no sale tuple, no log line.

Root-caused to `coordin8-space/src/manager.rs:396-400` (`on_tuple_expired`): expiry is pushed onto a live `tokio::broadcast` channel with no backing mailbox, and the expired tuple is hard-deleted from the store — nothing left to reconcile against for a late subscriber. Confirmed this is *not* a regression from the patch (untouched code path) and *not* explained by the in-memory provider (Djinn itself never restarted).

Checked this against the actual Jini Distributed Events spec (Apache River reference docs) via WebFetch — confirmed Space's `watch()`/`Notify()` (which maps directly to JavaSpace's `notify()`, `coordin8-design-napkin.md:114`) is behaving exactly as that 1999 spec defines: best-effort, no guaranteed delivery, no retroactive delivery on reconnect, verbatim: *"a distributed event cannot be guaranteed to be delivered in a timely fashion... may be delayed indefinitely and even lost"*. So this isn't a platform bug — Coordin8's own design already anticipated this with a two-tier model (EventMgr = durable mailbox, `coordin8-design-napkin.md:143-155`; Space = Jini-faithful best-effort). The auction-house example just used the wrong tier for a job that needs durability, and its README oversold it.

Filed as [issue #17](https://github.com/jsarman/coordin8/issues/17), with both fix options (correct the README, or rework settlement-engine to use EventMgr's durable subscribe — the architecturally "right" fix). Not blocking PR #16 — logged for the hardening roadmap instead (see `.claude/plans/hardening-roadmap/PRD.md`).

## PR #16 CI failure — unrelated toolchain drift, fixed separately

Opened PR #16, Rust CI check failed on Clippy's `result_large_err` lint against **generated** `coordin8-proto` code (tonic's `Result<_, tonic::Status>`, 176 bytes) — nothing the patch touches. Root cause: `.github/workflows/ci.yml` used unpinned `dtolnay/rust-toolchain@stable`; CI hadn't run since April 2026, silently drifted to Rust 1.98.0 by the time it next ran, while `.mise.toml` (and all local dev) pins `1.94.1`. Confirmed empirically: `cargo clippy --all -- -D warnings` passes clean locally on 1.94.1.

Fixed via a separate, standalone PR [#18](https://github.com/jsarman/coordin8/pull/18) (pin CI to 1.94.1), merged first, then #16 rebased onto the fix and went green.

## Local tooling drift synced (PR #19)

Working tree had carried uncommitted local changes since before this session (from the user's earlier brew/mise/tool-install work): `.mise.toml` (rust 1.93→1.94.1, plus `CGO_ENABLED=0` — the actual fix for the "dyld: missing LC_UUID load command" crash observed firsthand on the Go greeter-service binary earlier this session), Java gradle-wrapper self-regeneration, and a Node lockfile correction (a stale locked path for the local `@coordin8/sdk` link, fixed to match `package.json`'s already-correct `file:../../../sdks/node`). Synced as PR #19 rather than left to rot.

## Also fixed along the way

- `gh` CLI wasn't installed — installed via brew, authenticated interactively.
- Git had no configured `user.name`/`user.email` at all — set globally to match the GitHub account (`johnsarman@gmail.com`) so commits attribute correctly; amended the first patch commit.

## DynamoDB vs InMemory provider — reviewed, not yet fixed

While PR #16's CI ran, reviewed the provider abstraction per the user's question ("we abstracted the memory layer, right?"). Confirmed: the trait layer (`LeaseStore`/`RegistryStore`/`EventStore`/`TxnStore`/`SpaceStore` in `coordin8-core`, implemented by both `providers/local` and `providers/dynamo`) is genuinely solid. The gap is purely in `djinn/crates/coordin8-djinn/src/services.rs`: `run_all()` branches on `COORDIN8_PROVIDER` (lines 63-118); every split-mode function — `run_registry_on_listener` (:319,:321), `run_lease_on_listener_with_shutdown` (:430), `run_event_on_listener` (:562), `run_space_on_listener` (:694), `run_txn_on_listener` (:847) — hardcodes `Arc::new(InMemory*Store::new())` directly with no such branch. `run_proxy` needs nothing (stateless). This matches the master PRD's existing "DynamoDB/MiniStack provider-swap test — Gap" line under Djinn Split Mode, now with exact fix locations. Feeds directly into `.claude/plans/hardening-roadmap/PRD.md` item 4.

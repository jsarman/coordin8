# Leasing: Jini's Model vs Coordin8's Implementation

## The root difference, in one sentence

In Jini, **a lease is an object held by the client, and every service is its own lease grantor**. In Coordin8, **a lease is a string ID held by the client, and one central service grants all leases for everyone**.

Almost every discrepancy below falls out of that single inversion.

---

## Side by side

| Aspect | Jini | Coordin8 |
|---|---|---|
| What the holder gets back | A `Lease` proxy object (grantor endpoint + cookie + expiration baked in) | A `lease_id` string + `expires_at` timestamp |
| Who grants | Each service, implementing `Landlord` itself | One `LeaseMgr` service at `:9001` |
| How you renew | `lease.renew(duration)` — holder needn't know who granted it | `LeaseService.Renew(lease_id)` — holder must know the LeaseMgr endpoint |
| Renew returns | New **duration** granted | Absolute **expires_at** timestamp |
| Which resource a lease covers | Implicit — the grantor *is* the service | String prefix convention: `registry:`, `space:`, `txn:`, `event:`, `space-watch:` |
| Lease policy | Per-grantor (`LeasePeriodPolicy`, e.g. `FixedLeasePeriodPolicy(default, max)`) | One global `LeaseConfig{max_ttl, preferred_ttl}` for every resource type |
| Batch renewal | `LeaseMap` / `canBatch` / `Landlord.renewAll` + `cancelAll` | None — one unary RPC per lease |
| Client-side renewal | `LeaseRenewalManager` with `LeaseListener` failure callbacks | `KeepAlive()` goroutine, silent return on failure |
| Third-party renewal | `LeaseRenewalService` ("Norm") — itself leased | None |
| Expiry visibility | Passive; surfaces as service-level events (e.g. lookup emits `TRANSITION_MATCH_NOMATCH`) | First-class `WatchExpiry` stream on LeaseService |
| Renew on dead lease | `UnknownLeaseException` / `LeaseDeniedException` | `LeaseNotFound` / `LeaseExpired` (which one you get is a race) |
| `FOREVER` sentinel | `Long.MAX_VALUE` | `0` |
| `ANY` sentinel | `-1` | `u64::MAX` |

---

## Where Jini's design was load-bearing and the difference bites

### 1. The lease is not self-describing

Jini's `LandlordLease(cookie, landlord, landlordUuid, expiration)` carries a reference to its grantor. That is why a Jini lease can be **handed to another process** — passed to a `LeaseRenewalManager`, deposited in a `LeaseRenewalSet` on Norm, stored and renewed by a supervisor that has never spoken to the grantor before. The holder renews without knowing, or caring, who the landlord is.

Coordin8's `Lease` message is `{lease_id, resource_id, granted_at, expires_at, ttl_seconds}`. There is no grantor identity in it. To renew, you must already hold a connection to the right LeaseMgr. That means:

- A lease can't be delegated. Handing `lease_id` to another service is useless unless you also hand over out-of-band knowledge of which Djinn granted it.
- In split mode (where several Djinn processes each run their own LeaseMgr for their namespace), a bare `lease_id` is genuinely ambiguous.
- No Norm-equivalent is buildable on the current wire format.

**Fix:** add `grantor_endpoint` (and ideally a `grantor_uuid`) to the `Lease` proto message. It is a small change now and an impossible one after you have users. This is the highest-leverage single change on this list.

### 2. `LeaseMgr` as a network service is the biggest structural divergence

Jini has no lease service. `com.sun.jini.landlord` is a **library**, not a daemon — `Landlord`, `LeasedResource`, `LeasePeriodPolicy`, `LandlordLeaseFactory`. The lookup service, JavaSpaces, Mahalo, and the event mailbox each implement `Landlord` in-process. Leasing was a *pattern* plus shared code, never a network dependency.

Coordin8 promoted it to Layer 1 bedrock that all four other services call over gRPC. Consequences:

- **SPOF and latency.** Every tuple `write` costs a `Grant` round-trip before it touches the store. Jini's `JavaSpace.write` allocated its lease locally.
- **Prefix strings replace grantor identity.** `resource_id.starts_with("space:")` is doing the job `Landlord` identity did structurally. It's stringly-typed, and the cascade tasks in `services.rs` each re-parse the same convention.
- **One policy for all resource types.** `LeaseConfig{max_ttl: 3600, preferred_ttl: 300}` is essentially `FixedLeasePeriodPolicy(default, maximum)` — good convergence — but Jini attached that policy *per grantor*. A tuple lease, a 2PC transaction lease, and a service registration have completely different natural bounds. Right now a txn can hold a lease for an hour by default.

Not necessarily wrong — a shared, observable LeaseMgr is a real design choice with real benefits (see "what you improved on"). But it should be a deliberate choice, documented as a departure, with per-namespace policy added.

### 3. Absolute expiry over the wire invites clock skew

Jini's `Landlord.renew(cookie, duration)` returns a **duration**; `AbstractLease` then converts it to absolute time *in the holder's local clock*. The `DURATION` vs `ABSOLUTE` serial formats exist precisely because the Jini team knew absolute timestamps crossing a machine boundary are a hazard.

Coordin8 returns `expires_at` as a server-side `Utc::now() + ttl` timestamp. A client with a skewed clock that computes "renew when `expires_at` is near" will renew too late or pointlessly early. Your Go `KeepAlive` dodges this by ticking at `ttl/2` rather than trusting `expires_at` — correct, but incidental. The proto still hands every SDK author the footgun.

**Fix:** treat `ttl_seconds` as the authoritative return value and document `expires_at` as advisory/observational only. Renewal scheduling should derive from the granted duration.

### 4. No batch renewal — this will hurt the Space first

`LeaseMap`, `canBatch(Lease)`, `Landlord.renewAll(cookies[], durations[])` returning `RenewResults`, and `cancelAll`. Sun built this because a JavaSpace holding thousands of leased entries cannot renew them one RPC at a time.

Coordin8 has `Renew(lease_id)` only. A Space with 10k live tuples on 30s TTLs needs ~660 renewal RPCs/sec against a single LeaseMgr. Combined with the DynamoDB `scan_all` in `take_match`, this is where the system will fall over first under real load.

**Fix:** add `RenewAll(repeated {lease_id, ttl})` returning per-lease results (granted TTL or error). Straightforward addition; mirrors `Landlord.RenewResults` exactly.

### 5. `KeepAlive` swallows renewal failure — the dangerous one

`LeaseRenewalManager.renewUntil(lease, expiration, listener)` calls back via `LeaseRenewalEvent` when renewal *fails*, and distinguishes "reached the expiration you asked for" from "grantor denied renewal" from "network is down." The holder always learns that it has lost the resource.

Coordin8's Go `KeepAlive`:

```go
if _, err := c.Renew(ctx, leaseID, ttl); err != nil {
    return
}
```

It returns silently. The service goes on serving traffic, believing it is registered, while the Registry has already dropped it. Anything relying on "stop renewing → I disappear" now has a case where **the process didn't stop renewing, renewal just failed, and nobody was told**. That inverts the core promise in the README.

**Fix:** `KeepAlive` should return an error channel or take an `OnLeaseLost` callback, and distinguish a transient RPC failure (retry with backoff) from `LeaseExpired`/`LeaseNotFound` (give up, notify). Same for the Java and Node SDKs.

---

## Three concrete bugs that fall directly out of the difference

### A. Cancel bypasses the cascade — orphans that can never expire

The cascade is driven exclusively by the reaper's expiry broadcast. `LeaseManager::cancel` calls `store.cancel()`, which removes the lease record and broadcasts nothing.

So: client calls `LeaseService.Cancel(lease_id)` on a registry lease → the lease is gone → `unregister_by_lease` never runs → **the registry entry remains, now with no lease at all, so it can never expire.** It is immortal. And `registry.proto` has no `Unregister` RPC, so there is no way to remove it.

In Jini this failure mode cannot exist: the lookup service *is* the landlord, so `cancel(cookie)` reclaims the registration directly. Splitting the landlord from the resource owner created the gap.

**Fix:** broadcast a reclamation event on cancel too (an `ExpiryEvent` with a `reason` field distinguishing `EXPIRED` from `CANCELLED`), and have the cascade tasks handle both. Jini's distinction between cancel and expiry is about *promptness and intent*, not about whether cleanup happens — cleanup happens either way.

### B. The cascade tasks die permanently on broadcast lag

All four cleanup tasks in `services.rs` are shaped:

```rust
while let Ok(lease) = registry_expiry_rx.recv().await {
```

`broadcast::Receiver::recv()` returns `Err(RecvError::Lagged(n))` when a slow subscriber falls behind the 256-slot channel. `while let Ok(..)` treats that as loop termination — the spawned task **exits and is never restarted**. The registry, event, txn, and space cleanup cascades are all silently dead for the remaining life of the process.

Trigger: any reaper sweep that expires more than the channel's headroom at once — e.g. a burst of tuples written with the same short TTL, exactly the Space's normal workload. This is the same lag-handling class of bug as the `read()` fix in the last patch, but with a worse blast radius: it takes out lease-driven cleanup entirely, which is the mechanism the whole system rests on.

**Fix:** match on the error explicitly — `continue` on `Lagged` (and reconcile, since you may have missed expiries: re-scan the relevant store for records whose lease is gone), `break` only on `Closed`. Consider raising the channel capacity and adding a lag counter metric.

### C. `FOREVER = 0` is a proto3 footgun

`LEASE_FOREVER = 0` collides with proto3's default value for an unset `uint64`. A client that forgets to set `ttl_seconds` — or an SDK with a zero-valued struct field — requests an **eternal lease**, not the server's preferred TTL. `LeaseConfig::negotiate` caps it to `max_ttl` when a cap is set, which limits the damage, but with `MAX_LEASE_TTL=forever` the mistake is unbounded, and `is_expired()` returns `false` unconditionally for `ttl_seconds == 0`, so the reaper can never reclaim it.

Jini avoided this by construction: `FOREVER = Long.MAX_VALUE` and `ANY = -1` are both implausible as accidental defaults.

**Fix:** swap the sentinels. `0` (the accidental default) should mean `ANY` → server's preferred TTL, which is the safe outcome. Use `u64::MAX` for `FOREVER`. This is a breaking wire change, so it's cheaper now than later.

---

## Where you improved on Jini — keep these

- **Expiry as a first-class observable event.** `WatchExpiry` streaming from the lease layer is better than Jini, where expiry was passive and you inferred it from service-level events. The README line — "absence is not an error, it's a coordination event" — is a genuine articulation of something Jini did implicitly. (Just fix the lag bug in B so the mechanism is trustworthy.)
- **Distinguishing `LeaseNotFound` from `LeaseExpired`.** More informative than Jini collapsing both into `UnknownLeaseException`. One caveat: which one a caller gets is currently a race against the reaper sweep — renew just before the sweep gives `LeaseExpired`, just after gives `LeaseNotFound`, for the identical logical condition. Either make the mapping deterministic (keep expired records as tombstones for a grace period) or document that clients must treat both identically.
- **Polyglot by construction.** Jini leases were Java-serialized proxies — the model was structurally unexportable from the JVM. Encoding the lease as data over protobuf is what makes the Go/Node/Java story possible at all. The cost is exactly point 1 (the lease is no longer self-describing); the fix there is to put the grantor's *address* in the message, recovering the useful half of the proxy without the serialization coupling.
- **`LeaseConfig` mirrors `FixedLeasePeriodPolicy`** almost exactly. You reinvented it correctly. Just make it per-namespace.

---

## Suggested order of work

1. **Cascade lag bug (B)** — silent, total loss of lease-driven cleanup. Fix first.
2. **Cancel-bypasses-cascade (A)** — creates permanently un-reclaimable records.
3. **Sentinel swap (C)** — breaking wire change; cheapest today.
4. **`grantor_endpoint` in the `Lease` message (1)** — also a wire change; unblocks delegation and any future Norm.
5. **`KeepAlive` failure surfacing (5)** — correctness hazard in all three SDKs.
6. **`RenewAll` batch renewal (4)** — before the Space meets real volume.
7. **Per-namespace lease policy (2)** — `space:` and `txn:` should not share a 3600s cap.
8. **Document duration-as-authoritative, expiry-as-advisory (3).**

Items 3 and 4 are both breaking proto changes — worth landing together in one version bump.

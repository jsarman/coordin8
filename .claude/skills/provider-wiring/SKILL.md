---
name: provider-wiring
description: How storage providers (InMemory vs DynamoDB) are selected and wired into the Djinn at runtime via COORDIN8_PROVIDER and the *_store_from_env() functions in coordin8-djinn/src/services.rs. Use when adding a new provider backend, adding or renaming a store or Dynamo table, changing per-service lease-table namespacing, or debugging which backend a Djinn service booted with.
---

# provider-wiring

How storage providers are wired into the Djinn. Load this skill when adding a provider backend, adding a store, or touching provider selection.

## Current Design (already implemented)

Provider selection is a **runtime** choice, not compile-time and not hard-coded in `main.rs`. `main.rs` only parses the subcommand; all wiring lives in `djinn/crates/coordin8-djinn/src/services.rs`.

- `provider_from_env()` reads `COORDIN8_PROVIDER` (default `local`). `dynamo` selects DynamoDB. **Any other value silently falls through to `local`** (the `_` match arm) — it never panics on an unknown name.
- One factory per store, each a `match provider_from_env()`:

| Factory | Returns | Dynamo type |
|---------|---------|-------------|
| `registry_store_from_env()` | `Arc<dyn RegistryStore>` | `DynamoRegistryStore::new(client)` |
| `event_store_from_env()` | `Arc<dyn EventStore>` | `DynamoEventStore::new(client)` |
| `txn_store_from_env()` | `Arc<dyn TxnStore>` | `DynamoTxnStore::new(client)` |
| `space_store_from_env()` | `Arc<dyn SpaceStore>` | `DynamoSpaceStore::new(client)` |
| `lease_store_from_env(namespace)` | `Arc<dyn LeaseStore>` | `DynamoLeaseStore::with_table(client, "coordin8_leases_{namespace}")` |

- Every Dynamo branch does `make_dynamo_client().await`, constructs the store, then `store.init().await?` before use (boot order: provider init completes before any manager starts), and logs `✓ Provider: ... — <Store>`.
- **Leasing is distributed**: `embedded_landlord(namespace, host, port, auth)` builds each service's own `LeaseManager` + reaper + `LeaseService`, calling `lease_store_from_env(namespace)`. Namespaces are `registry`, `event`, `txn`, `space`, giving the Dynamo tables `coordin8_leases_registry`, `coordin8_leases_event`, `coordin8_leases_txn`, `coordin8_leases_space`. They never share lease state. There is no shared/central lease table.
- The same factories serve bundled mode (`run_all()`) and every split-mode service (`run_registry`, `run_event`, `run_space`, `run_txn`, and their `*_on_listener` variants), so `COORDIN8_PROVIDER=dynamo` behaves identically in both. `run_proxy` has no store of its own: bundled mode hands `LocalCapabilityResolver` the registry store; split mode uses `RemoteCapabilityResolver` through Registry.
- Managers take trait objects: `RegistryIndex::new(Arc<dyn RegistryStore>)`, `LeaseManager::new(store, config, expiry_tx)`, etc. Bundled mode passes the **same** `registry_store` Arc to both `RegistryIndex` and the proxy's `LocalCapabilityResolver`.
- `coordin8-djinn/Cargo.toml` depends on both `coordin8-provider-local` and `coordin8-provider-dynamo` unconditionally; the Docker image is one binary for both.

## Dynamo Environment

| Var | Purpose |
|-----|---------|
| `COORDIN8_PROVIDER` | `local` (default) or `dynamo` |
| `DYNAMODB_ENDPOINT` | Endpoint override (MiniStack: `http://localhost:4566`, in compose `http://ministack:4566`); unset = real AWS credential chain |
| `COORDIN8_AUTO_CREATE_TABLES` | `true`/`1` makes every store's `init()` create its tables (dev/tests). Unset = assume tables exist (production: CloudFormation) |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_DEFAULT_REGION` | Standard AWS config; MiniStack accepts `test`/`test`/`us-east-1` |

Tables are defined in `djinn/providers/dynamo/src/table.rs` (names as consts) and, for production/compose, `infra/dynamodb-tables.cfn.yml` (12 tables; `docker-compose.yml`'s `cfn-init` deploys them to MiniStack before Djinn starts, regardless of provider). Table names: `coordin8_registry`, `coordin8_txn`, `coordin8_event_subscriptions`, `coordin8_event_mailbox`, `coordin8_space`, `coordin8_space_uncommitted`, `coordin8_space_txn_taken`, `coordin8_space_watches`, plus the four `coordin8_leases_*`. **If you add or rename a table, update `table.rs`, the CFN template, and the `init()` of the store.**

## Adding a New Store or Backend

1. Add the trait to `coordin8-core` and re-export it in `lib.rs`.
2. Implement it in BOTH `providers/local` and `providers/dynamo`, with identical behavior (see the per-store skills: `lease-provider`, `registry-provider`, `event-provider`, `space-provider`, `txn-provider`).
3. Add a `<store>_store_from_env()` factory next to the others in `services.rs` and call it from `run_all()` and the matching `run_*_on_listener()`.
4. Add the table(s) to `table.rs` + `infra/dynamodb-tables.cfn.yml`.
5. For a whole new backend (not DynamoDB), add a new arm to each factory's `match`, a new provider crate under `djinn/providers/`, and register it in the `djinn/Cargo.toml` workspace members.

## Important Constraints

- **Boot order is sacred.** Provider `init()` must finish before the corresponding manager starts.
- **Default is always `local`.** Unset or unrecognized `COORDIN8_PROVIDER` means InMemory.
- **Per-service lease namespaces.** Never point two services at the same lease table.
- **InMemory state dies with the process.** Anything that keeps in-memory counters beside a durable store (event sequence numbers, etc.) must reconcile on startup.

## Verification

1. `cd djinn && cargo build --all && cargo test --all` (CI-equivalent; Dynamo tests are `#[ignore]` and do NOT run).
2. `mise r djinn` with no env: boots with `Provider: local (in-memory)` log lines for all stores.
3. MiniStack-backed: `docker compose up -d ministack`, then run `mise r djinn` with `COORDIN8_PROVIDER=dynamo DYNAMODB_ENDPOINT=http://localhost:4566 COORDIN8_AUTO_CREATE_TABLES=true AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test AWS_DEFAULT_REGION=us-east-1` set — logs should say `Provider: dynamo (DynamoDB)` per store.
4. Provider tests: `cd djinn && cargo test -p coordin8-provider-dynamo -- --ignored` (needs MiniStack on `:4566`).
5. Full Dockerized: `COORDIN8_PROVIDER=dynamo mise r up` (compose passes the var through; default `local`).

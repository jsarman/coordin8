# coordin8-provider-dynamo

DynamoDB-backed provider. Implements every `*Store` trait from [`coordin8-core`](../../crates/coordin8-core/README.md) against AWS DynamoDB (or MiniStack for dev). State survives restarts.

## Where It Fits

Selected by the Djinn binary when `COORDIN8_PROVIDER=dynamo`. The binary calls `init()` on each store at boot — `init()` either creates the table (if `COORDIN8_AUTO_CREATE_TABLES=true`) or assumes it already exists. **In production the tables are provisioned by CloudFormation** (see [`../../../infra/`](../../../infra/README.md) and the `cfn-init` service in `docker-compose.yml`).

## Tables

Nine tables, all `PAY_PER_REQUEST`. Only `coordin8_leases` has DynamoDB TTL enabled.

| Table                          | Hash key          | Range key  | GSI                  |
|--------------------------------|-------------------|------------|----------------------|
| `coordin8_leases`              | `lease_id`        | —          | `resource_id-index`  |
| `coordin8_registry`            | `capability_id`   | —          | `lease_id-index`     |
| `coordin8_txn`                 | `txn_id`          | —          | —                    |
| `coordin8_event_subscriptions` | `registration_id` | —          | `lease_id-index`     |
| `coordin8_event_mailbox`       | `registration_id` | `seq_num`  | —                    |
| `coordin8_space`               | `tuple_id`        | —          | `lease_id-index`     |
| `coordin8_space_uncommitted`   | `txn_id`          | `tuple_id` | —                    |
| `coordin8_space_txn_taken`     | `txn_id`          | `tuple_id` | —                    |
| `coordin8_space_watches`       | `watch_id`        | —          | `lease_id-index`     |

The authoritative shape lives in [`infra/dynamodb-tables.cfn.yml`](../../../infra/dynamodb-tables.cfn.yml). The `table.rs` helpers (`ensure_*_table`) match that shape and exist for `auto_create_enabled()` mode.

## Layout

```
src/
  lib.rs               feature flag (auto_create_enabled) + re-exports
  client.rs            make_dynamo_client — honors DYNAMODB_ENDPOINT for MiniStack
  table.rs             ensure_*_table helpers used when auto-create is on
  lease_store.rs       DynamoLeaseStore
  registry_store.rs    DynamoRegistryStore
  event_store.rs       DynamoEventStore
  txn_store.rs         DynamoTxnStore
  space_store.rs       DynamoSpaceStore
```

## Configuration

| Env var                       | Default | Purpose |
|-------------------------------|---------|---------|
| `COORDIN8_PROVIDER`           | `local` | set to `dynamo` to select this provider |
| `COORDIN8_AUTO_CREATE_TABLES` | unset   | `true`/`1` → stores create their tables on init; otherwise tables must already exist |
| `DYNAMODB_ENDPOINT`           | unset   | endpoint override (e.g. `http://ministack:4566` for MiniStack) |
| `AWS_REGION`, `AWS_ACCESS_KEY_ID`, etc. | — | standard AWS credential chain |

## Build / Test

```bash
cargo build -p coordin8-provider-dynamo
cargo test  -p coordin8-provider-dynamo
```

Integration tests assume MiniStack is running (`mise r stack-up` or `docker compose up ministack`).

## Notes

- `auto_create_enabled()` is opt-in by design — production runs through CloudFormation so the schema can evolve under change control
- The `cfn-init` compose service deploys the template into MiniStack before the Djinn starts, so `docker compose up` works regardless of provider
- DynamoDB TTL is eventual — the Djinn's reaper still drives lease expiry semantics; the TTL attribute is a backstop for cleanup

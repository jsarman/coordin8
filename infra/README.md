# infra

CloudFormation provisioning for the Coordin8 DynamoDB provider.

## What's Here

```
dynamodb-tables.cfn.yml      defines all 12 DynamoDB tables used by coordin8-provider-dynamo
```

This template is the **authoritative schema** for the DynamoDB provider in production. The matching `ensure_*_table` helpers in [`../djinn/providers/dynamo/`](../djinn/providers/dynamo/README.md) only run when `COORDIN8_AUTO_CREATE_TABLES=true` and exist for local dev / integration tests; in any environment that ships to AWS, deploy this template instead.

## Tables

All `PAY_PER_REQUEST`. The four `coordin8_leases_*` tables have DynamoDB TTL enabled.

Leasing is distributed — there's no standalone LeaseMgr, so there's no single shared leases table either. Registry, EventMgr, Space, and TransactionMgr each own their own lease table, namespaced by service (`coordin8-djinn/src/services.rs`'s `lease_store_from_env()` builds `coordin8_leases_{namespace}` at runtime — keep this list in sync with that function if a new leasing service is ever added).

| Table                          | Owner          |
|--------------------------------|----------------|
| `coordin8_leases_registry`     | Registry       |
| `coordin8_leases_event`        | EventMgr       |
| `coordin8_leases_space`        | Space          |
| `coordin8_leases_txn`          | TransactionMgr |
| `coordin8_registry`            | Registry       |
| `coordin8_txn`                 | TransactionMgr |
| `coordin8_event_subscriptions` | EventMgr       |
| `coordin8_event_mailbox`       | EventMgr       |
| `coordin8_space`               | Space          |
| `coordin8_space_uncommitted`   | Space          |
| `coordin8_space_txn_taken`     | Space          |
| `coordin8_space_watches`       | Space          |

## Deploy

### MiniStack (via docker compose)

The `cfn-init` service in [`../docker-compose.yml`](../docker-compose.yml) deploys this template into MiniStack before the Djinn starts. It runs unconditionally — harmless when `COORDIN8_PROVIDER=local` since the tables sit unused.

```bash
docker compose up --build
```

### MiniStack (manual)

```bash
aws cloudformation deploy \
  --template-file infra/dynamodb-tables.cfn.yml \
  --stack-name coordin8-tables \
  --endpoint-url http://localhost:4566 \
  --no-fail-on-empty-changeset
```

### Real AWS

```bash
aws cloudformation deploy \
  --template-file infra/dynamodb-tables.cfn.yml \
  --stack-name coordin8-tables \
  --no-fail-on-empty-changeset
```

Then start the Djinn with:

```bash
COORDIN8_PROVIDER=dynamo cargo run -p coordin8-djinn
```

`COORDIN8_AUTO_CREATE_TABLES` should remain unset — the stores will assume the tables exist.

## Notes

- Schema changes go here first, then the `ensure_*_table` helpers in `coordin8-provider-dynamo/src/table.rs` are updated to match
- The `cfn-init` compose service uses `--no-fail-on-empty-changeset` so re-running `docker compose up` is idempotent

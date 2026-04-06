# MiniStack Migration & CloudFormation Table Management

## Context

LocalStack Community Edition went paid ($39/mo) on March 23, 2026. We spiked MiniStack as a replacement on branch `ministack-replacement` (PR #1). Results:

- **Drop-in replacement** — same port (4566), all 36 DynamoDB integration tests pass
- **5x smaller image**, MIT license, no telemetry, no API key
- **CloudFormation support included free** — LocalStack gated this behind Pro

## Goals

### Phase 1: MiniStack Swap (PR #1 — done)
Replace LocalStack with MiniStack across compose, skills, agents, and test annotations. All integration tests green.

### Phase 2: CloudFormation Table Provisioning
Separate table schema from application code. Production-grade infra pattern.

**Why:** `table.rs` currently runs `create_table` for all 9 tables on every Djinn boot. In production, app code should never have CreateTable/DeleteTable permissions. Schema should be owned by infra tooling.

**What:**
1. **CloudFormation template** (`infra/dynamodb-tables.cfn.yml`) — single source of truth for all 9 tables + GSIs
2. **Stack-up deploys CF first** — `aws cloudformation deploy` against MiniStack before Djinn boots
3. **`table.rs` becomes conditional** — behind a `COORDIN8_AUTO_CREATE_TABLES=true` env var (default false). Dev convenience, off in prod.
4. **Validate CF template against MiniStack** — ensure all table features we use (GSIs, KeyConditionExpression patterns, conditional writes) work through CF-provisioned tables

### Phase 3: CI Integration (future)
- Same CF template deploys to real AWS in CI/staging
- Drift detection between CF template and `table.rs` definitions
- One template, three tiers: MiniStack -> staging -> prod

## Table Inventory (from table.rs)

| Table | PK | SK | GSIs | Used By |
|-------|----|----|------|---------|
| coordin8_leases | lease_id | — | resource_id-index | LeaseMgr |
| coordin8_capabilities | capability_id | — | lease_id-index | Registry |
| coordin8_subscriptions | registration_id | — | lease_id-index | EventMgr |
| coordin8_mailbox | registration_id | seq_num | — | EventMgr |
| coordin8_transactions | txn_id | — | — | TransactionMgr |
| coordin8_tuples | tuple_id | — | lease_id-index | Space |
| coordin8_uncommitted | txn_id | tuple_id | — | Space (2PC) |
| coordin8_txn_taken | txn_id | tuple_id | — | Space (2PC) |
| coordin8_watches | watch_id | — | lease_id-index | Space |

## Design Constraints

- CF template must be deployable to both MiniStack and real AWS without modification
- `table.rs` definitions are the current source of truth — extract, don't rewrite from scratch
- Boot order: CF deploy -> MiniStack healthy -> Djinn starts -> services register
- The `COORDIN8_AUTO_CREATE_TABLES` escape hatch keeps `cargo test --ignored` working without CF

## Success Criteria

- `docker compose up` with `COORDIN8_PROVIDER=dynamo` provisions tables via CF, Djinn boots clean
- All 36 integration tests pass against CF-provisioned tables
- `table.rs` create logic only runs when `COORDIN8_AUTO_CREATE_TABLES=true`
- CF template validates against `aws cloudformation validate-template`

## Open Questions

- Should we use CDK instead of raw CF? (Probably not — raw CF is simpler, no synth step)
- Init container vs. sidecar vs. compose entrypoint script for CF deploy?
- Do we want a `make infra` / `mise r infra` task for manual CF deploys?

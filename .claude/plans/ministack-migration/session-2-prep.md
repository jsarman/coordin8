# Session 2 Prep — CloudFormation Table Provisioning

**Goal:** Extract the 9 DynamoDB table definitions from `table.rs` into a CloudFormation template and deploy it via MiniStack before Djinn boots.

## Pre-requisites

- PR #1 merged (MiniStack swap)
- MiniStack running locally

## Steps

### 1. Extract CF Template from table.rs
Read `djinn/providers/dynamo/src/table.rs` and translate all 9 `create_table` calls into a single `infra/dynamodb-tables.cfn.yml`. Pay close attention to:
- Key schemas (PK, SK types)
- GSI definitions (projection type, key schema)
- BillingMode (PAY_PER_REQUEST for all)

### 2. Make table.rs Conditional
- Gate all `create_table` calls behind `COORDIN8_AUTO_CREATE_TABLES` env var
- Default: `false` (tables must exist)
- When `true`: current behavior (create if not exists)
- Integration tests should set this to `true` in their setup

### 3. Add CF Deploy to Stack-Up
Update `docker-compose.yml` or `stack-up` skill to deploy the CF template against MiniStack before Djinn starts. Options:
- Init container that runs `aws cloudformation deploy`
- Compose entrypoint script
- Stack-up skill runs it as a pre-step

### 4. Validate
- `docker compose up` with `COORDIN8_PROVIDER=dynamo` — tables created by CF, Djinn boots without creating tables
- All 36 integration tests still pass (with `COORDIN8_AUTO_CREATE_TABLES=true`)
- `aws cloudformation validate-template` passes on the template

## Files to Touch

- `infra/dynamodb-tables.cfn.yml` — NEW, the CF template
- `djinn/providers/dynamo/src/table.rs` — gate behind env var
- `docker-compose.yml` — possibly add init container or entrypoint
- `.claude/skills/stack-up/SKILL.md` — add CF deploy step

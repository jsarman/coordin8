# Session 1 Complete — MiniStack Spike

**Date:** 2026-04-05
**Branch:** `ministack-replacement`
**PR:** jsarman/coordin8#1

## What We Did

- Researched MiniStack as LocalStack replacement (LocalStack free tier killed Mar 2026)
- Swapped compose, skills, agents, Rust test annotations from LocalStack -> MiniStack
- Ran all 36 DynamoDB integration tests — **all pass, 1.2s**
- Stood up full `docker compose up` with `COORDIN8_PROVIDER=dynamo` — Djinn + greeter + MiniStack all healthy

## Gotchas Found

1. **No curl or wget in MiniStack image** — Alpine-based, minimal. Health check uses `python3 -c "import urllib.request; ..."` instead
2. **Stale container names** — `container_name: coordin8-djinn` conflicts if old containers exist from main branch. `docker rm -f` before compose up.

## Decisions

- MiniStack is a go — warrants full replacement
- CloudFormation for table provisioning identified as Phase 2 (MiniStack includes CF free, unlike LocalStack Pro)
- `table.rs` should become conditional, not deleted — keep as dev escape hatch

## What's Not Done

- README / design napkin still reference LocalStack (cosmetic, deferred to merge)
- CloudFormation template not yet written (Phase 2)
- Full e2e test with Go/Java/Node SDK clients against MiniStack stack (low risk — port is identical)

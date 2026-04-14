# Phase 3: Cloud Topology — Concept Doc

**Status:** Thinking out loud. Refine after Phase 2 spec alignment is solid.

**Core idea:** The Djinn is currently a single Rust binary running all services on separate ports. Phase 3 splits these into independently deployable services with cloud-native backing stores. Three deployment topologies, same interfaces, same SDKs.

---

## The Three Topologies

### 1. Monolith Djinn (what we have today)

Single binary, in-memory stores, all ports on one process. Dev, edge, embedded, air-gapped.

```
┌─────────────────────────────────┐
│            Djinn                │
│  Lease  Registry  EventMgr     │
│  Proxy  TxnMgr   Space        │
│      InMemory Provider         │
└─────────────────────────────────┘
```

**When:** Local dev, CI, edge deployments, single-node coordination.

### 2. Containerized Split (ECS/Fargate/EKS/EC2)

Each service is its own container. Shared backing store (DynamoDB, Redis, SQS). Service mesh or internal ALB for inter-service communication. Still gRPC, still the same proto contracts.

```
┌──────────┐ ┌──────────┐ ┌──────────┐
│ LeaseMgr │ │ Registry │ │ EventMgr │
│  :9001   │ │  :9002   │ │  :9005   │
└────┬─────┘ └────┬─────┘ └────┬─────┘
     │            │            │
     └────────────┼────────────┘
                  │
          ┌───────┴───────┐
          │   DynamoDB /  │
          │  ElastiCache  │
          │     SQS       │
          └───────────────┘

┌──────────┐ ┌──────────┐ ┌──────────┐
│  Proxy   │ │  TxnMgr  │ │  Space   │
│  :9003   │ │  :9004   │ │  :9006   │
└──────────┘ └──────────┘ └──────────┘
```

**When:** Team deployments, staging, production where you want horizontal scaling per service, rolling updates without full restart.

**Container options:**
- **ECS Fargate** — simplest. No cluster management. Each service is a Fargate task. Service discovery via Cloud Map or internal ALB.
- **EKS** — if the org is already on Kubernetes. Helm chart per service. Service mesh (Istio/Linkerd) for mTLS between services.
- **EC2 + Docker Compose** — cheapest. Single EC2 instance running the split containers. Good for small teams that want the split topology without the orchestration overhead.
- **ECS on EC2** — middle ground. You manage the instances but ECS handles placement and health.

**Key concern:** Inter-service latency. Lease renewal, registry lookups, and transaction prepare/commit all become network hops instead of function calls. Need to measure and decide what's acceptable.

### 3. Serverless (Lambda + managed services)

No containers. Each service is Lambda functions behind API Gateway (or AppSync). Backing stores are fully managed. Event-driven glue via DynamoDB Streams, EventBridge, Step Functions.

```
API Gateway / AppSync
       │
       ├─ /lease/*    → Lambda → DynamoDB (TTL = reaper)
       ├─ /registry/* → Lambda → DynamoDB + ElastiCache
       ├─ /event/*    → Lambda → EventBridge + SQS
       ├─ /space/*    → Lambda → DynamoDB (conditional writes)
       ├─ /txn/*      → Lambda → DynamoDB + Step Functions
       │
       └─ DynamoDB Streams → Lambda (lease expiry cascade)
```

**When:** Fully managed, scale-to-zero, pay-per-request. Good for bursty coordination workloads where you don't want to pay for idle.

---

## Service-by-Service Cloud Mapping

### LeaseMgr

| Operation | Serverless | Container |
|-----------|-----------|-----------|
| Grant | DynamoDB PutItem (lease_id, resource_id, expires_at) | Same DynamoDB, just called from container |
| Renew | DynamoDB UpdateItem (conditional on not-expired) | Same |
| Cancel | DynamoDB DeleteItem | Same |
| Reaper | DynamoDB native TTL → Stream → Lambda | Background tokio task polling DynamoDB for expired |
| WatchExpiry | DynamoDB Stream → Lambda → fan out (SNS/EventBridge) | DynamoDB Stream → internal broadcast channel |

**Key insight:** DynamoDB TTL *is* the reaper. No custom sweep needed. The Stream event is the expiry broadcast.

### Registry

| Operation | Serverless | Container |
|-----------|-----------|-----------|
| Register | DynamoDB PutItem + ElastiCache SET | Same |
| Lookup | ElastiCache GET (fast path) → DynamoDB Query (fallback) | Same |
| LookupAll | DynamoDB Query with filter expressions | Same |
| Watch | EventBridge rule on registry change events | gRPC stream backed by internal broadcast |
| Template matching | Redis SCAN with MATCH pattern (exact, starts_with) / DynamoDB filter (contains) | Same |

**ElastiCache vs pure DynamoDB:** Lookups are the hot path — every `ServiceDiscovery.Get()` touches this. ElastiCache (Redis) gives sub-ms reads and native pattern matching. DynamoDB is the source of truth, Redis is the read cache. Registry writes go to both.

### EventMgr

| Operation | Serverless | Container |
|-----------|-----------|-----------|
| Subscribe (durable) | Create SQS queue + EventBridge rule routing to it | DynamoDB subscription record + SQS queue |
| Subscribe (best-effort) | EventBridge rule → Lambda → WebSocket push | Internal broadcast channel |
| Emit | EventBridge PutEvents | Same |
| Receive (drain) | SQS ReceiveMessage + DeleteMessageBatch | Same |
| Receive (live) | WebSocket API Gateway + Lambda push | gRPC server-stream |
| Sequence numbers | DynamoDB atomic counter (UpdateItem ADD) | Same, or in-memory AtomicU64 |

**Natural fit.** EventBridge is basically a managed event bus with template matching built in. SQS is the mailbox. This maps almost 1:1.

### Space

| Operation | Serverless | Container |
|-----------|-----------|-----------|
| Out | DynamoDB PutItem + Stream triggers appearance watchers | Same |
| Read | DynamoDB Query/Scan with filter | Same |
| Take | DynamoDB DeleteItem with ConditionExpression (atomic — one winner) | Same |
| Blocking take/read | Step Function wait → SQS notification on match → resume | Tokio broadcast + timeout |
| Watch | DynamoDB Stream → Lambda → filter → WebSocket push | gRPC server-stream |
| Template matching | DynamoDB filter expressions (limited) or ElastiCache secondary index | In-memory scan (current) |

**Hardest to serverless-ify.** Blocking operations don't fit Lambda's request/response model. Two approaches:

1. **Long-poll with Step Functions** — client calls "take with wait", API Gateway returns a Step Function execution ARN. Client polls for result. Step Function listens on SQS for matching tuple notification.
2. **WebSocket** — client holds open WebSocket. DynamoDB Stream → Lambda → checks template → pushes to WebSocket when match arrives.

Option 2 is closer to the spec's spirit. Option 1 is simpler to build.

**Take atomicity:** DynamoDB conditional delete is the key. `DeleteItem` with `ConditionExpression: "attribute_exists(tuple_id)"` — exactly one caller wins, losers get ConditionalCheckFailed. Same semantics as our DashMap race loop, but guaranteed by DynamoDB.

### TransactionMgr

| Operation | Serverless | Container |
|-----------|-----------|-----------|
| Begin | DynamoDB PutItem (txn record) + Step Function execution | DynamoDB PutItem |
| Enlist | DynamoDB UpdateItem (add participant to txn record) | Same |
| Commit | Step Function: Parallel prepare → evaluate votes → Parallel commit/abort | Tokio join_all (current) |
| Abort | Step Function: Parallel abort all participants | Same |
| Lease expiry = auto-abort | DynamoDB TTL on txn record → Stream → Lambda triggers abort Step Function | Background listener |
| Crash recovery | Step Function state is durable — resumes after Lambda timeout | Not implemented (in-memory) |

**Step Functions are a natural 2PC coordinator.** Each phase (prepare, evaluate, commit/abort) is a state. The parallel fan-out for prepare/commit maps to Step Functions Parallel state. If a Lambda times out mid-prepare, Step Functions retries or takes the error path (abort). This gives us crash recovery basically for free — something we don't have today.

### Proxy

**Doesn't go serverless.** TCP forwarding needs long-lived connections. This stays as a container (or sidecar). In the serverless topology, clients use ServiceDiscovery to resolve addresses directly — the proxy layer is optional.

In the container topology, Proxy could be an NLB + target group per service, or an Envoy sidecar doing the same resolve-at-connect pattern.

---

## Infrastructure as Code

CDK stacks, one per service. Shared constructs for the common patterns.

```
coordin8-infra/
  lib/
    lease-stack.ts        → DynamoDB table, Lambda, API Gateway routes
    registry-stack.ts     �� DynamoDB + ElastiCache, Lambda
    event-stack.ts        → EventBridge + SQS, Lambda
    space-stack.ts        → DynamoDB + Streams, Lambda, WebSocket APIGW
    txn-stack.ts          → DynamoDB + Step Functions
    shared/
      dynamo-table.ts     → Common table patterns (TTL, streams, GSIs)
      api-routes.ts       → API Gateway route builder
  bin/
    coordin8-serverless.ts  → Full serverless stack
    coordin8-containers.ts  → ECS/Fargate task definitions + ALB
```

**Dependency edges mirror boot order:**
- LeaseStack has no deps
- RegistryStack depends on LeaseStack (needs lease table ARN)
- EventStack depends on LeaseStack
- SpaceStack depends on LeaseStack + RegistryStack (for matcher config)
- TxnStack depends on LeaseStack

---

## The Provider Abstraction

This is why the `SpaceStore` / `LeaseStore` / `EventStore` traits exist. Phase 3 adds:

```
providers/
  local/          ← InMemory (exists today)
  dynamo/         ← DynamoDB implementations
  redis/          ← ElastiCache for Registry read cache
```

The manager layer (`SpaceManager`, `LeaseManager`, etc.) doesn't change. It calls `store.insert()` and doesn't care if that's a DashMap or a DynamoDB PutItem. That's the whole point.

---

## Open Questions (to refine later)

1. **gRPC vs REST for serverless** — API Gateway natively speaks REST/WebSocket. gRPC over API Gateway requires HTTP/2 and ALB, not API Gateway. Do we add a REST API alongside gRPC? Or use ALB for the serverless topology too?

2. **Cold start impact** — Lambda cold starts on lease renewal could cause lease expiry. Need to measure. Provisioned concurrency for critical paths?

3. **Cost model** — DynamoDB on-demand vs provisioned. SQS per-message cost for high-frequency events. Need to model for realistic workloads.

4. **Multi-region** — DynamoDB Global Tables for lease/registry. EventBridge cross-region event bus. Space tuples — partition by attribute? This is a Phase 4 conversation.

5. **Local dev story** — MiniStack for the full serverless stack locally? Or just use the monolith Djinn for dev and only deploy serverless to staging/prod?

6. **Streaming in serverless** — Watch/Receive RPCs are server-streaming. WebSocket API Gateway handles this but adds complexity. Worth it, or do we offer polling alternatives?

---

## Summary

| Topology | Best For | Backing | Complexity |
|----------|----------|---------|------------|
| Monolith Djinn | Dev, edge, embedded | InMemory | Low |
| Container split | Teams, staging, prod (predictable load) | DynamoDB + ElastiCache + SQS | Medium |
| Serverless | Scale-to-zero, bursty, fully managed | DynamoDB + EventBridge + SQS + Step Functions | High |

Same proto contracts. Same SDK clients. Same spec compliance. Different operational characteristics. The Provider trait is the seam.

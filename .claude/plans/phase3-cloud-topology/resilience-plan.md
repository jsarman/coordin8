# Phase 3B: Resilience & Scalability Plan

**Goal:** Every Coordin8 service becomes stateless, ephemeral, and horizontally scalable — without sacrificing functionality. The Proxy becomes an edge service, just another Coordin8 service registered in the Registry.

## Core Principle

With the DynamoDB provider in place, all services are already stateless *for storage*. The one thing keeping them tied to a specific process is **in-process broadcast channels** (`tokio::broadcast`). Externalize those, and every service becomes fungible.

## What Needs to Change

### The Broadcast Problem

Five broadcast channels exist today:

| Channel | Purpose | Subscribers |
|---------|---------|-------------|
| `expiry_tx` (LeaseRecord) | Lease expiry notifications | Registry, EventMgr, TxnMgr, Space |
| `registry_tx` (RegistryChangedEvent) | Service registered/expired | Proxy, Watch clients |
| `event_tx` (EventRecord) | Live event delivery (best-effort) | Streaming subscribers |
| `space_tuple_tx` (TupleRecord) | Tuple appearance | Watch clients |
| `space_expiry_tx` (TupleRecord) | Tuple expiration | Watch clients |

All are `tokio::broadcast` — single-process, lost on restart, invisible to other instances.

### External Pub/Sub Options

| Option | Latency | Cost | Complexity | Best For |
|--------|---------|------|------------|----------|
| DynamoDB Streams + Lambda | ~1-2s | Low (per stream record) | Medium | Lease expiry (already has TTL) |
| SNS + SQS | ~100ms | Very low | Low | Fan-out to multiple service instances |
| ElastiCache (Redis) Pub/Sub | ~1ms | $13+/mo (smallest node) | Low | Low-latency watch notifications |
| EventBridge | ~500ms | Low | Medium | Event routing with filtering |

**Recommended approach — SNS fan-out:**

Each service instance creates an SQS queue on startup and subscribes it to the relevant SNS topics. On shutdown (or crash), the queue is orphaned and cleaned up by TTL policy. Simple, cheap, no Redis to manage.

```
Producer → SNS Topic → SQS Queue (instance A)
                      → SQS Queue (instance B)
                      → SQS Queue (instance C)
```

For **watch streams** (gRPC server-streaming to clients), the flow becomes:
1. Tuple written to DynamoDB by Space instance A
2. Space A publishes to SNS `space-appearances` topic
3. All Space instances receive via their SQS queue
4. Each instance pushes to its locally-connected watch clients

### Service-by-Service Breakdown

#### LeaseMgr
- **Reaper:** Replace polling scan with DynamoDB TTL + Streams. When a lease item's TTL expires, DynamoDB deletes it and emits a stream record. A Lambda (or each instance polling its own stream) picks it up and publishes to SNS `lease-expiry`.
- **Grant/Renew/Cancel:** Already stateless. Multiple instances fine.
- **Scaling:** Lambda candidate. Simple request/response operations.

#### Registry
- **Register/Lookup:** Already stateless against DynamoDB.
- **Watch (gRPC stream):** Needs external pub/sub. Registry change events go to SNS, each instance fans out to its connected watchers.
- **Scaling:** Lambda for CRUD, Fargate for Watch streams (or WebSocket API Gateway).

#### EventMgr
- **Subscribe/Emit/Dequeue:** Already stateless against DynamoDB.
- **Live stream (best-effort):** Same pattern as Registry Watch — SNS fan-out to instance-local subscribers.
- **Scaling:** Lambda for CRUD + enqueue/dequeue, Fargate for live streams.

#### TransactionMgr
- **Begin/Enlist/Commit/Abort:** Already stateless. 2PC coordinator calls participants via gRPC — any instance can drive it.
- **Lease expiry → auto-abort:** Receives from SNS `lease-expiry` topic.
- **Scaling:** Lambda candidate. Could also be Step Functions for crash recovery.

#### Space
- **Out/Read/Take:** Already stateless. Conditional delete gives atomic take across instances.
- **Blocking take/read:** Needs watch mechanism — subscribe to SNS, wait for matching tuple appearance.
- **Watch streams:** SNS fan-out, same as Registry/EventMgr.
- **Scaling:** Fargate for streaming, Lambda for CRUD.

#### Proxy (Edge Service)
- **Reclassified:** Not a core Djinn service. It's an edge service that:
  - Registers itself in the Registry like any other service
  - Uses ServiceDiscovery to resolve upstreams
  - Runs wherever edge clients need TCP-level access
- **Deployment:** Optional Fargate task or EC2 at the edge. Not part of the core scalable deployment.
- **No changes needed** — it already works this way, just needs to be framed as optional.

## Deployment Topology

```
┌─────────────────────────────────────────────────────────┐
│                    API Gateway / ALB                     │
│                         │                               │
│    ┌────────────────────┼───────────────────┐            │
│    ▼                    ▼                   ▼            │
│ ┌──────┐          ┌──────────┐        ┌─────────┐       │
│ │Lambda│          │ Fargate  │        │ Fargate  │       │
│ │      │          │          │        │          │       │
│ │Lease │          │ Space    │        │ Registry │       │
│ │Txn   │          │ (stream) │        │ (watch)  │       │
│ │Event │          │          │        │          │       │
│ │(CRUD)│          │ scales   │        │ scales   │       │
│ └──┬───┘          │ 0-N      │        │ 0-N      │       │
│    │              └────┬─────┘        └────┬─────┘       │
│    │                   │                   │             │
│    └───────┬───────────┼───────────────────┘             │
│            ▼           ▼                                 │
│     ┌─────────────────────────┐   ┌──────────────┐      │
│     │       DynamoDB          │   │  SNS Topics   │      │
│     │       9 tables          │   │  + SQS queues │      │
│     └─────────────────────────┘   └──────────────┘      │
└─────────────────────────────────────────────────────────┘

           ┌──────────────┐
           │ Proxy (edge) │  ← Optional, deployed where needed
           │ Fargate/EC2  │     registered in Registry like any service
           └──────────────┘
```

## Implementation Order

1. **SNS/SQS provider for broadcasts** — new crate `coordin8-provider-sns` or extend the dynamo provider. Abstract the broadcast channel behind a trait so local mode still uses `tokio::broadcast`.
2. **DynamoDB Streams for lease expiry** — replace the reaper scan. Eliminates the biggest recurring cost.
3. **Wire services to use external pub/sub** — each manager takes a broadcast trait instead of a concrete `tokio::broadcast::Sender`.
4. **Fargate task definitions** — Space and Registry (streaming) as Fargate services behind ALB.
5. **Lambda functions** — Lease, Txn, Event CRUD operations.
6. **CDK stacks** — infrastructure as code for all of it.
7. **Proxy as edge service** — decouple from Djinn boot sequence, make it a standalone binary that registers itself.

## What We Keep

- **Proto contracts** — unchanged. Same gRPC interfaces.
- **SDK clients** — unchanged. They don't know or care about the deployment topology.
- **Provider trait** — the DynamoDB stores we just built are the backing for all topologies.
- **Template matching** — client-side scan against DynamoDB. Works the same from Lambda or Fargate.
- **Boot order within a service** — each service still initializes its own dependencies. The *inter-service* boot order relaxes because they're independent processes.

## Cost Estimate (Serverless + Fargate)

| Component | Monthly |
|-----------|---------|
| Lambda (Lease/Txn/Event CRUD) | ~$0 (free tier covers light use) |
| Fargate (Space + Registry streaming, 0.25 vCPU each) | ~$18.60 |
| DynamoDB (9 tables, on-demand) | ~$1-2 |
| SNS + SQS | ~$0 (free tier: 1M publishes, 1M requests) |
| DynamoDB Streams | ~$0 (first 2.5M reads free) |
| ALB | ~$16 + data |
| **Total** | **~$36-40/mo** |

vs. single EC2 t3.micro: ~$10/mo — but no auto-scaling, no independent deploys, manual management.

## Open Questions

1. **gRPC through API Gateway** — API Gateway doesn't natively support HTTP/2 / gRPC. Options: ALB (supports gRPC), or REST adapter in front of Lambda. ALB adds ~$16/mo base cost.
2. **Watch stream scaling** — how many concurrent watch clients do we expect? This determines whether Fargate or WebSocket API Gateway is the right call.
3. **Broadcast trait design** — should this be a new trait in `coordin8-core`, or a wrapper around the existing channel types?
4. **DynamoDB Streams vs SNS for lease expiry** — Streams gives you the change event directly. SNS requires the reaper (or a Lambda on the Stream) to republish. Streams + Lambda is cleaner but adds a Lambda invocation per expiry.
5. **Local dev story** — `COORDIN8_PROVIDER=local` stays as-is with `tokio::broadcast`. Do we also want a `COORDIN8_PROVIDER=dynamo-local` that uses DynamoDB but keeps local broadcasts? (Probably yes — that's what we have today.)

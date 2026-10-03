---
name: stack-up
description: Start the Coordin8 infrastructure stack (Docker, MiniStack, Djinn) in tmux panes. Use when the user needs to bring up services for development or testing.
allowed-tools: Bash, Read
---

# stack-up

Spin up the Coordin8 development stack in a tmux session, with each service in its own named pane for visibility.

## Usage

- `/stack-up` — interactive: ask what to bring up
- `/stack-up docker` — bundled docker-compose stack (MiniStack + cfn-init + Djinn + greeter) == `mise r up`
- `/stack-up split` — split-mode compose stack (registry/event/space/txn/proxy, one container each) == `mise r up-split`
- `/stack-up ministack` — MiniStack only (what the Dynamo provider tests need)
- `/stack-up djinn` — local Djinn binary only (`cargo run`, in-memory by default)
- `/stack-up all` — MiniStack + local Djinn

Auth variants exist (`mise r up-auth`, `mise r up-split-auth`) and enable JWT via `COORDIN8_JWT_SECRET`; only use them if the user asks.

## Behavior

### 1. Preflight

Check if a `coordin8` tmux session already exists:
```bash
tmux has-session -t coordin8 2>/dev/null
```
- If it exists, list what's running and ask the user if they want to reuse or tear down first.
- If not, create it:
  ```bash
  tmux new-session -d -s coordin8 -n main
  ```

### 2. Ask about tmux visibility

If the user hasn't attached to the session, suggest:
> "I've started a `coordin8` tmux session. You can attach with `tmux attach -t coordin8` in another terminal to watch the output live."

### 3. Start services in named panes

Create a pane per service and send the start command:

Set `ROOT=$(git rev-parse --show-toplevel)` first and `cd "$ROOT"` in every pane — never hard-code a home path.

**Docker compose (bundled):**
```bash
tmux new-window -t coordin8 -n docker
tmux send-keys -t coordin8:docker "cd $ROOT && docker compose up --build 2>&1" Enter
```
Add `COORDIN8_PROVIDER=dynamo` before `docker compose` to run Djinn against MiniStack/DynamoDB instead of in-memory (default `local`).

**Docker compose (split mode):**
```bash
tmux send-keys -t coordin8:docker "cd $ROOT && docker compose -f docker-compose.split.yml up --build 2>&1" Enter
```

**MiniStack:**
```bash
tmux new-window -t coordin8 -n ministack
tmux send-keys -t coordin8:ministack "cd $ROOT && docker compose up ministack 2>&1" Enter
```

**Local Djinn (cargo run, bundled mode):**
```bash
tmux new-window -t coordin8 -n djinn
tmux send-keys -t coordin8:djinn "cd $ROOT/djinn && cargo run 2>&1" Enter
```
For a Dynamo-backed local Djinn, MiniStack must be up and the pane command needs `COORDIN8_PROVIDER=dynamo DYNAMODB_ENDPOINT=http://localhost:4566 COORDIN8_AUTO_CREATE_TABLES=true AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test AWS_DEFAULT_REGION=us-east-1` prefixed.

### 4. Poll for readiness

After launching, poll health checks. Don't flood — check every 3 seconds, max 20 attempts:

**Docker/Djinn health (Registry on 9002 — the bootstrap anchor; the stack-wide readiness signal):**
```bash
for i in $(seq 1 20); do nc -z localhost 9002 && echo "ready" && break || sleep 3; done
```
The bundled compose healthcheck is a TCP probe on 9002 (bundled mode mounts no gRPC health service). A cold `--build` of the Rust image can take much longer than 60s; if it times out, peek at the pane before assuming failure.

**MiniStack:**
```bash
for i in $(seq 1 20); do curl -sf http://localhost:4566/_ministack/health && echo "ready" && break || sleep 3; done
```

### 5. Report

Once ready (or timed out), report:
- Which services are up and on which ports (Registry 9002, Proxy 9003, TransactionMgr 9004, EventMgr 9005, Space 9006, proxy forwarding 9100-9200, MiniStack 4566)
- Any that failed to start — peek at their pane output for errors
- Keep it brief: "Stack is up. Djinn on :9002, MiniStack on :4566." or "Djinn failed to start — build error in the coordin8-registry crate."

## Important

- **Boot order matters:** MiniStack first (compose's `cfn-init` creates the 12 DynamoDB tables), then Djinn, then application services. In the compose files this is enforced with `depends_on`.
- Read `docker-compose.yml` / `docker-compose.split.yml` if needed to confirm service names and ports
- Don't run any compose Djinn stack and `cargo run` Djinn simultaneously — they port-conflict on 9002-9006. Likewise bundled and split compose stacks conflict with each other.
- There is no LeaseMgr service and no port 9001; each service mounts its own LeaseService on its own port.
- If something fails, peek at the pane output and surface the error — don't retry blindly

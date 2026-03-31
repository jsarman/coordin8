---
name: stack-up
description: Start the Coordin8 infrastructure stack (Docker, LocalStack, Djinn) in tmux panes. Use when the user needs to bring up services for development or testing.
allowed-tools: Bash, Read
---

# stack-up

Spin up the Coordin8 development stack in a tmux session, with each service in its own named pane for visibility.

## Usage

- `/stack-up` — interactive: ask what to bring up
- `/stack-up docker` — docker-compose stack only
- `/stack-up localstack` — LocalStack only
- `/stack-up djinn` — local Djinn binary only
- `/stack-up all` — full stack

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

**Docker compose:**
```bash
tmux new-window -t coordin8 -n docker
tmux send-keys -t coordin8:docker 'docker compose up --build 2>&1' Enter
```

**LocalStack:**
```bash
tmux new-window -t coordin8 -n localstack
tmux send-keys -t coordin8:localstack 'docker compose up -d localstack 2>&1' Enter
```

**Local Djinn (cargo run):**
```bash
tmux new-window -t coordin8 -n djinn
tmux send-keys -t coordin8:djinn 'cd djinn && cargo run 2>&1' Enter
```

### 4. Poll for readiness

After launching, poll health checks. Don't flood — check every 3 seconds, max 20 attempts:

**Docker/Djinn health (LeaseMgr on 9001):**
```bash
for i in $(seq 1 20); do nc -z localhost 9001 && echo "ready" && break || sleep 3; done
```

**LocalStack:**
```bash
for i in $(seq 1 20); do curl -sf http://localhost:4566/_localstack/health && echo "ready" && break || sleep 3; done
```

### 5. Report

Once ready (or timed out), report:
- Which services are up and on which ports
- Any that failed to start — peek at their pane output for errors
- Keep it brief: "Stack is up. Djinn on :9001, LocalStack on :4566." or "Djinn failed to start — build error in coordin8-lease crate."

## Important

- **Boot order matters:** LocalStack/Docker first, then Djinn, then application services
- Read `docker-compose.yml` if needed to confirm service names and ports
- Don't run `docker compose up` and `cargo run` Djinn simultaneously unless the user is testing against Docker — they'll port-conflict on 9001
- If something fails, peek at the pane output and surface the error — don't retry blindly

---
name: stack-down
description: Tear down the Coordin8 infrastructure stack and clean up tmux panes. Use when the user is done with development services.
allowed-tools: Bash
---

# stack-down

Cleanly shut down the Coordin8 development stack and optionally kill the tmux session.

## Usage

- `/stack-down` — tear down everything and kill the tmux session
- `/stack-down docker` — stop docker-compose only
- `/stack-down localstack` — stop LocalStack only  
- `/stack-down djinn` — stop the local Djinn only
- `/stack-down keep-session` — stop services but keep the tmux session alive

## Behavior

### 1. Check what's running

```bash
tmux has-session -t coordin8 2>/dev/null && \
tmux list-windows -t coordin8 -F '#{window_name}: #{pane_current_command}'
```

If no session exists, say so and stop.

### 2. Stop services gracefully

**Docker compose:**
```bash
tmux send-keys -t coordin8:docker C-c
sleep 2
tmux send-keys -t coordin8:docker 'docker compose down' Enter
```

**LocalStack:**
```bash
tmux send-keys -t coordin8:localstack 'docker compose down localstack' Enter
```

**Local Djinn:**
```bash
tmux send-keys -t coordin8:djinn C-c
```

Wait a few seconds after each, then peek to confirm shutdown.

### 3. Clean up tmux

Unless `keep-session` was specified:
```bash
tmux kill-session -t coordin8
```

If keeping the session, just close the service windows:
```bash
tmux kill-window -t coordin8:docker
tmux kill-window -t coordin8:localstack
tmux kill-window -t coordin8:djinn
```

### 4. Verify

Quick port check to confirm nothing is lingering:
```bash
nc -z localhost 9001 2>/dev/null && echo "9001 still open" || echo "9001 clear"
nc -z localhost 4566 2>/dev/null && echo "4566 still open" || echo "4566 clear"
```

Report what was stopped and whether ports are clear.

## Important

- Always try graceful shutdown (ctrl-c, docker compose down) before killing
- If a process doesn't stop after ctrl-c + 5 seconds, tell the user rather than escalating to kill -9
- Don't remove Docker volumes unless explicitly asked — data loss is not a default

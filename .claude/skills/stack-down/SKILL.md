---
name: stack-down
description: Tear down the Coordin8 infrastructure stack and clean up tmux panes. Use when the user is done with development services.
allowed-tools: Bash
---

# stack-down

Cleanly shut down the Coordin8 development stack and optionally kill the tmux session.

## Usage

- `/stack-down` — tear down everything and kill the tmux session
- `/stack-down docker` — stop docker-compose only (bundled or split)
- `/stack-down ministack` — stop MiniStack only  
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

**Docker compose** (run from the repo root, `ROOT=$(git rev-parse --show-toplevel)`; match whichever stack was started):
```bash
tmux send-keys -t coordin8:docker C-c
sleep 2
tmux send-keys -t coordin8:docker "cd $ROOT && docker compose down" Enter          # bundled == mise r down
# split mode: docker compose -f docker-compose.split.yml down                      # == mise r down-split
# auth overlays: pass the same -f files used to start them
```

**MiniStack** (only if it was started on its own — `docker compose down` above already removes it with the bundled stack):
```bash
tmux send-keys -t coordin8:ministack "cd $ROOT && docker compose stop ministack && docker compose rm -f ministack" Enter
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
tmux kill-window -t coordin8:ministack
tmux kill-window -t coordin8:djinn
```

### 4. Verify

Quick port check to confirm nothing is lingering:
```bash
for p in 9002 9003 9004 9005 9006 4566; do
  nc -z localhost $p 2>/dev/null && echo "$p still open" || echo "$p clear"
done
```

Report what was stopped and whether ports are clear.

## Important

- The Auction House demo runs its own compose stack from `examples/auction-house/` (`mise r demo-auction-down`); it is not in the `coordin8` tmux session unless the user put it there.
- Always try graceful shutdown (ctrl-c, docker compose down) before killing
- If a process doesn't stop after ctrl-c + 5 seconds, tell the user rather than escalating to kill -9
- Don't remove Docker volumes unless explicitly asked — data loss is not a default

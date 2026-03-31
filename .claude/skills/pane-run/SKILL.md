---
name: pane-run
description: Send a command to a named tmux pane. Use when acting on behalf of the user in a background pane — restarting a service, running a test, issuing a ctrl-c, etc.
allowed-tools: Bash
---

# pane-run

Send a command to a named tmux pane in the `coordin8` session. This is how the agent acts on behalf of the user in a background process.

## Usage

- `/pane-run <pane-name> <command>` — send a command to the named pane
- `/pane-run <pane-name> ctrl-c` — interrupt the running process

## Behavior

1. **Check the session and pane exist:**
   ```bash
   tmux has-session -t coordin8 2>/dev/null
   ```
   If not, tell the user. Don't create sessions — that's `stack-up`'s job.

2. **Send the command:**
   ```bash
   tmux send-keys -t coordin8:<pane> '<command>' Enter
   ```
   For interrupts:
   ```bash
   tmux send-keys -t coordin8:<pane> C-c
   ```

3. **Peek after sending.** Wait 2 seconds, then capture last 15 lines to confirm the command landed:
   ```bash
   sleep 2 && tmux capture-pane -t coordin8:<pane> -p -S -15
   ```
   Summarize the result briefly.

## Safety

- **Destructive commands require confirmation.** If the command involves `rm`, `drop`, `kill`, `docker rm`, or similar — confirm with the user before sending.
- **Don't chain commands.** One command per invocation. If multiple steps are needed, present the plan and let the user approve each.
- This skill sends commands — it does not create panes or sessions (that's `stack-up`).

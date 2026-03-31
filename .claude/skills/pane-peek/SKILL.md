---
name: pane-peek
description: Capture recent output from a named tmux pane without flooding context. Use when monitoring background processes (docker, localstack, djinn) or checking on long-running commands.
allowed-tools: Bash
---

# pane-peek

Capture the last N lines from a named tmux pane. This is how you "check on" a background process without pulling its entire log into the conversation.

## Usage

The user may say `/pane-peek` with optional arguments:
- `/pane-peek` — list active tmux panes in the `coordin8` session, let the user pick
- `/pane-peek <pane-name>` — peek at that pane (default: last 30 lines)
- `/pane-peek <pane-name> <N>` — peek at last N lines

## Behavior

1. **Check tmux is running:**
   ```bash
   tmux has-session -t coordin8 2>/dev/null
   ```
   If no session exists, tell the user — don't create one.

2. **If no pane specified**, list panes:
   ```bash
   tmux list-panes -t coordin8 -F '#{pane_index}: #{pane_title} (#{pane_current_command})'
   ```
   Present the list and ask which one.

3. **Capture output:**
   ```bash
   tmux capture-pane -t coordin8:<pane> -p -S -<N>
   ```
   Default N=30. Keep it tight — the whole point is to avoid context bloat.

4. **Summarize, don't dump.** After capturing:
   - If the output is routine (healthy logs, expected output), summarize in 1-2 sentences
   - If there's an error or something notable, show the relevant lines only
   - If the user wants the raw dump, they'll ask

## Important

- Never capture more than 80 lines unless explicitly asked
- If the pane appears idle or exited, say so
- This skill is read-only — it never sends commands (that's `pane-run`)

# Workflow Preferences (Example)

Copy this to `WORKFLOW.md` and customize. This file is gitignored — each developer may have their own or none at all.

## Pacing

- Checkpoint after each milestone before proceeding
- Ask before making large changes across multiple files
- Keep commits small and focused

## Session Pattern

1. Read the relevant PRD and latest session prep in `.claude/plans/`
2. Execute the work
3. Write completion notes
4. Write next session prep while context is fresh

## Stack Management

- Use `/stack-up` and `/stack-down` to manage the dev environment
- Use `/pane-peek` to monitor background services
- Prefer `COORDIN8_PROVIDER=dynamo` for integration testing

## Testing

- Run `cargo test` before committing Rust changes
- Run integration tests with `--ignored` flag (requires MiniStack)
- Use the test-runner agent for visible test output in tmux

# Command Semantics

The current implementation is shell-first and uses checkpointing at wrapped mutation boundaries.
Internally, rule matching now flows through a shared tool invocation model so future tool adapters can reuse the same matcher and checkpoint recorder:

- `ddl init` creates repo-local state, initializes the shadow git repository, and writes `.daedalus/config.json`.
- `ddl run -- <agent command>` supports `codex` and `claude`, launches them from the repo root, and prepares runtime-specific protection:
  Claude gets a session-scoped `PreToolUse` hook for `Edit|MultiEdit|Write|Bash`, while Codex keeps the checkpointed Bash shell path.
- `ddl shell -- <command>` executes a shell command through the same matcher and checkpoint path used by wrapped runtimes.
- `ddl log` lists timelines and checkpoints, including shell-triggered checkpoint reasons and triggering commands when present.
- `ddl diff` compares checkpoint snapshots with `git diff --no-index`.
- `ddl restore` copies a checkpoint snapshot back into the workspace.
- `ddl resume` restores a checkpoint and reruns the owned top-level command on the same timeline.
- `ddl fork` creates a new timeline rooted in an existing checkpoint.

Current v1 limits:

- `Bash(...)` rules are enforced for supported runtimes
- Claude Code also enforces `Edit(*)`, `MultiEdit(*)`, `Write(*)`, and `Bash(...)` through `PreToolUse`
- Codex remains Bash-only for now
- unsupported runtimes fail clearly instead of running partially protected
- transcript capture is still metadata-ready, not deeply integrated
- symlink snapshots are rejected for now
- restore operates on the configured workspace scope and does not attempt to rewind external side effects

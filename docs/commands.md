# Command Semantics

The current implementation is shell-first and uses checkpointing at wrapped mutation boundaries.
Internally, rule matching now flows through a shared tool invocation model so future tool adapters can reuse the same matcher and checkpoint recorder:

- `ddl init` creates repo-local state, initializes the shadow git repository, and writes `.daedalus/config.json`.
- `ddl run -- <agent command>` supports `codex` and `claude`, launches them from the repo root, and prepares runtime-specific protection:
  Claude gets a session-scoped `PreToolUse` hook for `Edit|MultiEdit|Write|Bash`, while Codex keeps the checkpointed Bash shell path.
- `ddl shell -- <command>` executes a shell command through the same matcher and checkpoint path used by wrapped runtimes.
- `ddl log` lists timelines and checkpoints, including shell-triggered checkpoint reasons and triggering commands when present.
- `ddl log` also reports restore availability separately from rewind availability, so users can choose repo-only recovery or agent-context recovery explicitly.
- `ddl diff` compares checkpoint snapshots with `git diff --no-index`.
- `ddl restore` copies a checkpoint snapshot back into the workspace.
- `ddl rewind` restores a checkpoint and rewinds the owned runtime on the same timeline.
  Claude-backed checkpoints first restore the experimental best-effort local Claude rewind snapshot, then launch `claude --resume <session_id>`.
  If checkpoint-head agent context is unavailable, `ddl rewind` fails clearly and the user should use `ddl restore` instead.
- `ddl fork` creates a new timeline rooted in an existing checkpoint.

Current v1 limits:

- `Bash(...)` rules are enforced for supported runtimes
- Claude Code also enforces `Edit(*)`, `MultiEdit(*)`, `Write(*)`, and `Bash(...)` through `PreToolUse`
- Codex remains Bash-only for now
- unsupported runtimes fail clearly instead of running partially protected
- Claude rewind is experimental and only covers the main session transcript plus `file-history` for the saved session id
- subagent Claude state, task state, telemetry, and unrelated `~/.claude` files are not rewound in v1
- symlink snapshots are rejected for now
- restore operates on the configured workspace scope and does not attempt to rewind external side effects

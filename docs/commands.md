# Command Semantics

The current implementation is shell-first and uses checkpointing at wrapped mutation boundaries.
Internally, rule matching now flows through a shared tool invocation model so future tool adapters can reuse the same matcher and checkpoint recorder:

- `ddl init` creates repo-local state, initializes the shadow git repository, and writes `.daedalus/config.json`.
- `ddl run -- <agent command>` supports `codex` and `claude`, launches them from the repo root, and prepares runtime-specific protection:
  Claude gets a session-scoped `PreToolUse` hook for `Edit|MultiEdit|Write|Bash`, while Codex keeps the checkpointed Bash shell path.
- `ddl shell -- <command>` executes a shell command through the same matcher and checkpoint path used by wrapped runtimes.
- `ddl log` lists timelines and checkpoints, including shell-triggered checkpoint reasons and triggering commands when present.
- `ddl log` also reports Claude rewind state explicitly as `available`, `native session only`, or `unavailable`.
- `ddl diff` compares checkpoint snapshots with `git diff --no-index`.
- `ddl restore` copies a checkpoint snapshot back into the workspace.
- `ddl resume` restores a checkpoint and resumes the owned runtime on the same timeline.
  Claude-backed checkpoints use the saved Claude session id and, when available, first restore an experimental best-effort local Claude rewind snapshot before launching `claude --resume <session_id>`.
  If that rewind snapshot is missing, `ddl resume` restores the workspace and falls back to native same-session Claude resume with a warning.
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

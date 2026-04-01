# Command Semantics

The current implementation is Claude-first and uses checkpointing at wrapped mutation boundaries.
Rule matching flows through a shared internal tool invocation model so the same matcher is used for Claude hooks and direct shell execution.

- `ddl init` creates repo-local state, initializes the shadow git repository, writes `.daedalus/config.json`, and records the workspace root for later recovery.
- `ddl run -- claude <args...>` launches Claude from the repo root and injects a session-scoped `PreToolUse` hook for `Edit|MultiEdit|Write|Bash`.
- `ddl shell -- <command>` executes a shell command through the same checkpoint matcher and recorder.
- `ddl log` is TTY-aware:
  in an interactive terminal it opens a recovery console with recent sessions, diff inspection, and recovery actions.
  In non-interactive contexts it prints plain text for scripts and pipes.
- `ddl diff` compares checkpoint snapshots with `git diff --no-index`.
- `ddl restore` destructively copies a checkpoint snapshot back into the workspace.
- `ddl rewind` restores a checkpoint and rewinds the owned Claude session on the same run.
  Claude-backed checkpoints first restore the experimental best-effort local Claude rewind snapshot, then launch `claude --resume <session_id>`.
  If Claude context is unavailable, or the checkpoint is not Claude-backed, `ddl rewind` fails clearly and the user should use `ddl restore` instead.

Current v1 limits:

- `Edit(*)`, `MultiEdit(*)`, `Write(*)`, and `Bash(...)` are enforced for Claude Code through `PreToolUse`
- unsupported runtimes fail clearly; Claude is the only supported runtime for `ddl run`
- Claude rewind is experimental and only covers the main session transcript plus `file-history` for the saved session id
- subagent Claude state, task state, telemetry, and unrelated `~/.claude` files are not rewound in v1
- `.git` is intentionally out of scope; `daedalus` does not snapshot, restore, or protect repo metadata
- symlink snapshots are rejected for now
- restore operates on the configured workspace scope, removes files created after the checkpoint, and does not attempt to rewind external side effects

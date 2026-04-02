# Architecture

The codebase is a Rust workspace with a single CLI crate, `ddl`.

The current base is split into a few stable responsibilities:

- CLI parsing and command dispatch
- JSON config parsing, normalized tool invocation construction, and checkpoint rule matching
- domain types for runs, timelines, checkpoints, and resumability
- metadata encoding and persistence
- per-repo state management under `~/.daedalus/` or `$DAEDALUS_HOME`
- runtime wrapper preparation for Claude Code
- a shadow git repository used to version checkpoint snapshots

The product boundary is intentionally narrow:

- workspace files are recoverable from external Daedalus state
- Claude local state is recoverable on Claude-backed checkpoints when captured
- the live repository `.git` directory is not part of `daedalus` recovery and remains Git's responsibility

This base favors a narrow, credible v1 over premature abstraction. The current implementation keeps a single storage backend, a small command surface, and a shell-first runtime model while routing checkpoint matching through a shared internal tool invocation pipeline. Claude Code emits `Edit`, `MultiEdit`, `Write`, and `Bash` invocations through a supported `PreToolUse` hook, and `daedalus` stores the metadata needed to restore the workspace and attempt a Claude session rewind.

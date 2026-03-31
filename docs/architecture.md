# Architecture

The codebase is set up as a Rust workspace with a single initial CLI crate, `ddl`.

The current base is split into a few stable responsibilities:

- CLI parsing and command dispatch
- JSON config parsing, normalized tool invocation construction, and checkpoint rule matching
- domain types for runs, timelines, checkpoints, and resumability
- metadata encoding and persistence
- repo-local state management under `.daedalus/`
- runtime wrapper preparation for supported agent CLIs
- a shadow git repository used to version checkpoint snapshots

This base favors a narrow, credible v1 over premature abstraction. The first implementation keeps a single storage backend, a small command surface, and a shell-first runtime model while routing checkpoint matching through a shared internal tool invocation pipeline. Public enforcement is still Bash-only for now, and `Edit(*)` / `Write(*)` remain dormant until a runtime emits those invocations.

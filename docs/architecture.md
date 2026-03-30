# Architecture

The codebase is set up as a Rust workspace with a single initial CLI crate, `ddl`.

The current base is split into a few stable responsibilities:

- CLI parsing and command dispatch
- JSON config parsing and checkpoint rule matching
- domain types for runs, timelines, checkpoints, and resumability
- metadata encoding and persistence
- repo-local state management under `.daedalus/`
- runtime wrapper preparation for supported agent CLIs
- a shadow git repository used to version checkpoint snapshots

This base favors a narrow, credible v1 over premature abstraction. The first implementation keeps a single storage backend, a small command surface, and a shell-first runtime model while leaving room for future edit/write hooks or a shared core crate if MCP support is added later.

# Architecture

The codebase is set up as a Rust workspace with a single initial CLI crate, `ddl`.

The current base is split into a few stable responsibilities:

- CLI parsing and command dispatch
- domain types for runs, timelines, checkpoints, and resumability
- metadata encoding and persistence
- repo-local state management under `.daedalus/`
- a shadow git repository used to version checkpoint snapshots

This base favors a narrow, credible v1 over premature abstraction. The first implementation keeps a single storage backend and a small command surface while leaving room for a future shared core crate if MCP support is added later.


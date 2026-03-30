# Command Semantics

The current scaffold implements the public CLI shape described in the README and provides a minimal working baseline:

- `ddl init` creates repo-local state and initializes the shadow git repository.
- `ddl run -- <agent command>` records a protected run, creates an initial checkpoint, and executes the agent command from the repo root.
- `ddl log` lists timelines and checkpoints.
- `ddl diff` compares checkpoint snapshots with `git diff --no-index`.
- `ddl restore` copies a checkpoint snapshot back into the workspace.
- `ddl resume` restores a checkpoint and reruns the owned command on the same timeline.
- `ddl fork` creates a new timeline rooted in an existing checkpoint.

Current limitations of the base:

- automatic checkpointing before specific unsafe tool actions is not implemented yet
- transcript capture is metadata-ready but not deeply integrated
- symlink snapshots are rejected for now
- restore operates on the configured workspace scope and does not attempt to rewind external side effects

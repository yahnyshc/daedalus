# State Layout

Runtime state is stored in a hidden repo-local directory:

```text
.daedalus/
  config.json
  runs/
  timelines/
  checkpoints/
  transcripts/
  tool_outputs/
  runtime/
  shadow/
    .git/
    snapshots/
```

Notes:

- `runs/`, `timelines/`, and `checkpoints/` contain hex-encoded line-based metadata records.
- `config.json` contains the v1 checkpointing rules. Older repos with only the legacy `config` file should be re-initialized or migrated.
- `runtime/` contains per-run wrapper shims, hook helpers, and runtime metadata such as Claude session ids used for true resume.
- `runtime/<run_id>/session.meta` stores resume-relevant runtime metadata for owned runs.
- `shadow/` is a git repository dedicated to checkpoint storage.
- `shadow/snapshots/<checkpoint_id>/` contains the captured workspace snapshot for a checkpoint.
- The initial base excludes `.git`, `.daedalus`, and `target` from snapshots to avoid capturing repository internals and build artifacts.

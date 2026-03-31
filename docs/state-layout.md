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
- `runtime/` contains per-run wrapper shims and hook helpers used to route supported runtime Bash execution through `ddl shell` and Claude Code `PreToolUse` events back through `ddl`.
- `shadow/` is a git repository dedicated to checkpoint storage.
- `shadow/snapshots/<checkpoint_id>/` contains the captured workspace snapshot for a checkpoint.
- The initial base excludes `.git`, `.daedalus`, and `target` from snapshots to avoid capturing repository internals and build artifacts.

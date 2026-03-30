# State Layout

Runtime state is stored in a hidden repo-local directory:

```text
.daedalus/
  config
  runs/
  timelines/
  checkpoints/
  transcripts/
  tool_outputs/
  shadow/
    .git/
    snapshots/
```

Notes:

- `runs/`, `timelines/`, and `checkpoints/` contain hex-encoded line-based metadata records.
- `shadow/` is a git repository dedicated to checkpoint storage.
- `shadow/snapshots/<checkpoint_id>/` contains the captured workspace snapshot for a checkpoint.
- The initial base excludes `.git`, `.daedalus`, and `target` from snapshots to avoid capturing repository internals and build artifacts.


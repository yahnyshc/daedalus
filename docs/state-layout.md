# State Layout

Runtime state is stored in a hidden repo-local directory:

```text
.daedalus/
  config.json
  store.meta
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
- `config.json` contains the current checkpointing rules. Older repos with only the legacy `config` file should be re-initialized or migrated.
- `store.meta` records the workspace root so `ddl` can keep operating even if the live `.git` directory is missing.
- `runtime/` contains per-run wrapper shims, Claude hook helpers, and runtime metadata such as Claude session ids plus experimental Claude rewind snapshots.
- `runtime/<run_id>/session.meta` stores rewind-relevant metadata for owned Claude runs.
- `runtime/<run_id>/claude-checkpoints/<checkpoint_id>/` stores experimental Claude local rewind state when captured.
  v1 only snapshots `~/.claude/projects/<escaped-cwd>/<session_id>.jsonl` and `~/.claude/file-history/<session_id>/`.
- `shadow/` is a git repository dedicated to checkpoint storage.
- `shadow/snapshots/<checkpoint_id>/` contains the captured workspace snapshot for a checkpoint.
- The initial base excludes `.git`, `.daedalus`, and `target` from snapshots. Git metadata is intentionally out of scope for `daedalus` recovery.

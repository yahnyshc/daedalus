# daedalus

Repo-local checkpointing and recovery for Claude Code runs.

`daedalus` wraps Claude Code, creates checkpoints before configured mutation tools, and gives you two recovery paths:

- `ddl restore` restores the workspace to a checkpoint
- `ddl rewind` restores the workspace and resumes the Claude-backed run when rewind data was captured

Git still owns commit history. `daedalus` handles the failure mode where an agent run was going fine until one edit or shell command damaged the working state.

> Status: early and intentionally narrow. `daedalus` is currently Claude-first and only supports Claude Code for `ddl run`.

<img width="1111" height="663" alt="Screenshot 2026-04-01 at 19 40 08" src="https://github.com/user-attachments/assets/3a62120e-504c-49d9-b390-04ae73b16af1" />

## What It Looks Like

Run Claude under `daedalus`:

```bash
ddl run -- claude
```

If a protected action goes wrong:

```bash
ddl log
ddl restore <checkpoint_id>
```

If the checkpoint came from a Claude-backed run and rewind state was captured:

```bash
ddl rewind <checkpoint_id>
```

That is the whole model:

- checkpoint before risky action
- inspect recent checkpoints
- restore files, or restore files and resume the run

## Why It Exists

AI coding workflows have a specific failure mode:

- the agent has already made useful progress
- a later edit or shell command damages the workspace
- a normal Git revert is too coarse or too late
- starting a fresh agent session throws away useful context

`daedalus` is built for that case. It does not replace Git. It adds short-range recovery around live agent actions.

## Quickstart

Install the local CLI:

```bash
cargo install --path crates/ddl
```

Initialize repo-local state:

```bash
ddl init
```

Run Claude under protection:

```bash
ddl run -- claude
```

Inspect recent checkpoints and recover when needed:

```bash
ddl log
ddl restore <checkpoint_id>
ddl rewind <checkpoint_id>
```

`ddl log` opens an interactive recovery console in a TTY and prints plain text in non-interactive contexts.

## How Checkpointing Works

`daedalus` owns the Claude run and checkpoints before configured mutation boundaries.

Today that means:

- `Edit(*)`
- `MultiEdit(*)`
- `Write(*)`
- configured `Bash(...)` rules

`ddl init` writes a repo-local config at `.daedalus/config.json`:

```json
{
  "checkpointing": {
    "before": [
      "Edit(*)",
      "MultiEdit(*)",
      "Write(*)",
      "Bash(npm install:*)",
      "Bash(rm:*)",
      "Bash(mv:*)"
    ]
  }
}
```

Recovery flow:

```text
Claude run
    |
    v
checkpoint before protected action
    |
    v
bad action lands
    |
  +-+-------------------+
  |                     |
  v                     v
restore             rewind
files only          files + Claude session resume
```

## Restore vs Rewind

Use `ddl restore` when you want the workspace back at a checkpoint.

Use `ddl rewind` when all of the following are true:

- the checkpoint came from a Claude-backed run owned by `daedalus`
- workspace snapshot data still exists
- Claude rewind state was captured for that checkpoint

`ddl rewind` first restores the checkpoint, then attempts to resume the same Claude session. If Claude context is unavailable, or the checkpoint is not rewindable, `ddl rewind` fails clearly and `ddl restore` remains available.

## What Gets Protected

Protected today:

- workspace files
- repo-local checkpoint metadata under `.daedalus/`
- Claude-backed local rewind snapshot data when captured

Checkpoint coverage today:

- `Edit(*)`
- `MultiEdit(*)`
- `Write(*)`
- configured `Bash(...)`

For Claude-backed runs owned by `daedalus`, checkpoints also record:

- the Claude session id
- a best-effort local Claude rewind snapshot under `.daedalus/runtime/<run_id>/claude-checkpoints/<checkpoint_id>/`

That snapshot currently covers:

- `~/.claude/projects/<escaped-cwd>/<session_id>.jsonl`
- `~/.claude/file-history/<session_id>/`

## Current Limits

The v1 scope is intentionally narrow:

- Claude Code only. Other runtimes are unsupported for `ddl run`.
- `ddl rewind` only works for Claude-backed checkpoints with captured rewind state.
- `.git` is out of scope. `daedalus` does not snapshot, restore, or protect repo metadata.
- External side effects outside the workspace are not rewound.
- The current Claude snapshot is best-effort and does not cover all of `~/.claude`, subagent state, task state, telemetry, or vendor UI state.
- Symlink snapshots are rejected.
- `ddl restore` replaces the current workspace snapshot and removes files created after the checkpoint while leaving `.git`, `.daedalus`, and `target` untouched.

## Commands

```bash
ddl init
ddl run -- claude <args...>
ddl shell -- <command>
ddl log
ddl diff [checkpoint_a] [checkpoint_b]
ddl restore <checkpoint_id>
ddl rewind <checkpoint_id>
```

- `ddl init` creates repo-local state, initializes the shadow git repository, and writes `.daedalus/config.json`
- `ddl run` launches Claude from the repo root with checkpoint protection enabled
- `ddl shell` runs a shell command through the same checkpoint matcher
- `ddl log` shows recent checkpoints and available recovery actions
- `ddl diff` compares checkpoint snapshots
- `ddl restore` is destructive workspace recovery only
- `ddl rewind` is workspace recovery plus Claude resume when the checkpoint is rewindable

## Status

The current shell-first base includes:

- repo-local `.daedalus/` state
- a shadow git-backed snapshot store
- automatic checkpointing before configured Bash rules
- Claude `PreToolUse` hook checkpointing for `Edit`, `MultiEdit`, `Write`, and `Bash`
- interactive `ddl log` recovery

The main question for the project is still practical: does restore plus rewind materially improve real AI-assisted development workflows.

## Docs

- [Command semantics](docs/commands.md)
- [Architecture](docs/architecture.md)
- [State layout](docs/state-layout.md)

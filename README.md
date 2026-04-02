# daedalus

`daedalus` is a recovery layer for Claude Code.

It protects a Claude run before risky edits and shell commands, then gives you two clean recovery moves:

- `ddl restore` puts the workspace back at the checkpoint
- `ddl rewind` restores the workspace and resumes Claude from that saved point when Claude context was captured

`daedalus` is not a replacement for Git. It protects workspace files and Claude local state while Git remains the source of truth for commits, refs, index state, rebases, and the `.git` directory.

The product is intentionally narrow right now: Claude-specific first, broader runtime support later only if this model proves valuable.

## Why try it

Git can recover files. It usually cannot take Claude back to the exact point right before a bad action.

`daedalus` is built for that moment:

- Claude has already made useful progress
- a risky edit or shell command goes wrong
- you want the workspace back without making safety commits
- you want Claude to continue from the pre-mistake point instead of starting over

Git still owns version control. `daedalus` coexists with it by protecting the live workspace snapshot that Git often cannot recover cleanly, especially for untracked or partially edited files.

## Quickstart

Install the local CLI:

```bash
cargo install --path crates/ddl
```

Initialize a repo:

```bash
ddl init
```

Run Claude under protection:

```bash
ddl run -- claude
```

When something goes wrong:

```bash
ddl log
ddl restore <checkpoint_id>
ddl rewind <checkpoint_id>
```

`ddl log` opens an interactive recovery console in a TTY and prints plain text in non-interactive contexts.

## Releasing

GitHub Releases are created from version tags. Push a `v*` tag such as `v0.1.0` and Actions will build release archives for Linux and macOS, then attach them to a GitHub release with the same tag name.

```bash
git tag v0.1.0
git push origin v0.1.0
```

## What happens during a run

`daedalus` owns the Claude run and checkpoints before configured actions.

Today that means:

- `Edit(*)`
- `MultiEdit(*)`
- `Write(*)`
- configured `Bash(...)` rules

`ddl init` writes the repo-local config at `.daedalus/config.json`:

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

## Recovery model

The current model is simple:

1. `ddl run -- claude ...`
2. Claude reads, edits, and runs commands
3. `daedalus` checkpoints before a matching risky action
4. the action goes bad
5. `ddl restore <checkpoint_id>` restores the workspace files
6. `ddl rewind <checkpoint_id>` resumes Claude from that checkpoint when Claude context is available

For Claude-backed runs owned by `daedalus`, checkpoints also capture:

- the Claude session id
- a best-effort local Claude rewind snapshot under `.daedalus/runtime/<run_id>/claude-checkpoints/<checkpoint_id>/`

That snapshot currently covers:

- `~/.claude/projects/<escaped-cwd>/<session_id>.jsonl`
- `~/.claude/file-history/<session_id>/`

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

- `ddl run` launches Claude from the repo root with checkpoint protection enabled
- `ddl shell` runs a shell command through the same checkpoint matcher
- `ddl restore` is destructive workspace-file recovery only
- `ddl rewind` is workspace recovery plus Claude resume when the checkpoint is Claude-backed and rewindable

## Current limits

- Claude Code only. Other runtimes are intentionally unsupported for now.
- `ddl rewind` only works for Claude-backed checkpoints when the workspace snapshot exists and the Claude local rewind snapshot was captured.
- If Claude context is unavailable, or the checkpoint is not Claude-backed, `ddl rewind` fails clearly and `ddl restore` remains available.
- `daedalus` does not snapshot, restore, or protect `.git`. If repo metadata is damaged, use Git or external recovery separately.
- External side effects outside the configured workspace are not rewound.
- The current Claude snapshot is best-effort and does not cover all of `~/.claude`, subagent state, or vendor UI state.
- Symlink snapshots are still rejected.
- `ddl restore` replaces the current workspace snapshot and removes files created after the checkpoint, while leaving `.git`, `.daedalus`, and `target` untouched.

## Status

The repo already has a working shell-first v1:

- repo-local `.daedalus/` state
- shadow git-backed snapshot storage
- automatic checkpointing before configured Bash rules
- Claude `PreToolUse` hook checkpointing for `Edit`, `MultiEdit`, `Write`, and `Bash`
- interactive `ddl log` recovery console

The main thing to evaluate now is whether Claude restore + rewind is a real workflow improvement in practice.

## Docs

- [Command semantics](docs/commands.md)
- [Architecture](docs/architecture.md)
- [State layout](docs/state-layout.md)

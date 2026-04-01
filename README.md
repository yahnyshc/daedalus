# daedalus

Version control for vibe coders.

Git owns history. `daedalus` protects both context and repo.

<img width="1111" height="663" alt="Screenshot 2026-04-01 at 19 40 08" src="https://github.com/user-attachments/assets/3a62120e-504c-49d9-b390-04ae73b16af1" />

`daedalus` checkpoints a Claude run before risky actions, then gives you two clean recovery moves:

- `ddl restore` puts the workspace back at the checkpoint
- `ddl rewind` restores the workspace and resumes the Claude-backed run when context was captured

This is for the moment when Claude already made useful progress, then one edit or shell command trashed the working state.

## The bad moment

```text
claude: "I cleaned things up"
you:     "why is half the repo gone"
```

`daedalus` is built for that exact failure mode:

- Claude is in the middle of a real run
- a risky edit or shell command goes wrong
- you want the workspace back without safety commits
- you want Claude back at the last safe point instead of starting over

## How it works

```text
run protected
    |
    v
checkpoint before Edit(*) / MultiEdit(*) / Write(*) / Bash(...)
    |
    v
bad action lands
    |
  +-+-------------------+
  |                     |
  v                     v
restore             rewind
files only          files + Claude-backed session resume
```

`daedalus` owns the Claude run and checkpoints before configured mutation boundaries.

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

## `daedalus` vs Claude Rewind

Claude Rewind is prompt-oriented. `daedalus` is action-oriented.

If you want to step back in the conversation, Claude Rewind is the right mental model. If you want a checkpoint right before `Bash(rm -rf tmp)` or some other risky tool action, that is what `daedalus` is for.

```text
Claude Rewind
  prompt -> prompt -> prompt
              ^
      rewind conversation state

daedalus
  Edit(*) -> Write(*) -> Bash(*) -> damage
                    ^
         checkpoint before impact
```

The practical difference:

- Claude Rewind operates at the prompt / conversation layer
- `daedalus` checkpoints before concrete tool actions
- `daedalus` covers file edits and configured shell commands
- if the blast radius came from Bash, that distinction matters

## Example log

A real `ddl log` screenshot belongs here.

The screenshot should sit here as proof that the recovery model is not conceptual. The hero explains the idea; the log proves the tool exists.

<!-- Insert real ddl log screenshot here -->

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

## What gets protected

```text
recovery scope:
  workspace files
  repo-local checkpoint metadata under .daedalus/
  Claude-backed local rewind snapshot when captured

checkpoint coverage:
  Edit(*)
  MultiEdit(*)
  Write(*)
  configured Bash(...)

not protected:
  .git internals
  external side effects outside the workspace
  all possible ~/.claude state
  unsupported runtimes
```

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

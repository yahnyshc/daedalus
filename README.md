# daedalus

`daedalus` is a checkpointing layer for coding agents.

It is built for the moment right before an agent does something unsafe, and for the moment right after you realize it was a mistake.

The goal is simple: restore the exact repo/workspace state and, when available, rewind the agent context immediately before a risky action.

## The Problem

Coding agents are no longer one-shot chats. They read files, edit code, install dependencies, run shell commands, ask for approvals, and carry plan state across long sessions.

When something goes wrong, git is usually not the right recovery primitive:

- the bad action may happen between commits
- you may not want to create noisy commits just to stay safe
- the damage may include more than source edits
- the agent's reasoning context matters, not just the diff

Today the relevant state is scattered across too many places:

- working tree changes
- generated files and local artifacts
- tool outputs the agent already consumed
- approval history
- conversation and plan state
- shell actions and their results

That makes recovery brittle. You can often get the files back. You usually cannot get back to the exact decision point cleanly.

## What v1 Is

`daedalus` v1 is a repo-local, agent-aware recovery tool.

It runs a supported agent under protection, saves checkpoints before configured unsafe shell actions, restores the workspace to a prior checkpoint, and reconstructs enough agent context to continue from that point.

The key promise is:

> Restore the exact repo/workspace state and the agent context immediately before a risky action, then rewind from there when agent context is available.

## Core Workflow

The public workflow is:

1. `ddl run -- <agent command>`
2. agent reads files, edits code, asks for approval
3. before a configured risky shell action, `daedalus` creates a checkpoint
4. the action goes bad
5. `ddl restore <checkpoint_id>`
6. workspace returns to the pre-action state
7. `ddl rewind <checkpoint_id>`
8. the agent continues from the saved context

This is why v1 is CLI-first. `daedalus` needs to own or observe the run in order to protect it properly.

## What v1 Stores

A checkpoint is not just a file diff.

For v1, a checkpoint is expected to include:

- workspace snapshot for the configured scope
- conversation transcript or transcript pointer owned by the wrapper/integration
- plan or todo state, if present
- tool outputs the agent has already seen
- approval history
- runtime fingerprint such as cwd, git HEAD/dirty state, selected environment, and relevant tool versions

The point is not to save everything the machine knows. The point is to save the minimum state needed to recreate what the agent knew and what the repo looked like immediately before a risky action.

## What v1 Does Not Do

v1 is intentionally narrow.

It is not:

- a replacement for git
- a full VM or container snapshot product
- a promise to rewind arbitrary external side effects outside the configured workspace
- a promise to mutate any vendor chat UI back in place

`daedalus` restores agent context for resumption. That is different from universally rewinding an existing Codex or Claude thread inside a host UI.

## Commands

The v1 CLI is intentionally small:

```bash
ddl init
ddl run -- <agent command>
ddl shell -- <command>
ddl log
ddl diff [checkpoint_a] [checkpoint_b]
ddl restore <checkpoint_id>
ddl rewind <checkpoint_id>
```

Command intent:

- `ddl init`: initialize `daedalus` state for the repo
- `ddl run`: execute a supported agent runtime under protection
- `ddl shell`: execute a shell command through `daedalus`' checkpoint matcher
- `ddl log`: inspect recent sessions in an interactive recovery console when attached to a TTY, or emit plain text in non-interactive contexts
- `ddl diff`: inspect file and metadata differences between checkpoints
- `ddl restore`: return workspace and checkpoint metadata to a known point without launching the agent
- `ddl rewind`: restore workspace and, when available, continue the same session from a checkpoint with saved agent context

Manual checkpoint commands are deliberately not in the first public story. The core value is automatic protection, not asking users to remember another save button.

## Config

`ddl init` writes a repo-local JSON config at `.daedalus/config.json`:

```json
{
  "checkpointing": {
    "before": [
      "Edit(*)",
      "MultiEdit(*)",
      "Write(*)",
      "Bash(npm install:*)",
      "Bash(git rebase:*)",
      "Bash(rm:*)",
      "Bash(mv:*)"
    ]
  }
}
```

v1 rule behavior is intentionally small:

- checkpoint matching uses a shared internal tool invocation model
- `Bash(prefix:*)` uses deterministic argv-prefix matching
- `Bash(command)` uses exact argv matching
- `:*` means any trailing args
- `Edit(*)`, `MultiEdit(*)`, `Write(*)`, and `Bash(...)` are enforced for Claude Code sessions launched through `ddl run -- claude ...`
- Codex remains Bash-only in this increment

Older repos that only have the legacy `.daedalus/config` file must be re-initialized or migrated. `daedalus` now fails clearly instead of silently skipping rule enforcement.

## Terms

- `checkpoint`: a saved execution point before an unsafe action
- `restore`: return workspace and checkpoint metadata to that point
- `rewind`: continue the same protected session from that checkpoint when agent context can be restored
- `session history`: the ordered history of protected actions for one run

## How It Works

The implementation goal is a repo-local hidden state store with content-addressed semantics and cheap checkpointing, diffing, restore, and rewind.

The exact storage internals can evolve. Using git primitives or a shadow git repository internally is acceptable. That is an implementation detail, not the product model.

The user model should stay simple:

- protected runs
- checkpoints
- restore
- rewind
- session history

## Integration Model

v1 should integrate in layers.

Primary:

- CLI wrapper around supported agent processes, currently `codex` and `claude`

Secondary:

- MCP server exposing checkpoint tools to agents explicitly

Tertiary:

- agent-specific hooks where supported for automatic checkpointing around tool execution

MCP matters because it lets agents call `restore`, `rewind`, `diff`, and `log` as first-class tools.

But MCP alone is not enough. The core value of `daedalus` is automatic protection before a risky action, which usually requires a wrapper or a hook surface.

Full `rewind` fidelity depends on `daedalus` owning or observing the run. If the agent was not run through `daedalus` or a supported integration, file restore may still work while rewind is unavailable.

For Claude-backed runs owned by `daedalus`, v1 now pins a Claude session id at run start and persists it in `.daedalus/runtime/<run_id>/session.meta`. Checkpoints also capture an experimental best-effort snapshot of Claude's local project transcript and file-history under `.daedalus/runtime/<run_id>/claude-checkpoints/<checkpoint_id>/` when those files exist.

For users, the decision is simpler:

- `ddl restore`: repo/workspace only
- `ddl rewind`: repo/workspace plus agent-context rewind when that checkpoint can actually provide it

For Claude-backed runs owned by `daedalus`, rewind is only considered available when the workspace snapshot exists and the experimental Claude local rewind snapshot exists. If that context is unavailable, `ddl rewind` fails clearly and the user should choose `ddl restore` instead.

## Demo Story

The first demo should prove one thing clearly:

An agent can make progress, take a bad action, and be brought back to the exact pre-action point without relying on git commits.

Example:

```bash
ddl run -- codex ...
```

During the run:

- the agent reads files and edits code
- the agent asks for approval
- before a configured shell command such as `rm -rf tmp`, `daedalus` creates checkpoint `cp_42`
- the command goes bad

Then:

```bash
ddl restore cp_42
ddl rewind cp_42
```

That is the product in one sequence.

## Why Rust

Rust fits the shape of this tool well:

- fast filesystem and process work
- strong control over storage and serialization
- easy distribution as a single binary
- good fit for a long-lived local systems tool

The main risk is not language choice. The main risk is scope. v1 only works if it stays narrow and credible.

## Roadmap

Near-term extensions after the core demo:

- MCP server backed by the same core engine
- hook-based integrations for supported agent runtimes
- richer checkpoint diff views across files, approvals, and tool outputs
- explicit export or sync flows into git when users want to turn a recovered session into source history

## Non-Goals for v1

- full machine snapshotting
- universal rollback of external side effects
- replacing git branches or commits
- deep vendor-specific promises that depend on undocumented rewind capabilities

## Status

This repo now includes a working shell-first v1 implementation. It keeps the repo-local state model and shadow git-backed checkpoint storage from the scaffold, but moves automatic checkpointing to wrapped mutation boundaries instead of creating checkpoints at run start. Claude Code sessions now also checkpoint before supported edit and Bash tools through an injected `PreToolUse` hook.

`ddl log` is now TTY-aware: in an interactive terminal it becomes a recovery console with recent sessions, protected action history, diff inspection, and direct restore/rewind actions. In non-interactive contexts it keeps the plain text log output.

The implementation should stay anchored to the same promise:

protect agent runs, checkpoint before risky actions, restore cleanly, then rewind from the exact decision point.

## Current Base

The current base provides:

- a Rust workspace with the `ddl` CLI crate
- repo-local `.daedalus/` state initialization with `.daedalus/config.json`
- session, run, and checkpoint metadata records
- shadow git-backed snapshot storage under `.daedalus/shadow/`
- `ddl run` wrapper mode for `codex` and `claude`
- `ddl shell` for direct shell execution through the matcher
- automatic checkpointing for configured `Bash(...)` rules
- Claude Code `PreToolUse` hook checkpointing for `Edit(*)`, `MultiEdit(*)`, `Write(*)`, and `Bash(...)`

The current enforcement surface is intentionally narrow:

- `Bash(...)` rules are enforced now
- checkpoint matching is routed through a shared internal tool invocation pipeline
- Claude Code enforces `Edit(*)`, `MultiEdit(*)`, `Write(*)`, and `Bash(...)` through a session-scoped hook when launched via `ddl run`
- Codex remains on the existing Bash-only path
- unsupported runtimes fail clearly instead of pretending they are protected

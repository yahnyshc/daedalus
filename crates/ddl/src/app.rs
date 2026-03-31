use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::io::Read;

use crate::error::{DdlError, Result};
use crate::log_ui::{LogUiExit, run_log_ui};
use crate::model::Resumability;
use crate::store::DaedalusStore;

pub fn run_cli(args: impl IntoIterator<Item = OsString>) -> Result<i32> {
    let arguments = parse_arguments(args)?;

    match arguments {
        CommandLine::Help => {
            print_help();
            Ok(0)
        }
        CommandLine::Init => {
            let store = DaedalusStore::discover()?;
            store.init()?;
            println!(
                "initialized daedalus state in {}",
                store.repo_root().join(".daedalus").display()
            );
            Ok(0)
        }
        CommandLine::Run { command } => {
            let store = DaedalusStore::discover()?;
            let result = store.run_agent(command)?;
            if let Some(checkpoint_id) = result.latest_checkpoint_id {
                println!(
                    "run {} finished on timeline {} with latest checkpoint {}",
                    result.run_id, result.timeline_id, checkpoint_id
                );
            } else {
                println!(
                    "run {} finished on timeline {} with no checkpoints yet",
                    result.run_id, result.timeline_id
                );
            }
            Ok(result.exit_code)
        }
        CommandLine::Shell { command } => {
            let store = DaedalusStore::discover()?;
            store.run_shell_command(command)
        }
        CommandLine::ClaudePreToolUseHook => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input)?;
            let store = DaedalusStore::discover()?;
            store.handle_claude_pre_tool_use(&input)
        }
        CommandLine::Log => {
            let store = DaedalusStore::discover()?;
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                match run_log_ui(&store)? {
                    LogUiExit::Quit => Ok(0),
                    LogUiExit::Rewind(checkpoint_id) => store.rewind(&checkpoint_id),
                }
            } else {
                print_log(&store)?;
                Ok(0)
            }
        }
        CommandLine::Diff {
            checkpoint_a,
            checkpoint_b,
        } => {
            let store = DaedalusStore::discover()?;
            let (a, b) = resolve_diff_targets(&store, checkpoint_a, checkpoint_b)?;
            let output = store.diff(&a, &b)?;
            if output.trim().is_empty() {
                println!("no differences between {a} and {b}");
            } else {
                print!("{output}");
            }
            Ok(0)
        }
        CommandLine::Restore { checkpoint } => {
            let store = DaedalusStore::discover()?;
            store.restore(&checkpoint)?;
            println!("restored workspace to checkpoint {checkpoint}");
            Ok(0)
        }
        CommandLine::Rewind { checkpoint } => {
            let store = DaedalusStore::discover()?;
            store.rewind(&checkpoint)
        }
        CommandLine::Fork { checkpoint, name } => {
            let store = DaedalusStore::discover()?;
            let (timeline_id, run_id) = store.fork(&checkpoint, name)?;
            println!("created fork timeline {timeline_id} with run {run_id}");
            Ok(0)
        }
    }
}

#[derive(Debug)]
enum CommandLine {
    Help,
    Init,
    Run {
        command: Vec<String>,
    },
    Shell {
        command: Vec<String>,
    },
    ClaudePreToolUseHook,
    Log,
    Diff {
        checkpoint_a: Option<String>,
        checkpoint_b: Option<String>,
    },
    Restore {
        checkpoint: String,
    },
    Rewind {
        checkpoint: String,
    },
    Fork {
        checkpoint: String,
        name: Option<String>,
    },
}

fn parse_arguments(args: impl IntoIterator<Item = OsString>) -> Result<CommandLine> {
    let parts = args
        .into_iter()
        .map(|item| item.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    if parts.len() <= 1 {
        return Ok(CommandLine::Help);
    }

    match parts[1].as_str() {
        "-h" | "--help" | "help" => Ok(CommandLine::Help),
        "init" => Ok(CommandLine::Init),
        "log" => Ok(CommandLine::Log),
        "internal" => parse_internal(parts),
        "run" => parse_run(parts),
        "shell" => parse_shell(parts),
        "diff" => parse_diff(parts),
        "restore" => parse_single_value(parts, "restore")
            .map(|checkpoint| CommandLine::Restore { checkpoint }),
        "rewind" => parse_single_value(parts, "rewind")
            .map(|checkpoint| CommandLine::Rewind { checkpoint }),
        "resume" => Err(DdlError::InvalidInput(
            "`ddl resume` was removed; use `ddl rewind <checkpoint_id>` for agent-context rewind or `ddl restore <checkpoint_id>` for repo-only recovery".to_string(),
        )),
        "fork" => {
            if parts.len() < 3 {
                return Err(DdlError::InvalidInput(
                    "usage: ddl fork <checkpoint_id> [name]".to_string(),
                ));
            }
            Ok(CommandLine::Fork {
                checkpoint: parts[2].clone(),
                name: parts.get(3).cloned(),
            })
        }
        other => Err(DdlError::InvalidInput(format!(
            "unknown command `{other}`; run `ddl --help` for usage"
        ))),
    }
}

fn parse_internal(parts: Vec<String>) -> Result<CommandLine> {
    match parts.get(2).map(String::as_str) {
        Some("claude-pre-tool-use") => Ok(CommandLine::ClaudePreToolUseHook),
        Some(other) => Err(DdlError::InvalidInput(format!(
            "unknown internal command `{other}`"
        ))),
        None => Err(DdlError::InvalidInput(
            "missing internal command".to_string(),
        )),
    }
}

fn parse_run(parts: Vec<String>) -> Result<CommandLine> {
    if parts.len() < 4 || parts[2] != "--" {
        return Err(DdlError::InvalidInput(
            "usage: ddl run -- <agent command>".to_string(),
        ));
    }

    Ok(CommandLine::Run {
        command: parts.into_iter().skip(3).collect(),
    })
}

fn parse_shell(parts: Vec<String>) -> Result<CommandLine> {
    if parts.len() < 4 || parts[2] != "--" {
        return Err(DdlError::InvalidInput(
            "usage: ddl shell -- <command>".to_string(),
        ));
    }

    Ok(CommandLine::Shell {
        command: parts.into_iter().skip(3).collect(),
    })
}

fn parse_diff(parts: Vec<String>) -> Result<CommandLine> {
    match parts.len() {
        2 => Ok(CommandLine::Diff {
            checkpoint_a: None,
            checkpoint_b: None,
        }),
        4 => Ok(CommandLine::Diff {
            checkpoint_a: Some(parts[2].clone()),
            checkpoint_b: Some(parts[3].clone()),
        }),
        _ => Err(DdlError::InvalidInput(
            "usage: ddl diff [checkpoint_a] [checkpoint_b]".to_string(),
        )),
    }
}

fn parse_single_value(parts: Vec<String>, name: &str) -> Result<String> {
    if parts.len() != 3 {
        return Err(DdlError::InvalidInput(format!(
            "usage: ddl {name} <checkpoint_id>"
        )));
    }
    Ok(parts[2].clone())
}

fn resolve_diff_targets(
    store: &DaedalusStore,
    checkpoint_a: Option<String>,
    checkpoint_b: Option<String>,
) -> Result<(String, String)> {
    match (checkpoint_a, checkpoint_b) {
        (Some(a), Some(b)) => Ok((a, b)),
        (None, None) => {
            let checkpoints = store.list_checkpoints()?;
            if checkpoints.len() < 2 {
                return Err(DdlError::InvalidInput(
                    "need at least two checkpoints to diff".to_string(),
                ));
            }
            let a = checkpoints[checkpoints.len() - 2].id.clone();
            let b = checkpoints[checkpoints.len() - 1].id.clone();
            Ok((a, b))
        }
        _ => Err(DdlError::InvalidInput(
            "either pass both checkpoint ids or neither".to_string(),
        )),
    }
}

fn print_log(store: &DaedalusStore) -> Result<()> {
    print!("{}", render_log(store)?);
    Ok(())
}

fn render_log(store: &DaedalusStore) -> Result<String> {
    let timelines = store.list_timelines()?;
    let checkpoints = store.list_checkpoints()?;
    let mut output = String::new();

    if timelines.is_empty() {
        output.push_str("no timelines recorded\n");
        return Ok(output);
    }

    for timeline in timelines {
        let label = timeline
            .name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        let _ = writeln!(output, "timeline {}{}", timeline.id, label);
        let _ = writeln!(output, "  run: {}", timeline.run_id);
        if let Some(root_checkpoint_id) = &timeline.root_checkpoint_id {
            let _ = writeln!(output, "  root checkpoint: {root_checkpoint_id}");
        } else {
            output.push_str("  root checkpoint: none yet\n");
        }
        if let Some(source) = &timeline.source_checkpoint_id {
            let _ = writeln!(output, "  source checkpoint: {source}");
        }

        for checkpoint in checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.timeline_id == timeline.id)
        {
            let trigger = checkpoint
                .trigger_command
                .as_deref()
                .map(|command| format!(" ({command})"))
                .unwrap_or_default();
            let _ = writeln!(
                output,
                "  checkpoint {} {}{}",
                checkpoint.id, checkpoint.reason, trigger
            );
            let restore_status = match checkpoint.resumability {
                Resumability::Unavailable => "Restore: unavailable",
                _ => "Restore: available",
            };
            let rewind_status = match (
                checkpoint.runtime_name.as_deref(),
                checkpoint.resumability.as_str(),
            ) {
                (_, "unavailable") => "Rewind: unavailable",
                (Some("claude"), "full") => "Rewind: available",
                (Some("claude"), "partial") => "Rewind: unavailable (restore only)",
                _ => "Rewind: available",
            };
            let _ = writeln!(output, "    {restore_status}");
            let _ = writeln!(output, "    {rewind_status}");
        }
    }

    Ok(output)
}

fn print_help() {
    println!(
        "\
daedalus v1 CLI scaffold

Usage:
  ddl init
  ddl run -- <agent command>
  ddl shell -- <command>
  ddl log
  ddl diff [checkpoint_a] [checkpoint_b]
  ddl restore <checkpoint_id>
  ddl rewind <checkpoint_id>
  ddl fork <checkpoint_id> [name]
"
    );
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::model::{
        CheckpointRecord, Resumability, RunRecord, RunStatus, RuntimeFingerprint, TimelineRecord,
    };
    use crate::store::DaedalusStore;

    use super::{CommandLine, parse_arguments, render_log};

    #[test]
    fn log_surfaces_claude_rewind_availability() {
        let repo_root = create_temp_repo("app-log");
        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        let created_at = 1;
        TimelineRecord {
            id: "tl_test".to_string(),
            name: Some("main".to_string()),
            run_id: "run_test".to_string(),
            root_checkpoint_id: Some("cp_test".to_string()),
            source_checkpoint_id: None,
            created_at,
        }
        .write(&repo_root.join(".daedalus/timelines/tl_test.meta"))
        .expect("write timeline");
        RunRecord {
            id: "run_test".to_string(),
            timeline_id: "tl_test".to_string(),
            command: vec!["claude".to_string()],
            created_at,
            status: RunStatus::Running,
            last_checkpoint_id: Some("cp_test".to_string()),
            resumability: Resumability::Full,
        }
        .write(&repo_root.join(".daedalus/runs/run_test.meta"))
        .expect("write run");
        let snapshot_path = repo_root.join(".daedalus/shadow/snapshots/cp_test");
        fs::create_dir_all(&snapshot_path).expect("create snapshot");
        let rewind_path = repo_root.join(".daedalus/runtime/run_test/claude-checkpoints/cp_full");
        fs::create_dir_all(&rewind_path).expect("create rewind snapshot");
        fs::write(rewind_path.join("marker.txt"), "saved").expect("write rewind marker");
        CheckpointRecord {
            id: "cp_test".to_string(),
            timeline_id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            parent_checkpoint_id: None,
            reason: "before-edit".to_string(),
            snapshot_rel_path: "snapshots/cp_test".to_string(),
            shadow_commit: "deadbeef".to_string(),
            created_at,
            resumability: Resumability::Partial,
            trigger_tool_type: Some("edit".to_string()),
            trigger_command: Some("src/main.rs".to_string()),
            runtime_name: Some("claude".to_string()),
            claude_session_id: None,
            claude_rewind_rel_path: None,
            fingerprint: RuntimeFingerprint {
                cwd: repo_root.display().to_string(),
                repo_root: repo_root.display().to_string(),
                git_head: "deadbeef".to_string(),
                git_branch: "main".to_string(),
                git_dirty: false,
                git_version: "git version".to_string(),
            },
        }
        .write(&repo_root.join(".daedalus/checkpoints/cp_test.meta"))
        .expect("write checkpoint");
        CheckpointRecord {
            id: "cp_full".to_string(),
            timeline_id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            parent_checkpoint_id: Some("cp_test".to_string()),
            reason: "before-shell".to_string(),
            snapshot_rel_path: "snapshots/cp_test".to_string(),
            shadow_commit: "cafebabe".to_string(),
            created_at: created_at + 1,
            resumability: Resumability::Full,
            trigger_tool_type: Some("bash".to_string()),
            trigger_command: Some("rm README.md".to_string()),
            runtime_name: Some("claude".to_string()),
            claude_session_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
            claude_rewind_rel_path: Some("runtime/run_test/claude-checkpoints/cp_full".to_string()),
            fingerprint: RuntimeFingerprint {
                cwd: repo_root.display().to_string(),
                repo_root: repo_root.display().to_string(),
                git_head: "cafebabe".to_string(),
                git_branch: "main".to_string(),
                git_dirty: false,
                git_version: "git version".to_string(),
            },
        }
        .write(&repo_root.join(".daedalus/checkpoints/cp_full.meta"))
        .expect("write full checkpoint");

        let output = render_log(&store).expect("render log");
        assert!(output.contains("checkpoint cp_test before-edit (src/main.rs)"));
        assert!(output.contains("Restore: available"));
        assert!(output.contains("Rewind: unavailable (restore only)"));
        assert!(output.contains("checkpoint cp_full before-shell (rm README.md)"));
        assert!(output.contains("Rewind: available"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn parse_arguments_accepts_rewind_and_rejects_resume() {
        match parse_arguments([
            OsString::from("ddl"),
            OsString::from("rewind"),
            OsString::from("cp_test"),
        ])
        .expect("parse rewind")
        {
            CommandLine::Rewind { checkpoint } => assert_eq!(checkpoint, "cp_test"),
            _ => panic!("expected rewind command"),
        }

        let error = parse_arguments([
            OsString::from("ddl"),
            OsString::from("resume"),
            OsString::from("cp_test"),
        ])
        .expect_err("resume should be removed");
        assert!(
            error
                .to_string()
                .contains("`ddl resume` was removed; use `ddl rewind <checkpoint_id>`")
        );
    }

    fn create_temp_repo(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ddl-app-test-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp repo");
        Command::new("git")
            .arg("init")
            .arg(&path)
            .output()
            .expect("git init");
        path
    }
}

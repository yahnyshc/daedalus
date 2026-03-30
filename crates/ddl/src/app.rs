use std::ffi::OsString;

use crate::error::{DdlError, Result};
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
        CommandLine::Log => {
            let store = DaedalusStore::discover()?;
            print_log(&store)?;
            Ok(0)
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
        CommandLine::Resume { checkpoint } => {
            let store = DaedalusStore::discover()?;
            store.resume(&checkpoint)
        }
        CommandLine::Fork { checkpoint, name } => {
            let store = DaedalusStore::discover()?;
            let (timeline_id, run_id) = store.fork(&checkpoint, name)?;
            println!("created fork timeline {timeline_id} with run {run_id}");
            Ok(0)
        }
    }
}

enum CommandLine {
    Help,
    Init,
    Run {
        command: Vec<String>,
    },
    Shell {
        command: Vec<String>,
    },
    Log,
    Diff {
        checkpoint_a: Option<String>,
        checkpoint_b: Option<String>,
    },
    Restore {
        checkpoint: String,
    },
    Resume {
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
        "run" => parse_run(parts),
        "shell" => parse_shell(parts),
        "diff" => parse_diff(parts),
        "restore" => parse_single_value(parts, "restore")
            .map(|checkpoint| CommandLine::Restore { checkpoint }),
        "resume" => {
            parse_single_value(parts, "resume").map(|checkpoint| CommandLine::Resume { checkpoint })
        }
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
    let timelines = store.list_timelines()?;
    let checkpoints = store.list_checkpoints()?;

    if timelines.is_empty() {
        println!("no timelines recorded");
        return Ok(());
    }

    for timeline in timelines {
        let label = timeline
            .name
            .as_deref()
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        println!("timeline {}{}", timeline.id, label);
        println!("  run: {}", timeline.run_id);
        if let Some(root_checkpoint_id) = &timeline.root_checkpoint_id {
            println!("  root checkpoint: {root_checkpoint_id}");
        } else {
            println!("  root checkpoint: none yet");
        }
        if let Some(source) = &timeline.source_checkpoint_id {
            println!("  source checkpoint: {source}");
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
            println!(
                "  checkpoint {} [{}] {}{}",
                checkpoint.id, checkpoint.resumability, checkpoint.reason, trigger
            );
        }
    }

    Ok(())
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
  ddl resume <checkpoint_id>
  ddl fork <checkpoint_id> [name]
"
    );
}

use std::env;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use crate::config::{CONFIG_FILE_NAME, split_command_words};
use crate::error::{DdlError, Result};
use crate::log_ui::{LogUiExit, run_log_ui};
use crate::presentation::{
    RecoveryCapability, continuation_label, format_absolute_time, latest_action_label,
    recovery_capability, session_status_label, session_title, tool_event_label, tool_event_preview,
};
use crate::store::{DaedalusStore, InitOutcome};

pub fn run_cli(args: impl IntoIterator<Item = OsString>) -> Result<i32> {
    let arguments = parse_arguments(args)?;

    match arguments {
        CommandLine::Help => {
            print_help();
            Ok(0)
        }
        CommandLine::Init => {
            let store = DaedalusStore::discover()?;
            let outcome = store.init()?;
            print!("{}", render_init_success(&store, outcome)?);
            Ok(0)
        }
        CommandLine::Config { action } => {
            let store = DaedalusStore::discover()?;
            match action {
                ConfigAction::Show => print!("{}", render_config(&store)?),
                ConfigAction::Path => {
                    let _ = store.read_config_text()?;
                    println!("{}", store.resolved_config_path()?.display());
                }
                ConfigAction::Edit => edit_config(&store)?,
            }
            Ok(0)
        }
        CommandLine::Where => {
            let store = DaedalusStore::discover()?;
            print!("{}", render_where(&store)?);
            Ok(0)
        }
        CommandLine::Run { command } => {
            let store = DaedalusStore::discover()?;
            let result = store.run_agent(command)?;
            if let Some(checkpoint_id) = result.latest_checkpoint_id {
                println!(
                    "run {} finished on timeline {} with latest checkpoint {} and session head {}",
                    result.run_id,
                    result.timeline_id,
                    checkpoint_id,
                    result.head_checkpoint_id.as_deref().unwrap_or("(missing)")
                );
            } else if let Some(head_checkpoint_id) = result.head_checkpoint_id {
                println!(
                    "run {} finished on timeline {} with session head {}",
                    result.run_id, result.timeline_id, head_checkpoint_id
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
    }
}

#[derive(Debug)]
enum CommandLine {
    Help,
    Init,
    Config {
        action: ConfigAction,
    },
    Where,
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
}

#[derive(Debug, Eq, PartialEq)]
enum ConfigAction {
    Show,
    Path,
    Edit,
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
        "config" => parse_config(parts),
        "where" => Ok(CommandLine::Where),
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
        other => Err(DdlError::InvalidInput(format!(
            "unknown command `{other}`; run `ddl --help` for usage"
        ))),
    }
}

fn parse_config(parts: Vec<String>) -> Result<CommandLine> {
    let action = match parts.get(2).map(String::as_str) {
        None => ConfigAction::Show,
        Some("path") => ConfigAction::Path,
        Some("edit") => ConfigAction::Edit,
        Some(other) => {
            return Err(DdlError::InvalidInput(format!(
                "unknown config command `{other}`; usage: ddl config [path|edit]"
            )));
        }
    };

    if parts.len() > 3 {
        return Err(DdlError::InvalidInput(
            "usage: ddl config [path|edit]".to_string(),
        ));
    }

    Ok(CommandLine::Config { action })
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
            "usage: ddl run -- claude <args...>".to_string(),
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

fn render_init_success(store: &DaedalusStore, outcome: InitOutcome) -> Result<String> {
    let state_dir = store.resolved_state_dir()?;
    let config_path = state_dir.join(CONFIG_FILE_NAME);
    let mut output = String::new();
    let status = match outcome {
        InitOutcome::Initialized => "initialized daedalus state in",
        InitOutcome::AlreadyInitialized => "daedalus state already initialized in",
    };
    let _ = writeln!(output, "{status} {}", state_dir.display());
    let _ = writeln!(
        output,
        "run `ddl config` to inspect rules or `ddl config edit` to change them"
    );
    let _ = writeln!(output, "config path: {}", config_path.display());
    Ok(output)
}

fn render_config(store: &DaedalusStore) -> Result<String> {
    let state_id = store.state_id()?;
    let config_path = store.resolved_config_path()?;
    let config_text = store.read_config_text()?;
    let mut output = String::new();
    let _ = writeln!(output, "Repo root: {}", store.repo_root().display());
    let _ = writeln!(output, "State id: {state_id}");
    let _ = writeln!(output, "Config path: {}", config_path.display());
    output.push('\n');
    output.push_str(&config_text);
    if !config_text.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn edit_config(store: &DaedalusStore) -> Result<()> {
    let config_path = store.resolved_config_path()?;
    let _ = store.read_config_text()?;
    let command_argv = editor_command_argv(env::var("EDITOR").ok().as_deref(), &config_path)?;
    let program = command_argv[0].clone();
    let status = Command::new(&program).args(&command_argv[1..]).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(DdlError::CommandFailed {
            program,
            status: status.code(),
            stderr: String::new(),
        })
    }
}

fn editor_command_argv(editor: Option<&str>, path: &Path) -> Result<Vec<String>> {
    let editor = editor
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            DdlError::InvalidInput(
                "EDITOR is not set; set `EDITOR` or use `ddl config path` to open the file manually"
                    .to_string(),
            )
        })?;
    let mut argv = split_command_words(editor).map_err(|error| {
        DdlError::InvalidInput(format!("invalid EDITOR value `{editor}`: {error}"))
    })?;
    if argv.is_empty() {
        return Err(DdlError::InvalidInput(
            "EDITOR is not set; set `EDITOR` or use `ddl config path` to open the file manually"
                .to_string(),
        ));
    }
    argv.push(path.display().to_string());
    Ok(argv)
}

fn render_where(store: &DaedalusStore) -> Result<String> {
    let state_dir = store.resolved_state_dir()?;
    let state_id = store.state_id()?;
    let config_path = store.resolved_config_path()?;
    let metadata_path = state_dir.join("store.meta");
    let mut output = String::new();
    let _ = writeln!(output, "Repo root: {}", store.repo_root().display());
    let _ = writeln!(output, "State id: {state_id}");
    let _ = writeln!(output, "State dir: {}", state_dir.display());
    let _ = writeln!(output, "Config: {}", config_path.display());
    let _ = writeln!(output, "Metadata: {}", metadata_path.display());
    Ok(output)
}

fn render_log(store: &DaedalusStore) -> Result<String> {
    let timelines = store.list_timelines()?;
    let checkpoints = store.list_checkpoints()?;
    let checkpoint_by_id = checkpoints
        .iter()
        .map(|checkpoint| (checkpoint.id.as_str(), checkpoint))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut output = String::new();

    if timelines.is_empty() {
        output.push_str("no sessions recorded\n");
        return Ok(output);
    }

    output.push_str("Recent Sessions\n");
    for timeline in timelines.into_iter().rev() {
        let run = store.read_run(&timeline.run_id)?;
        let mut session_checkpoints = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.timeline_id == timeline.id)
            .collect::<Vec<_>>();
        session_checkpoints.sort_by_key(|checkpoint| checkpoint.created_at);
        let session_head = run.head_checkpoint_id.as_deref().and_then(|head_id| {
            session_checkpoints
                .iter()
                .copied()
                .find(|item| item.id == head_id)
        });
        let protected_actions = session_checkpoints
            .iter()
            .copied()
            .filter(|checkpoint| checkpoint.kind == crate::model::CheckpointKind::ProtectedAction)
            .collect::<Vec<_>>();
        let mut recovery_points = Vec::new();
        if let Some(head) = session_head {
            recovery_points.push(head);
        }
        recovery_points.extend(protected_actions.iter().rev().copied());
        let latest_checkpoint = protected_actions.last().copied();
        let capability = recovery_points
            .first()
            .copied()
            .map(recovery_capability)
            .unwrap_or(RecoveryCapability::Unavailable);
        let continuation = continuation_label(
            run.rewind_source_checkpoint_id
                .as_deref()
                .and_then(|checkpoint_id| checkpoint_by_id.get(checkpoint_id).copied()),
        );

        let _ = writeln!(output, "{}", session_title(&timeline, &run));
        let _ = writeln!(
            output,
            "  Started: {}  |  {} protected actions  |  {}",
            format_absolute_time(timeline.created_at),
            protected_actions.len(),
            capability.label()
        );
        let _ = writeln!(
            output,
            "  Status: {}  |  Latest protected action: {}",
            session_status_label(&run.status),
            latest_checkpoint
                .map(|item| latest_action_label(Some(item)))
                .unwrap_or_else(|| latest_action_label(None),)
        );
        if let Some(continuation) = continuation {
            let _ = writeln!(output, "  {continuation}");
        }

        if recovery_points.is_empty() {
            output.push_str("  No recovery points recorded yet.\n");
        } else {
            output.push_str("  Recovery points:\n");
            for checkpoint in recovery_points {
                let _ = writeln!(
                    output,
                    "    {}  |  {}  |  {}",
                    tool_event_label(checkpoint),
                    crate::presentation::format_relative_time(checkpoint.created_at),
                    recovery_capability(checkpoint).label()
                );
                if let Some(preview) = tool_event_preview(checkpoint) {
                    let _ = writeln!(output, "      {preview}");
                }
            }
        }
    }

    Ok(output)
}

fn print_help() {
    println!(
        "\
daedalus v1 CLI

Usage:
  ddl init
  ddl config [path|edit]
  ddl where
  ddl run -- claude <args...>
  ddl shell -- <command>
  ddl log
  ddl diff [checkpoint_a] [checkpoint_b]
  ddl restore <checkpoint_id>
  ddl rewind <checkpoint_id>
"
    );
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::model::{
        CheckpointKind, CheckpointRecord, Resumability, RunRecord, RunStatus, RuntimeFingerprint,
        TimelineRecord,
    };
    use crate::store::{DaedalusStore, InitOutcome};

    use super::{
        CommandLine, ConfigAction, editor_command_argv, parse_arguments, render_config,
        render_init_success, render_log, render_where,
    };

    #[test]
    fn log_surfaces_claude_rewind_availability() {
        let repo_root = create_temp_repo("app-log");
        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let state_dir = store.state_dir().to_path_buf();

        let created_at = 1;
        TimelineRecord {
            id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            created_at,
        }
        .write(&state_dir.join("timelines/tl_test.meta"))
        .expect("write timeline");
        RunRecord {
            id: "run_test".to_string(),
            timeline_id: "tl_test".to_string(),
            command: vec!["claude".to_string()],
            created_at,
            status: RunStatus::Running,
            last_checkpoint_id: Some("cp_test".to_string()),
            head_checkpoint_id: Some("cp_head".to_string()),
            rewind_source_checkpoint_id: None,
            resumability: Resumability::Full,
        }
        .write(&state_dir.join("runs/run_test.meta"))
        .expect("write run");
        let snapshot_path = state_dir.join("shadow/snapshots/cp_test");
        fs::create_dir_all(&snapshot_path).expect("create snapshot");
        let head_snapshot_path = state_dir.join("shadow/snapshots/cp_head");
        fs::create_dir_all(&head_snapshot_path).expect("create head snapshot");
        let rewind_path = state_dir.join("runtime/run_test/claude-checkpoints/cp_full");
        fs::create_dir_all(&rewind_path).expect("create rewind snapshot");
        fs::write(rewind_path.join("marker.txt"), "saved").expect("write rewind marker");
        CheckpointRecord {
            id: "cp_test".to_string(),
            timeline_id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            kind: CheckpointKind::ProtectedAction,
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
        .write(&state_dir.join("checkpoints/cp_test.meta"))
        .expect("write checkpoint");
        CheckpointRecord {
            id: "cp_head".to_string(),
            timeline_id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            kind: CheckpointKind::SessionHead,
            parent_checkpoint_id: Some("cp_test".to_string()),
            reason: "session-head".to_string(),
            snapshot_rel_path: "snapshots/cp_head".to_string(),
            shadow_commit: "cafebabe".to_string(),
            created_at: created_at + 1,
            resumability: Resumability::Full,
            trigger_tool_type: None,
            trigger_command: None,
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
        .write(&state_dir.join("checkpoints/cp_head.meta"))
        .expect("write session head checkpoint");

        let output = render_log(&store).expect("render log");
        assert!(output.contains("Recent Sessions"));
        assert!(output.contains("Claude session"));
        assert!(output.contains("Status: Active"));
        assert!(output.contains("Recovery points:"));
        assert!(output.contains("Session Head"));
        assert!(output.contains("Rewindable"));
        assert!(output.contains("Edit src/main.rs"));
        assert!(output.contains("Restore only"));
        assert!(output.contains("Latest protected action: Edit src/main.rs"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn parse_arguments_accepts_rewind_and_rejects_removed_commands() {
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

        let error = parse_arguments([
            OsString::from("ddl"),
            OsString::from("fork"),
            OsString::from("cp_test"),
        ])
        .expect_err("fork should be removed");
        assert!(error.to_string().contains("unknown command `fork`"));
    }

    #[test]
    fn parse_arguments_accepts_where() {
        match parse_arguments([OsString::from("ddl"), OsString::from("where")])
            .expect("parse where")
        {
            CommandLine::Where => {}
            _ => panic!("expected where command"),
        }
    }

    #[test]
    fn parse_arguments_accepts_config_variants() {
        match parse_arguments([OsString::from("ddl"), OsString::from("config")])
            .expect("parse config")
        {
            CommandLine::Config { action } => assert_eq!(action, ConfigAction::Show),
            _ => panic!("expected config show command"),
        }

        match parse_arguments([
            OsString::from("ddl"),
            OsString::from("config"),
            OsString::from("path"),
        ])
        .expect("parse config path")
        {
            CommandLine::Config { action } => assert_eq!(action, ConfigAction::Path),
            _ => panic!("expected config path command"),
        }

        match parse_arguments([
            OsString::from("ddl"),
            OsString::from("config"),
            OsString::from("edit"),
        ])
        .expect("parse config edit")
        {
            CommandLine::Config { action } => assert_eq!(action, ConfigAction::Edit),
            _ => panic!("expected config edit command"),
        }
    }

    #[test]
    fn render_where_surfaces_state_location() {
        let repo_root = create_temp_repo("app-where");
        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        let output = render_where(&store).expect("render where");
        assert!(output.contains("Repo root:"));
        assert!(output.contains(&repo_root.display().to_string()));
        assert!(output.contains("State id:"));
        assert!(output.contains("State dir:"));
        assert!(output.contains("Config:"));
        assert!(output.contains("Metadata:"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn render_config_surfaces_config_contents() {
        let repo_root = create_temp_repo("app-config");
        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        let output = render_config(&store).expect("render config");
        assert!(output.contains("Repo root:"));
        assert!(output.contains("State id:"));
        assert!(output.contains("Config path:"));
        assert!(output.contains("\"checkpointing\""));
        assert!(output.contains("Bash(rm:*)"));
        assert!(!output.contains("Bash(npm install:*)"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn init_success_mentions_config_commands() {
        let repo_root = create_temp_repo("app-init-message");
        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        let output =
            render_init_success(&store, InitOutcome::Initialized).expect("render init success");
        assert!(output.contains("initialized daedalus state in"));
        assert!(output.contains("`ddl config`"));
        assert!(output.contains("`ddl config edit`"));
        assert!(output.contains("config path:"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn init_success_mentions_existing_state_when_reinitialized() {
        let repo_root = create_temp_repo("app-init-existing-message");
        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        let output = render_init_success(&store, InitOutcome::AlreadyInitialized)
            .expect("render init success");
        assert!(output.contains("daedalus state already initialized in"));
        assert!(output.contains("config path:"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn editor_command_argv_requires_editor() {
        let error = editor_command_argv(None, Path::new("/tmp/config.json"))
            .expect_err("missing editor should fail");
        assert!(error.to_string().contains("EDITOR is not set"));
    }

    #[test]
    fn editor_command_argv_appends_config_path() {
        let argv = editor_command_argv(Some("code --wait"), Path::new("/tmp/daedalus/config.json"))
            .expect("build editor argv");
        assert_eq!(argv, vec!["code", "--wait", "/tmp/daedalus/config.json"]);
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

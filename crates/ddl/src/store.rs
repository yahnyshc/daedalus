use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{CONFIG_FILE_NAME, DEFAULT_CONFIG_JSON, DaedalusConfig, ToolInvocation};
use crate::error::{DdlError, Result};
use crate::ids::next_id;
use crate::model::{
    CheckpointRecord, Resumability, RunRecord, RunStatus, RuntimeFingerprint,
    RuntimeMetadataRecord, TimelineRecord,
};
use crate::runtime::{
    ENV_REAL_SHELL, ShellWrapperContext, SupportedRuntime, apply_runtime_environment,
    current_shell_context, prepare_runtime_command,
};
use uuid::Uuid;

const STATE_DIR: &str = ".daedalus";
const SNAPSHOT_DIR_NAME: &str = "snapshots";
const LEGACY_CONFIG_FILE_NAME: &str = "config";
const PRESERVED_ROOTS: &[&str] = &[".git", ".daedalus", "target"];

#[derive(Clone, Debug)]
pub struct DaedalusStore {
    repo_root: PathBuf,
    state_dir: PathBuf,
    runs_dir: PathBuf,
    timelines_dir: PathBuf,
    checkpoints_dir: PathBuf,
    transcripts_dir: PathBuf,
    tool_outputs_dir: PathBuf,
    shadow_dir: PathBuf,
    snapshots_dir: PathBuf,
}

#[derive(Debug)]
pub struct RunInvocation {
    pub timeline_id: String,
    pub run_id: String,
    pub latest_checkpoint_id: Option<String>,
    pub exit_code: i32,
}

#[derive(Clone, Debug, Default)]
struct CheckpointTriggerMetadata {
    tool_type: Option<String>,
    command: Option<String>,
    runtime_name: Option<String>,
}

#[derive(Clone, Debug)]
struct StandaloneShellRun {
    timeline_id: String,
    run_id: String,
}

impl DaedalusStore {
    pub fn discover() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::discover_from(&cwd)
    }

    pub fn discover_from(cwd: &Path) -> Result<Self> {
        let repo_root = resolve_repo_root(cwd)?;
        let state_dir = repo_root.join(STATE_DIR);
        Ok(Self {
            repo_root,
            runs_dir: state_dir.join("runs"),
            timelines_dir: state_dir.join("timelines"),
            checkpoints_dir: state_dir.join("checkpoints"),
            transcripts_dir: state_dir.join("transcripts"),
            tool_outputs_dir: state_dir.join("tool_outputs"),
            shadow_dir: state_dir.join("shadow"),
            snapshots_dir: state_dir.join("shadow").join(SNAPSHOT_DIR_NAME),
            state_dir,
        })
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn init(&self) -> Result<()> {
        fs::create_dir_all(&self.runs_dir)?;
        fs::create_dir_all(&self.timelines_dir)?;
        fs::create_dir_all(&self.checkpoints_dir)?;
        fs::create_dir_all(&self.transcripts_dir)?;
        fs::create_dir_all(&self.tool_outputs_dir)?;
        fs::create_dir_all(&self.snapshots_dir)?;

        fs::write(self.state_dir.join(CONFIG_FILE_NAME), DEFAULT_CONFIG_JSON)?;
        let legacy_config = self.state_dir.join(LEGACY_CONFIG_FILE_NAME);
        if legacy_config.exists() {
            fs::remove_file(legacy_config)?;
        }

        if !self.shadow_dir.join(".git").exists() {
            run_command(
                Command::new("git")
                    .arg("init")
                    .arg(&self.shadow_dir)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped()),
                "git init",
            )?;
        }

        let _ = run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("config")
                .arg("user.name")
                .arg("daedalus")
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            "git config",
        );
        let _ = run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("config")
                .arg("user.email")
                .arg("daedalus@local")
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            "git config",
        );

        Ok(())
    }

    pub fn ensure_initialized(&self) -> Result<()> {
        if self.state_dir.exists() && self.shadow_dir.join(".git").exists() {
            Ok(())
        } else {
            Err(DdlError::NotInitialized(self.repo_root.clone()))
        }
    }

    pub fn run_agent(&self, command: Vec<String>) -> Result<RunInvocation> {
        self.ensure_initialized()?;
        self.load_config()?;
        let runtime = SupportedRuntime::detect(&command)?;

        let created_at = unix_timestamp();
        let run_id = next_id("run");
        let timeline_id = next_id("tl");

        let timeline = TimelineRecord {
            id: timeline_id.clone(),
            name: Some("main".to_string()),
            run_id: run_id.clone(),
            root_checkpoint_id: None,
            source_checkpoint_id: None,
            created_at,
        };
        self.write_timeline(&timeline)?;

        let mut run = RunRecord {
            id: run_id.clone(),
            timeline_id: timeline_id.clone(),
            command: command.clone(),
            created_at,
            status: RunStatus::Running,
            last_checkpoint_id: None,
            resumability: self.initial_run_resumability(runtime),
        };
        self.write_run(&run)?;
        let runtime_metadata = self.initialize_runtime_metadata(&run_id, runtime)?;
        if runtime == SupportedRuntime::Claude && runtime_metadata.is_some() {
            run.resumability = Resumability::Full;
            self.write_run(&run)?;
        }

        let shell_context = ShellWrapperContext {
            run_id: run_id.clone(),
            timeline_id: timeline_id.clone(),
            runtime,
            claude_session_id: runtime_metadata
                .as_ref()
                .and_then(|metadata| metadata.claude_session_id.clone()),
        };
        let status = self.execute_owned_command(&command, Some(&shell_context))?;

        run = self.read_run(&run_id)?;
        run.status = if status.success() {
            RunStatus::Succeeded
        } else {
            RunStatus::Failed
        };
        self.write_run(&run)?;

        Ok(RunInvocation {
            timeline_id,
            run_id,
            latest_checkpoint_id: run.last_checkpoint_id,
            exit_code: status.code().unwrap_or(1),
        })
    }

    pub fn run_shell_command(&self, command: Vec<String>) -> Result<i32> {
        self.ensure_initialized()?;
        if command.is_empty() {
            return Err(DdlError::InvalidInput(
                "missing command after `ddl shell --`".to_string(),
            ));
        }

        let config = self.load_config()?;
        let real_shell = env::var(ENV_REAL_SHELL).ok();
        let invocation = match real_shell.as_deref() {
            Some(_) => ToolInvocation::from_shell_args(&command),
            None => ToolInvocation::from_shell_command(command.clone()),
        };

        let mut standalone = None;
        if config.matching_rule(&invocation).is_some() {
            standalone = match current_shell_context() {
                Some(context) => {
                    let invocation = invocation
                        .clone()
                        .with_runtime_name(context.runtime.as_str());
                    self.record_tool_checkpoint(
                        &context.timeline_id,
                        &context.run_id,
                        &invocation,
                    )?;
                    None
                }
                None => {
                    let run = self.create_standalone_shell_run(&command)?;
                    self.record_tool_checkpoint(&run.timeline_id, &run.run_id, &invocation)?;
                    Some(run)
                }
            };
        }

        let status = self.execute_shell_command(&command, real_shell.as_deref())?;

        if let Some(run) = standalone {
            let mut record = self.read_run(&run.run_id)?;
            record.status = if status.success() {
                RunStatus::Succeeded
            } else {
                RunStatus::Failed
            };
            self.write_run(&record)?;
        }

        Ok(status.code().unwrap_or(1))
    }

    pub fn handle_claude_pre_tool_use(&self, raw: &str) -> Result<i32> {
        self.ensure_initialized()?;
        let Some(invocation) = ToolInvocation::from_claude_pre_tool_use(raw)? else {
            return Ok(0);
        };

        let config = self.load_config()?;
        if config.matching_rule(&invocation).is_none() {
            return Ok(0);
        }

        let context = current_shell_context().ok_or_else(|| {
            DdlError::InvalidState(
                "missing daedalus runtime context for Claude hook invocation".to_string(),
            )
        })?;
        if context.runtime != SupportedRuntime::Claude {
            return Err(DdlError::InvalidState(format!(
                "Claude hook invoked under unexpected runtime `{}`",
                context.runtime.as_str()
            )));
        }

        let invocation = invocation.with_runtime_name(context.runtime.as_str());
        self.record_tool_checkpoint(&context.timeline_id, &context.run_id, &invocation)?;
        Ok(0)
    }

    pub fn resume(&self, checkpoint_id: &str) -> Result<i32> {
        self.ensure_initialized()?;
        let checkpoint = self.read_checkpoint(checkpoint_id)?;
        if checkpoint.resumability != Resumability::Full {
            return Err(DdlError::InvalidInput(format!(
                "checkpoint `{checkpoint_id}` cannot be resumed"
            )));
        }

        let mut run = self.read_run(&checkpoint.run_id)?;
        self.restore(checkpoint_id)?;

        run.last_checkpoint_id = Some(checkpoint.id.clone());
        run.status = RunStatus::Running;
        self.write_run(&run)?;

        let runtime = SupportedRuntime::detect(&run.command).ok();
        let mut resume_command = run.command.clone();
        if runtime.is_some() {
            self.load_config()?;
        }
        let shell_context = runtime.map(|runtime| {
            if runtime == SupportedRuntime::Claude {
                resume_command = self.claude_resume_command(&run.command, &checkpoint);
            }

            ShellWrapperContext {
                run_id: run.id.clone(),
                timeline_id: checkpoint.timeline_id.clone(),
                runtime,
                claude_session_id: None,
            }
        });

        let status = self.execute_owned_command(&resume_command, shell_context.as_ref())?;
        run.status = if status.success() {
            RunStatus::Succeeded
        } else {
            RunStatus::Failed
        };
        self.write_run(&run)?;

        Ok(status.code().unwrap_or(1))
    }

    pub fn fork(&self, checkpoint_id: &str, name: Option<String>) -> Result<(String, String)> {
        self.ensure_initialized()?;
        let source = self.read_checkpoint(checkpoint_id)?;
        let source_run = self.read_run(&source.run_id)?;

        let timeline_id = next_id("tl");
        let run_id = next_id("run");
        let checkpoint = self.clone_checkpoint(
            &source,
            &timeline_id,
            &run_id,
            "fork-root".to_string(),
            None,
        )?;

        let timeline = TimelineRecord {
            id: timeline_id.clone(),
            name,
            run_id: run_id.clone(),
            root_checkpoint_id: Some(checkpoint.id.clone()),
            source_checkpoint_id: Some(source.id),
            created_at: unix_timestamp(),
        };
        self.write_timeline(&timeline)?;

        let run = RunRecord {
            id: run_id.clone(),
            timeline_id: timeline_id.clone(),
            command: source_run.command,
            created_at: unix_timestamp(),
            status: RunStatus::Forked,
            last_checkpoint_id: Some(checkpoint.id),
            resumability: checkpoint.resumability.clone(),
        };
        self.write_run(&run)?;

        Ok((timeline_id, run_id))
    }

    pub fn restore(&self, checkpoint_id: &str) -> Result<()> {
        self.ensure_initialized()?;
        let checkpoint = self.read_checkpoint(checkpoint_id)?;
        let snapshot_path = self.snapshot_path(&checkpoint.snapshot_rel_path);
        if !snapshot_path.exists() {
            return Err(DdlError::InvalidState(format!(
                "snapshot for checkpoint `{checkpoint_id}` is missing"
            )));
        }

        for entry in fs::read_dir(&self.repo_root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if PRESERVED_ROOTS.contains(&name_lossy.as_ref()) {
                continue;
            }

            let entry_path = entry.path();
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry_path)?;
            } else {
                fs::remove_file(entry_path)?;
            }
        }

        copy_dir_contents(&snapshot_path, &self.repo_root)?;
        Ok(())
    }

    pub fn diff(&self, checkpoint_a: &str, checkpoint_b: &str) -> Result<String> {
        self.ensure_initialized()?;
        let a = self.read_checkpoint(checkpoint_a)?;
        let b = self.read_checkpoint(checkpoint_b)?;
        let path_a = self.snapshot_path(&a.snapshot_rel_path);
        let path_b = self.snapshot_path(&b.snapshot_rel_path);

        let output = Command::new("git")
            .arg("--no-pager")
            .arg("diff")
            .arg("--no-index")
            .arg("--")
            .arg(&path_a)
            .arg(&path_b)
            .output()?;

        match output.status.code() {
            Some(0) | Some(1) => Ok(String::from_utf8_lossy(&output.stdout).to_string()),
            status => Err(DdlError::CommandFailed {
                program: "git diff --no-index".to_string(),
                status,
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            }),
        }
    }

    pub fn list_timelines(&self) -> Result<Vec<TimelineRecord>> {
        self.ensure_initialized()?;
        let mut items = Vec::new();
        for path in list_meta_files(&self.timelines_dir)? {
            items.push(TimelineRecord::read(&path)?);
        }
        items.sort_by_key(|item| item.created_at);
        Ok(items)
    }

    pub fn list_checkpoints(&self) -> Result<Vec<CheckpointRecord>> {
        self.ensure_initialized()?;
        let mut items = Vec::new();
        for path in list_meta_files(&self.checkpoints_dir)? {
            let checkpoint = CheckpointRecord::read(&path)?;
            items.push(self.with_live_resumability(checkpoint));
        }
        items.sort_by_key(|item| item.created_at);
        Ok(items)
    }

    pub fn read_run(&self, run_id: &str) -> Result<RunRecord> {
        let path = self.runs_dir.join(format!("{run_id}.meta"));
        if !path.exists() {
            return Err(DdlError::NotFound {
                kind: "run",
                id: run_id.to_string(),
            });
        }
        RunRecord::read(&path)
    }

    pub fn read_timeline(&self, timeline_id: &str) -> Result<TimelineRecord> {
        let path = self.timelines_dir.join(format!("{timeline_id}.meta"));
        if !path.exists() {
            return Err(DdlError::NotFound {
                kind: "timeline",
                id: timeline_id.to_string(),
            });
        }
        TimelineRecord::read(&path)
    }

    pub fn read_checkpoint(&self, checkpoint_id: &str) -> Result<CheckpointRecord> {
        let path = self.checkpoints_dir.join(format!("{checkpoint_id}.meta"));
        if !path.exists() {
            return Err(DdlError::NotFound {
                kind: "checkpoint",
                id: checkpoint_id.to_string(),
            });
        }
        Ok(self.with_live_resumability(CheckpointRecord::read(&path)?))
    }

    fn load_config(&self) -> Result<DaedalusConfig> {
        let path = self.state_dir.join(CONFIG_FILE_NAME);
        if !path.exists() {
            return Err(DdlError::InvalidConfig(format!(
                "daedalus checkpointing is not configured in {}; re-run `ddl init` or migrate to {}",
                self.repo_root.display(),
                path.display()
            )));
        }

        let raw = fs::read_to_string(&path)?;
        DaedalusConfig::parse(&raw)
    }

    fn write_run(&self, run: &RunRecord) -> Result<()> {
        run.write(&self.runs_dir.join(format!("{}.meta", run.id)))
    }

    fn write_timeline(&self, timeline: &TimelineRecord) -> Result<()> {
        timeline.write(&self.timelines_dir.join(format!("{}.meta", timeline.id)))
    }

    fn write_checkpoint(&self, checkpoint: &CheckpointRecord) -> Result<()> {
        checkpoint.write(&self.checkpoints_dir.join(format!("{}.meta", checkpoint.id)))
    }

    fn write_runtime_metadata(&self, run_id: &str, metadata: &RuntimeMetadataRecord) -> Result<()> {
        let path = self.runtime_metadata_path(run_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        metadata.write(&path)
    }

    fn create_standalone_shell_run(&self, command: &[String]) -> Result<StandaloneShellRun> {
        let created_at = unix_timestamp();
        let run_id = next_id("run");
        let timeline_id = next_id("tl");

        let timeline = TimelineRecord {
            id: timeline_id.clone(),
            name: Some("shell".to_string()),
            run_id: run_id.clone(),
            root_checkpoint_id: None,
            source_checkpoint_id: None,
            created_at,
        };
        self.write_timeline(&timeline)?;

        let run = RunRecord {
            id: run_id.clone(),
            timeline_id: timeline_id.clone(),
            command: command.to_vec(),
            created_at,
            status: RunStatus::Running,
            last_checkpoint_id: None,
            resumability: Resumability::Full,
        };
        self.write_run(&run)?;

        Ok(StandaloneShellRun {
            timeline_id,
            run_id,
        })
    }

    fn initial_run_resumability(&self, runtime: SupportedRuntime) -> Resumability {
        match runtime {
            SupportedRuntime::Claude => Resumability::Partial,
            SupportedRuntime::Codex => Resumability::Full,
        }
    }

    fn initialize_runtime_metadata(
        &self,
        run_id: &str,
        runtime: SupportedRuntime,
    ) -> Result<Option<RuntimeMetadataRecord>> {
        match runtime {
            SupportedRuntime::Claude => {
                let metadata = RuntimeMetadataRecord {
                    runtime_name: runtime.as_str().to_string(),
                    claude_session_id: Some(Uuid::new_v4().to_string()),
                };
                self.write_runtime_metadata(run_id, &metadata)?;
                Ok(Some(metadata))
            }
            SupportedRuntime::Codex => Ok(None),
        }
    }

    fn read_runtime_metadata(&self, run_id: &str) -> Result<Option<RuntimeMetadataRecord>> {
        let path = self.runtime_metadata_path(run_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(RuntimeMetadataRecord::read(&path)?))
    }

    fn record_tool_checkpoint(
        &self,
        timeline_id: &str,
        run_id: &str,
        invocation: &ToolInvocation,
    ) -> Result<CheckpointRecord> {
        let mut run = self.read_run(run_id)?;
        let mut timeline = self.read_timeline(timeline_id)?;

        let checkpoint = self.create_checkpoint_internal(
            timeline_id,
            run_id,
            run.last_checkpoint_id.clone(),
            invocation.reason().to_string(),
            CheckpointTriggerMetadata {
                tool_type: Some(invocation.tool.to_string()),
                command: Some(invocation.display.clone()),
                runtime_name: invocation.runtime_name.clone(),
            },
        )?;

        run.last_checkpoint_id = Some(checkpoint.id.clone());
        self.write_run(&run)?;

        if timeline.root_checkpoint_id.is_none() {
            timeline.root_checkpoint_id = Some(checkpoint.id.clone());
            self.write_timeline(&timeline)?;
        }

        Ok(checkpoint)
    }

    fn execute_owned_command(
        &self,
        command: &[String],
        shell_context: Option<&ShellWrapperContext>,
    ) -> Result<ExitStatus> {
        let prepared_command = match shell_context {
            Some(context) => prepare_runtime_command(command, &self.state_dir, context)?,
            None => command.to_vec(),
        };

        let mut process = Command::new(&prepared_command[0]);
        process
            .args(prepared_command.iter().skip(1))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        if let Some(context) = shell_context {
            apply_runtime_environment(&mut process, &self.repo_root, &self.state_dir, context)?;
        } else {
            process.current_dir(&self.repo_root);
        }

        Ok(process.status()?)
    }

    fn execute_shell_command(
        &self,
        command: &[String],
        real_shell: Option<&str>,
    ) -> Result<ExitStatus> {
        let mut process = match real_shell {
            Some(program) => {
                let mut process = Command::new(program);
                process.args(command);
                process
            }
            None => {
                let mut process = Command::new(&command[0]);
                process.args(command.iter().skip(1));
                process
            }
        };

        process
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        Ok(process.status()?)
    }

    fn create_checkpoint_internal(
        &self,
        timeline_id: &str,
        run_id: &str,
        parent_checkpoint_id: Option<String>,
        reason: String,
        trigger: CheckpointTriggerMetadata,
    ) -> Result<CheckpointRecord> {
        let checkpoint_id = next_id("cp");
        let snapshot_rel_path = format!("{SNAPSHOT_DIR_NAME}/{checkpoint_id}");
        let snapshot_path = self.snapshot_path(&snapshot_rel_path);
        if snapshot_path.exists() {
            fs::remove_dir_all(&snapshot_path)?;
        }
        fs::create_dir_all(&snapshot_path)?;
        copy_workspace_to_snapshot(&self.repo_root, &snapshot_path)?;
        let runtime = self.runtime_for_run(run_id);
        let runtime_name = trigger
            .runtime_name
            .clone()
            .or_else(|| runtime.map(|runtime| runtime.as_str().to_string()));
        let claude_session_id = match runtime {
            Some(SupportedRuntime::Claude) => self
                .read_runtime_metadata(run_id)?
                .and_then(|metadata| metadata.claude_session_id)
                .filter(|value| Self::is_valid_claude_session_id(value)),
            _ => None,
        };

        run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("add")
                .arg(&snapshot_rel_path)
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            "git add",
        )?;

        run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("commit")
                .arg("-m")
                .arg(format!("checkpoint {checkpoint_id}"))
                .arg("--allow-empty")
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            "git commit",
        )?;

        let shadow_commit = read_git_output(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("rev-parse")
                .arg("HEAD"),
            "git rev-parse",
        )?;

        let checkpoint = CheckpointRecord {
            id: checkpoint_id,
            timeline_id: timeline_id.to_string(),
            run_id: run_id.to_string(),
            parent_checkpoint_id,
            reason,
            snapshot_rel_path,
            shadow_commit,
            created_at: unix_timestamp(),
            resumability: self.compute_checkpoint_resumability(
                &snapshot_path,
                runtime_name.as_deref(),
                claude_session_id.as_deref(),
            ),
            trigger_tool_type: trigger.tool_type,
            trigger_command: trigger.command,
            runtime_name,
            claude_session_id,
            fingerprint: self.capture_fingerprint()?,
        };
        self.write_checkpoint(&checkpoint)?;
        Ok(checkpoint)
    }

    fn clone_checkpoint(
        &self,
        source: &CheckpointRecord,
        timeline_id: &str,
        run_id: &str,
        reason: String,
        parent_checkpoint_id: Option<String>,
    ) -> Result<CheckpointRecord> {
        let checkpoint_id = next_id("cp");
        let snapshot_rel_path = format!("{SNAPSHOT_DIR_NAME}/{checkpoint_id}");
        let new_snapshot = self.snapshot_path(&snapshot_rel_path);
        fs::create_dir_all(&new_snapshot)?;
        let source_snapshot = self.snapshot_path(&source.snapshot_rel_path);
        copy_dir_contents(&source_snapshot, &new_snapshot)?;

        run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("add")
                .arg(&snapshot_rel_path)
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            "git add",
        )?;

        run_command(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("commit")
                .arg("-m")
                .arg(format!("fork checkpoint {checkpoint_id}"))
                .arg("--allow-empty")
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
            "git commit",
        )?;

        let shadow_commit = read_git_output(
            Command::new("git")
                .arg("-C")
                .arg(&self.shadow_dir)
                .arg("rev-parse")
                .arg("HEAD"),
            "git rev-parse",
        )?;

        let checkpoint = CheckpointRecord {
            id: checkpoint_id,
            timeline_id: timeline_id.to_string(),
            run_id: run_id.to_string(),
            parent_checkpoint_id,
            reason,
            snapshot_rel_path,
            shadow_commit,
            created_at: unix_timestamp(),
            resumability: self.compute_checkpoint_resumability(
                &new_snapshot,
                source.runtime_name.as_deref(),
                source.claude_session_id.as_deref(),
            ),
            trigger_tool_type: None,
            trigger_command: None,
            runtime_name: source.runtime_name.clone(),
            claude_session_id: source.claude_session_id.clone(),
            fingerprint: source.fingerprint.clone(),
        };
        self.write_checkpoint(&checkpoint)?;
        Ok(checkpoint)
    }

    fn capture_fingerprint(&self) -> Result<RuntimeFingerprint> {
        let git_head = read_git_output_or_default(
            Command::new("git")
                .arg("-C")
                .arg(&self.repo_root)
                .arg("rev-parse")
                .arg("HEAD"),
            "git rev-parse HEAD",
            "(unborn)",
        )?;
        let git_branch = read_git_output_or_default(
            Command::new("git")
                .arg("-C")
                .arg(&self.repo_root)
                .arg("rev-parse")
                .arg("--abbrev-ref")
                .arg("HEAD"),
            "git rev-parse --abbrev-ref HEAD",
            "(detached)",
        )?;
        let dirty_status = read_git_output(
            Command::new("git")
                .arg("-C")
                .arg(&self.repo_root)
                .arg("status")
                .arg("--porcelain"),
            "git status --porcelain",
        )?;
        let git_version = read_git_output(Command::new("git").arg("--version"), "git --version")?;

        Ok(RuntimeFingerprint {
            cwd: std::env::current_dir()?.display().to_string(),
            repo_root: self.repo_root.display().to_string(),
            git_head,
            git_branch,
            git_dirty: !dirty_status.trim().is_empty(),
            git_version,
        })
    }

    fn snapshot_path(&self, relative: &str) -> PathBuf {
        self.shadow_dir.join(relative)
    }

    fn runtime_metadata_path(&self, run_id: &str) -> PathBuf {
        self.state_dir
            .join("runtime")
            .join(run_id)
            .join("session.meta")
    }

    fn runtime_for_run(&self, run_id: &str) -> Option<SupportedRuntime> {
        self.read_run(run_id)
            .ok()
            .and_then(|run| SupportedRuntime::detect(&run.command).ok())
    }

    fn with_live_resumability(&self, mut checkpoint: CheckpointRecord) -> CheckpointRecord {
        let snapshot_path = self.snapshot_path(&checkpoint.snapshot_rel_path);
        checkpoint.resumability = self.compute_checkpoint_resumability(
            &snapshot_path,
            checkpoint.runtime_name.as_deref(),
            checkpoint.claude_session_id.as_deref(),
        );
        checkpoint
    }

    fn compute_checkpoint_resumability(
        &self,
        snapshot_path: &Path,
        runtime_name: Option<&str>,
        claude_session_id: Option<&str>,
    ) -> Resumability {
        if !snapshot_path.exists() {
            return Resumability::Unavailable;
        }

        if runtime_name == Some("claude")
            && !claude_session_id.is_some_and(Self::is_valid_claude_session_id)
        {
            return Resumability::Partial;
        }

        Resumability::Full
    }

    fn claude_resume_command(
        &self,
        original_command: &[String],
        checkpoint: &CheckpointRecord,
    ) -> Vec<String> {
        let session_id = checkpoint
            .claude_session_id
            .as_deref()
            .filter(|value| Self::is_valid_claude_session_id(value))
            .unwrap_or_default();
        vec![
            original_command[0].clone(),
            "--resume".to_string(),
            session_id.to_string(),
        ]
    }

    fn is_valid_claude_session_id(value: &str) -> bool {
        Uuid::parse_str(value).is_ok()
    }
}

fn resolve_repo_root(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()?;

    if output.status.success() {
        Ok(PathBuf::from(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Err(DdlError::InvalidInput(
            "daedalus must run inside a git repository".to_string(),
        ))
    }
}

fn list_meta_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension() == Some(OsStr::new("meta")) {
            files.push(path);
        }
    }
    Ok(files)
}

fn copy_workspace_to_snapshot(repo_root: &Path, snapshot_path: &Path) -> Result<()> {
    for entry in fs::read_dir(repo_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_lossy = name.to_string_lossy();
        if PRESERVED_ROOTS.contains(&name_lossy.as_ref()) {
            continue;
        }

        let destination = snapshot_path.join(&name);
        copy_path(&entry.path(), &destination)?;
    }
    Ok(())
}

fn copy_dir_contents(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        copy_path(&entry.path(), &target)?;
    }
    Ok(())
}

fn copy_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        copy_dir_contents(source, destination)?;
        return Ok(());
    }

    if metadata.file_type().is_symlink() {
        return Err(DdlError::InvalidInput(format!(
            "symlink snapshots are not supported yet: {}",
            source.display()
        )));
    }

    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

fn run_command(command: &mut Command, label: &str) -> Result<()> {
    let output = command.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(DdlError::CommandFailed {
            program: label.to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn read_git_output(command: &mut Command, label: &str) -> Result<String> {
    let output = command.output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(DdlError::CommandFailed {
            program: label.to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}

fn read_git_output_or_default(
    command: &mut Command,
    _label: &str,
    default: &str,
) -> Result<String> {
    let output = command.output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Ok(default.to_string())
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};

    use crate::config::{CONFIG_FILE_NAME, DEFAULT_CONFIG_JSON};
    use crate::model::{Resumability, RunRecord, RunStatus, RuntimeMetadataRecord, TimelineRecord};
    use crate::runtime::{ENV_RUN_ID, ENV_RUNTIME, ENV_TIMELINE_ID};

    use super::DaedalusStore;

    fn test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn init_creates_state_directories_and_json_config() {
        let repo_root = create_temp_repo("init");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        assert!(repo_root.join(".daedalus").exists());
        assert!(repo_root.join(".daedalus/shadow/.git").exists());
        assert!(repo_root.join(".daedalus/checkpoints").exists());
        assert_eq!(
            fs::read_to_string(repo_root.join(".daedalus").join(CONFIG_FILE_NAME))
                .expect("read config"),
            DEFAULT_CONFIG_JSON
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn shell_command_creates_checkpoint_before_matching_mutation() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("shell-match");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        fs::write(repo_root.join("test.txt"), "hello").expect("seed file");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");

        let exit_code = store
            .run_shell_command(vec!["rm".to_string(), "test.txt".to_string()])
            .expect("run shell");
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        assert!(!repo_root.join("test.txt").exists());

        let timelines = store.list_timelines().expect("list timelines");
        assert_eq!(timelines.len(), 1);
        assert_eq!(timelines[0].name.as_deref(), Some("shell"));

        let checkpoints = store.list_checkpoints().expect("list checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].timeline_id, timelines[0].id);
        assert_eq!(checkpoints[0].reason, "before-shell");
        assert_eq!(
            checkpoints[0].trigger_command.as_deref(),
            Some("rm test.txt")
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn shell_command_skips_non_matching_commands() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("shell-skip");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");

        let exit_code = store
            .run_shell_command(vec!["pwd".to_string()])
            .expect("run shell");
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        assert!(store.list_timelines().expect("list timelines").is_empty());
        assert!(
            store
                .list_checkpoints()
                .expect("list checkpoints")
                .is_empty()
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn shell_command_uses_existing_runtime_context() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("shell-runtime");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        fs::write(repo_root.join("test.txt"), "hello").expect("seed file");

        let (run_id, timeline_id) = create_active_run(&store, "codex");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        unsafe {
            std::env::set_var(ENV_RUN_ID, &run_id);
            std::env::set_var(ENV_TIMELINE_ID, &timeline_id);
            std::env::set_var(ENV_RUNTIME, "codex");
        }

        let exit_code = store
            .run_shell_command(vec!["rm".to_string(), "test.txt".to_string()])
            .expect("run shell");

        unsafe {
            std::env::remove_var(ENV_RUN_ID);
            std::env::remove_var(ENV_TIMELINE_ID);
            std::env::remove_var(ENV_RUNTIME);
        }
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        let checkpoints = store.list_checkpoints().expect("list checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].timeline_id, timeline_id);
        assert_eq!(checkpoints[0].runtime_name.as_deref(), Some("codex"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_edit_hook_records_checkpoint_on_active_timeline() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-edit-hook");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let (run_id, timeline_id) = create_active_run(&store, "claude");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        unsafe {
            std::env::set_var(ENV_RUN_ID, &run_id);
            std::env::set_var(ENV_TIMELINE_ID, &timeline_id);
            std::env::set_var(ENV_RUNTIME, "claude");
        }

        let exit_code = store
            .handle_claude_pre_tool_use(
                r#"{"tool_name":"Edit","tool_input":{"file_path":"src/main.rs"}}"#,
            )
            .expect("handle hook");

        unsafe {
            std::env::remove_var(ENV_RUN_ID);
            std::env::remove_var(ENV_TIMELINE_ID);
            std::env::remove_var(ENV_RUNTIME);
        }
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        let checkpoints = store.list_checkpoints().expect("list checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].timeline_id, timeline_id);
        assert_eq!(checkpoints[0].runtime_name.as_deref(), Some("claude"));
        assert_eq!(checkpoints[0].reason, "before-edit");
        assert_eq!(checkpoints[0].trigger_tool_type.as_deref(), Some("edit"));
        assert_eq!(
            checkpoints[0].trigger_command.as_deref(),
            Some("src/main.rs")
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_bash_hook_records_before_shell_checkpoint_on_active_timeline() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-bash-hook");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let (run_id, timeline_id) = create_active_run(&store, "claude");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        unsafe {
            std::env::set_var(ENV_RUN_ID, &run_id);
            std::env::set_var(ENV_TIMELINE_ID, &timeline_id);
            std::env::set_var(ENV_RUNTIME, "claude");
        }

        let exit_code = store
            .handle_claude_pre_tool_use(
                r#"{"tool_name":"Bash","tool_input":{"command":"rm /tmp/test.txt"}}"#,
            )
            .expect("handle hook");

        unsafe {
            std::env::remove_var(ENV_RUN_ID);
            std::env::remove_var(ENV_TIMELINE_ID);
            std::env::remove_var(ENV_RUNTIME);
        }
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        let checkpoints = store.list_checkpoints().expect("list checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].timeline_id, timeline_id);
        assert_eq!(checkpoints[0].runtime_name.as_deref(), Some("claude"));
        assert_eq!(checkpoints[0].reason, "before-shell");
        assert_eq!(checkpoints[0].trigger_tool_type.as_deref(), Some("bash"));
        assert_eq!(
            checkpoints[0].trigger_command.as_deref(),
            Some("rm /tmp/test.txt")
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_multiedit_hook_records_checkpoint_when_rule_matches() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-multiedit-hook");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let (run_id, timeline_id) = create_active_run(&store, "claude");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        unsafe {
            std::env::set_var(ENV_RUN_ID, &run_id);
            std::env::set_var(ENV_TIMELINE_ID, &timeline_id);
            std::env::set_var(ENV_RUNTIME, "claude");
        }

        let exit_code = store
            .handle_claude_pre_tool_use(
                r#"{"tool_name":"MultiEdit","tool_input":{"file_path":"src/main.rs","edits":[{"old_string":"a","new_string":"b"},{"old_string":"c","new_string":"d"}]}}"#,
            )
            .expect("handle hook");

        unsafe {
            std::env::remove_var(ENV_RUN_ID);
            std::env::remove_var(ENV_TIMELINE_ID);
            std::env::remove_var(ENV_RUNTIME);
        }
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        let checkpoints = store.list_checkpoints().expect("list checkpoints");
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].timeline_id, timeline_id);
        assert_eq!(checkpoints[0].reason, "before-multiedit");
        assert_eq!(
            checkpoints[0].trigger_tool_type.as_deref(),
            Some("multiedit")
        );
        assert_eq!(
            checkpoints[0].trigger_command.as_deref(),
            Some("src/main.rs (2 edits)")
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_bash_restart_hook_skips_checkpointing() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-bash-restart");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let (run_id, timeline_id) = create_active_run(&store, "claude");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        unsafe {
            std::env::set_var(ENV_RUN_ID, &run_id);
            std::env::set_var(ENV_TIMELINE_ID, &timeline_id);
            std::env::set_var(ENV_RUNTIME, "claude");
        }

        let exit_code = store
            .handle_claude_pre_tool_use(r#"{"tool_name":"Bash","tool_input":{"restart":true}}"#)
            .expect("handle hook");

        unsafe {
            std::env::remove_var(ENV_RUN_ID);
            std::env::remove_var(ENV_TIMELINE_ID);
            std::env::remove_var(ENV_RUNTIME);
        }
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        assert!(
            store
                .list_checkpoints()
                .expect("list checkpoints")
                .is_empty()
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_hook_skips_non_matching_tools() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-hook-skip");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let (run_id, timeline_id) = create_active_run(&store, "claude");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        unsafe {
            std::env::set_var(ENV_RUN_ID, &run_id);
            std::env::set_var(ENV_TIMELINE_ID, &timeline_id);
            std::env::set_var(ENV_RUNTIME, "claude");
        }

        let exit_code = store
            .handle_claude_pre_tool_use(
                r#"{"tool_name":"Read","tool_input":{"file_path":"src/main.rs"}}"#,
            )
            .expect("handle hook");

        unsafe {
            std::env::remove_var(ENV_RUN_ID);
            std::env::remove_var(ENV_TIMELINE_ID);
            std::env::remove_var(ENV_RUNTIME);
        }
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        assert!(
            store
                .list_checkpoints()
                .expect("list checkpoints")
                .is_empty()
        );

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_run_persists_session_metadata_and_injects_session_id() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-run-session");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let args_path = repo_root.join("claude-run-args.txt");
        let command = vec![create_fake_agent_script(&repo_root, "claude", &args_path)];

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        let result = store.run_agent(command).expect("run agent");
        std::env::set_current_dir(previous).expect("restore cwd");

        let metadata = store
            .read_runtime_metadata(&result.run_id)
            .expect("read runtime metadata")
            .expect("runtime metadata");
        let session_id = metadata
            .claude_session_id
            .clone()
            .expect("claude session id");
        assert!(super::DaedalusStore::is_valid_claude_session_id(
            &session_id
        ));

        let args = fs::read_to_string(&args_path).expect("read agent args");
        assert!(args.contains("--settings"));
        assert!(args.contains("--session-id"));
        assert!(args.contains(&session_id));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_checkpoint_inherits_session_metadata() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-checkpoint-session");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let command = vec![repo_root.join("claude").display().to_string()];
        let (run_id, timeline_id) =
            create_active_run_with_command(&store, command, Resumability::Full);
        let session_id = "11111111-1111-4111-8111-111111111111".to_string();
        store
            .write_runtime_metadata(
                &run_id,
                &RuntimeMetadataRecord {
                    runtime_name: "claude".to_string(),
                    claude_session_id: Some(session_id.clone()),
                },
            )
            .expect("write runtime metadata");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        let checkpoint = store
            .create_checkpoint_internal(
                &timeline_id,
                &run_id,
                None,
                "before-edit".to_string(),
                super::CheckpointTriggerMetadata {
                    tool_type: Some("edit".to_string()),
                    command: Some("src/main.rs".to_string()),
                    runtime_name: Some("claude".to_string()),
                },
            )
            .expect("create checkpoint");
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(
            checkpoint.claude_session_id.as_deref(),
            Some(session_id.as_str())
        );
        assert_eq!(checkpoint.resumability, Resumability::Full);

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn claude_checkpoint_is_partial_without_session_metadata() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-checkpoint-partial");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let command = vec![repo_root.join("claude").display().to_string()];
        let (run_id, timeline_id) =
            create_active_run_with_command(&store, command, Resumability::Partial);

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        let checkpoint = store
            .create_checkpoint_internal(
                &timeline_id,
                &run_id,
                None,
                "before-edit".to_string(),
                super::CheckpointTriggerMetadata {
                    tool_type: Some("edit".to_string()),
                    command: Some("src/main.rs".to_string()),
                    runtime_name: Some("claude".to_string()),
                },
            )
            .expect("create checkpoint");
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(checkpoint.resumability, Resumability::Partial);
        assert!(checkpoint.claude_session_id.is_none());

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn resume_uses_claude_resume_path_instead_of_fresh_run() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("claude-resume");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let args_path = repo_root.join("claude-resume-args.txt");
        let command = vec![
            create_fake_agent_script(&repo_root, "claude", &args_path),
            "--print".to_string(),
            "fresh prompt".to_string(),
        ];
        let (run_id, timeline_id) =
            create_active_run_with_command(&store, command, Resumability::Full);
        let session_id = "11111111-1111-4111-8111-111111111111".to_string();
        store
            .write_runtime_metadata(
                &run_id,
                &RuntimeMetadataRecord {
                    runtime_name: "claude".to_string(),
                    claude_session_id: Some(session_id.clone()),
                },
            )
            .expect("write runtime metadata");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        let checkpoint = store
            .create_checkpoint_internal(
                &timeline_id,
                &run_id,
                None,
                "before-edit".to_string(),
                super::CheckpointTriggerMetadata {
                    tool_type: Some("edit".to_string()),
                    command: Some("src/main.rs".to_string()),
                    runtime_name: Some("claude".to_string()),
                },
            )
            .expect("create checkpoint");
        fs::remove_file(&args_path).ok();
        let exit_code = store.resume(&checkpoint.id).expect("resume checkpoint");
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        let args = fs::read_to_string(&args_path).expect("read agent args");
        assert!(args.contains("--resume"));
        assert!(args.contains(&session_id));
        assert!(!args.contains("fresh prompt"));
        assert!(!args.contains("--print"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn non_claude_resume_keeps_existing_behavior() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("codex-resume");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        let args_path = repo_root.join("codex-resume-args.txt");
        let command = vec![
            create_fake_agent_script(&repo_root, "codex", &args_path),
            "--resume".to_string(),
            "keep-me".to_string(),
        ];
        let (run_id, timeline_id) =
            create_active_run_with_command(&store, command, Resumability::Full);

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        let checkpoint = store
            .create_checkpoint_internal(
                &timeline_id,
                &run_id,
                None,
                "before-shell".to_string(),
                super::CheckpointTriggerMetadata {
                    tool_type: Some("bash".to_string()),
                    command: Some("rm README.md".to_string()),
                    runtime_name: Some("codex".to_string()),
                },
            )
            .expect("create checkpoint");
        fs::remove_file(&args_path).ok();
        let exit_code = store.resume(&checkpoint.id).expect("resume checkpoint");
        std::env::set_current_dir(previous).expect("restore cwd");

        assert_eq!(exit_code, 0);
        let args = fs::read_to_string(&args_path).expect("read agent args");
        assert!(args.contains("--resume"));
        assert!(args.contains("keep-me"));

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn fork_clones_checkpoint_into_new_timeline() {
        let _guard = test_lock().lock().expect("lock tests");
        let repo_root = create_temp_repo("fork");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        fs::write(repo_root.join("README.md"), "hello").expect("seed file");

        let previous = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(&repo_root).expect("cd temp repo");
        store
            .run_shell_command(vec!["rm".to_string(), "README.md".to_string()])
            .expect("run shell");
        std::env::set_current_dir(previous).expect("restore cwd");

        let checkpoint_id = store
            .list_checkpoints()
            .expect("checkpoints")
            .last()
            .expect("checkpoint")
            .id
            .clone();

        let (timeline_id, run_id) = store
            .fork(&checkpoint_id, Some("alt".to_string()))
            .expect("fork");
        assert!(!timeline_id.is_empty());
        assert!(!run_id.is_empty());

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    fn create_temp_repo(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ddl-store-test-{name}-{}",
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

    fn create_active_run(store: &DaedalusStore, runtime: &str) -> (String, String) {
        create_active_run_with_command(store, vec![runtime.to_string()], Resumability::Full)
    }

    fn create_active_run_with_command(
        store: &DaedalusStore,
        command: Vec<String>,
        resumability: Resumability,
    ) -> (String, String) {
        let created_at = super::unix_timestamp();
        let run_id = "run_test".to_string();
        let timeline_id = "tl_test".to_string();
        store
            .write_timeline(&TimelineRecord {
                id: timeline_id.clone(),
                name: Some("main".to_string()),
                run_id: run_id.clone(),
                root_checkpoint_id: None,
                source_checkpoint_id: None,
                created_at,
            })
            .expect("write timeline");
        store
            .write_run(&RunRecord {
                id: run_id.clone(),
                timeline_id: timeline_id.clone(),
                command,
                created_at,
                status: RunStatus::Running,
                last_checkpoint_id: None,
                resumability,
            })
            .expect("write run");
        (run_id, timeline_id)
    }

    fn create_fake_agent_script(
        repo_root: &std::path::Path,
        name: &str,
        output: &std::path::Path,
    ) -> String {
        let path = repo_root.join(name);
        let mut file = fs::File::create(&path).expect("create script");
        writeln!(
            file,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            output.display()
        )
        .expect("write script");
        let mut permissions = fs::metadata(&path).expect("stat script").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod script");
        path.display().to_string()
    }
}

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{DdlError, Result};
use crate::ids::next_id;
use crate::model::{
    CheckpointRecord, Resumability, RunRecord, RunStatus, RuntimeFingerprint, TimelineRecord,
};

const STATE_DIR: &str = ".daedalus";
const SNAPSHOT_DIR_NAME: &str = "snapshots";
const CONFIG_VERSION: &str = "1";
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
    pub checkpoint_id: String,
    pub exit_code: i32,
}

impl DaedalusStore {
    pub fn discover() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        Self::discover_from(&cwd)
    }

    pub fn discover_from(cwd: &Path) -> Result<Self> {
        let repo_root = resolve_repo_root(&cwd)?;
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

        let config_path = self.state_dir.join("config");
        fs::write(
            config_path,
            format!("version={CONFIG_VERSION}\nstorage=shadow-git\n"),
        )?;

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
        if command.is_empty() {
            return Err(DdlError::InvalidInput(
                "missing agent command after `ddl run --`".to_string(),
            ));
        }

        let created_at = unix_timestamp();
        let run_id = next_id("run");
        let timeline_id = next_id("tl");

        let checkpoint =
            self.create_checkpoint_internal(&timeline_id, &run_id, None, "run-start".to_string())?;

        let timeline = TimelineRecord {
            id: timeline_id.clone(),
            name: Some("main".to_string()),
            run_id: run_id.clone(),
            root_checkpoint_id: checkpoint.id.clone(),
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
            last_checkpoint_id: checkpoint.id.clone(),
            resumability: Resumability::Full,
        };
        self.write_run(&run)?;

        let status = Command::new(&command[0])
            .args(command.iter().skip(1))
            .current_dir(&self.repo_root)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;

        run.status = if status.success() {
            RunStatus::Succeeded
        } else {
            RunStatus::Failed
        };
        self.write_run(&run)?;

        Ok(RunInvocation {
            timeline_id,
            run_id,
            checkpoint_id: checkpoint.id,
            exit_code: status.code().unwrap_or(1),
        })
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

        let next_checkpoint = self.create_checkpoint_internal(
            &checkpoint.timeline_id,
            &checkpoint.run_id,
            Some(checkpoint.id.clone()),
            "resume-start".to_string(),
        )?;
        run.last_checkpoint_id = next_checkpoint.id;
        run.status = RunStatus::Running;
        self.write_run(&run)?;

        let status = Command::new(&run.command[0])
            .args(run.command.iter().skip(1))
            .current_dir(&self.repo_root)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;

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
            root_checkpoint_id: checkpoint.id.clone(),
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
            last_checkpoint_id: checkpoint.id,
            resumability: Resumability::Full,
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
            items.push(CheckpointRecord::read(&path)?);
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

    pub fn read_checkpoint(&self, checkpoint_id: &str) -> Result<CheckpointRecord> {
        let path = self.checkpoints_dir.join(format!("{checkpoint_id}.meta"));
        if !path.exists() {
            return Err(DdlError::NotFound {
                kind: "checkpoint",
                id: checkpoint_id.to_string(),
            });
        }
        CheckpointRecord::read(&path)
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

    fn create_checkpoint_internal(
        &self,
        timeline_id: &str,
        run_id: &str,
        parent_checkpoint_id: Option<String>,
        reason: String,
    ) -> Result<CheckpointRecord> {
        let checkpoint_id = next_id("cp");
        let snapshot_rel_path = format!("{SNAPSHOT_DIR_NAME}/{checkpoint_id}");
        let snapshot_path = self.snapshot_path(&snapshot_rel_path);
        if snapshot_path.exists() {
            fs::remove_dir_all(&snapshot_path)?;
        }
        fs::create_dir_all(&snapshot_path)?;
        copy_workspace_to_snapshot(&self.repo_root, &snapshot_path)?;

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
            resumability: Resumability::Full,
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
            resumability: Resumability::Full,
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
    use std::path::PathBuf;
    use std::process::Command;

    use super::DaedalusStore;

    #[test]
    fn init_creates_state_directories() {
        let repo_root = create_temp_repo("init");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");

        assert!(repo_root.join(".daedalus").exists());
        assert!(repo_root.join(".daedalus/shadow/.git").exists());
        assert!(repo_root.join(".daedalus/checkpoints").exists());

        fs::remove_dir_all(repo_root).expect("cleanup temp repo");
    }

    #[test]
    fn fork_clones_checkpoint_into_new_timeline() {
        let repo_root = create_temp_repo("fork");

        let store = DaedalusStore::discover_from(&repo_root).expect("discover store");
        store.init().expect("initialize store");
        fs::write(repo_root.join("README.md"), "hello").expect("seed file");
        let run = store
            .run_agent(vec!["/usr/bin/true".to_string()])
            .expect("run command");

        let (timeline_id, run_id) = store
            .fork(&run.checkpoint_id, Some("alt".to_string()))
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
}

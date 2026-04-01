use std::fmt::{Display, Formatter};
use std::path::Path;

use crate::error::{DdlError, Result};
use crate::kv::{optional_value, read_pairs, repeated_values, required_value, write_pairs};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunStatus {
    Ready,
    Running,
    Succeeded,
    Failed,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(DdlError::InvalidState(format!(
                "unknown run status `{value}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resumability {
    Full,
    Partial,
    Unavailable,
}

impl Resumability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "full" => Ok(Self::Full),
            "partial" => Ok(Self::Partial),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(DdlError::InvalidState(format!(
                "unknown resumability `{value}`"
            ))),
        }
    }
}

impl Display for Resumability {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckpointKind {
    ProtectedAction,
    SessionHead,
}

impl CheckpointKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProtectedAction => "protected_action",
            Self::SessionHead => "session_head",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "protected_action" => Ok(Self::ProtectedAction),
            "session_head" => Ok(Self::SessionHead),
            _ => Err(DdlError::InvalidState(format!(
                "unknown checkpoint kind `{value}`"
            ))),
        }
    }
}

impl Display for CheckpointKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeFingerprint {
    pub cwd: String,
    pub repo_root: String,
    pub git_head: String,
    pub git_branch: String,
    pub git_dirty: bool,
    pub git_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimelineRecord {
    pub id: String,
    pub run_id: String,
    pub created_at: u64,
}

impl TimelineRecord {
    pub fn read(path: &Path) -> Result<Self> {
        let map = read_pairs(path)?;
        Ok(Self {
            id: required_value(&map, "id")?,
            run_id: required_value(&map, "run_id")?,
            created_at: required_value(&map, "created_at")?.parse()?,
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let pairs = vec![
            ("id", self.id.clone()),
            ("run_id", self.run_id.clone()),
            ("created_at", self.created_at.to_string()),
        ];
        write_pairs(path, &pairs)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRecord {
    pub id: String,
    pub timeline_id: String,
    pub command: Vec<String>,
    pub created_at: u64,
    pub status: RunStatus,
    pub last_checkpoint_id: Option<String>,
    pub head_checkpoint_id: Option<String>,
    pub rewind_source_checkpoint_id: Option<String>,
    pub resumability: Resumability,
}

impl RunRecord {
    pub fn read(path: &Path) -> Result<Self> {
        let map = read_pairs(path)?;
        Ok(Self {
            id: required_value(&map, "id")?,
            timeline_id: required_value(&map, "timeline_id")?,
            command: repeated_values(&map, "arg"),
            created_at: required_value(&map, "created_at")?.parse()?,
            status: RunStatus::parse(&required_value(&map, "status")?)?,
            last_checkpoint_id: optional_value(&map, "last_checkpoint_id"),
            head_checkpoint_id: optional_value(&map, "head_checkpoint_id"),
            rewind_source_checkpoint_id: optional_value(&map, "rewind_source_checkpoint_id"),
            resumability: Resumability::parse(&required_value(&map, "resumability")?)?,
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut pairs = vec![
            ("id", self.id.clone()),
            ("timeline_id", self.timeline_id.clone()),
            ("created_at", self.created_at.to_string()),
            ("status", self.status.as_str().to_string()),
            ("resumability", self.resumability.as_str().to_string()),
        ];
        if let Some(last_checkpoint_id) = &self.last_checkpoint_id {
            pairs.push(("last_checkpoint_id", last_checkpoint_id.clone()));
        }
        if let Some(head_checkpoint_id) = &self.head_checkpoint_id {
            pairs.push(("head_checkpoint_id", head_checkpoint_id.clone()));
        }
        if let Some(rewind_source_checkpoint_id) = &self.rewind_source_checkpoint_id {
            pairs.push((
                "rewind_source_checkpoint_id",
                rewind_source_checkpoint_id.clone(),
            ));
        }
        for arg in &self.command {
            pairs.push(("arg", arg.clone()));
        }
        write_pairs(path, &pairs)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeMetadataRecord {
    pub runtime_name: String,
    pub claude_session_id: Option<String>,
}

impl RuntimeMetadataRecord {
    pub fn read(path: &Path) -> Result<Self> {
        let map = read_pairs(path)?;
        Ok(Self {
            runtime_name: required_value(&map, "runtime_name")?,
            claude_session_id: optional_value(&map, "claude_session_id"),
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut pairs = vec![("runtime_name", self.runtime_name.clone())];
        if let Some(claude_session_id) = &self.claude_session_id {
            pairs.push(("claude_session_id", claude_session_id.clone()));
        }
        write_pairs(path, &pairs)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateMetadataRecord {
    pub repo_root: String,
}

impl StateMetadataRecord {
    pub fn read(path: &Path) -> Result<Self> {
        let map = read_pairs(path)?;
        Ok(Self {
            repo_root: required_value(&map, "repo_root")?,
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let pairs = vec![("repo_root", self.repo_root.clone())];
        write_pairs(path, &pairs)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointRecord {
    pub id: String,
    pub timeline_id: String,
    pub run_id: String,
    pub kind: CheckpointKind,
    pub parent_checkpoint_id: Option<String>,
    pub reason: String,
    pub snapshot_rel_path: String,
    pub shadow_commit: String,
    pub created_at: u64,
    pub resumability: Resumability,
    pub trigger_tool_type: Option<String>,
    pub trigger_command: Option<String>,
    pub runtime_name: Option<String>,
    pub claude_session_id: Option<String>,
    pub claude_rewind_rel_path: Option<String>,
    pub fingerprint: RuntimeFingerprint,
}

impl CheckpointRecord {
    pub fn read(path: &Path) -> Result<Self> {
        let map = read_pairs(path)?;
        let reason = required_value(&map, "reason")?;
        let kind = match optional_value(&map, "kind") {
            Some(value) => CheckpointKind::parse(&value)?,
            None if reason == "session-head" => CheckpointKind::SessionHead,
            None => CheckpointKind::ProtectedAction,
        };
        Ok(Self {
            id: required_value(&map, "id")?,
            timeline_id: required_value(&map, "timeline_id")?,
            run_id: required_value(&map, "run_id")?,
            kind,
            parent_checkpoint_id: optional_value(&map, "parent_checkpoint_id"),
            reason,
            snapshot_rel_path: required_value(&map, "snapshot_rel_path")?,
            shadow_commit: required_value(&map, "shadow_commit")?,
            created_at: required_value(&map, "created_at")?.parse()?,
            resumability: Resumability::parse(&required_value(&map, "resumability")?)?,
            trigger_tool_type: optional_value(&map, "trigger_tool_type"),
            trigger_command: optional_value(&map, "trigger_command"),
            runtime_name: optional_value(&map, "runtime_name"),
            claude_session_id: optional_value(&map, "claude_session_id"),
            claude_rewind_rel_path: optional_value(&map, "claude_rewind_rel_path"),
            fingerprint: RuntimeFingerprint {
                cwd: required_value(&map, "cwd")?,
                repo_root: required_value(&map, "repo_root")?,
                git_head: required_value(&map, "git_head")?,
                git_branch: required_value(&map, "git_branch")?,
                git_dirty: required_value(&map, "git_dirty")? == "true",
                git_version: required_value(&map, "git_version")?,
            },
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let mut pairs = vec![
            ("id", self.id.clone()),
            ("timeline_id", self.timeline_id.clone()),
            ("run_id", self.run_id.clone()),
            ("kind", self.kind.as_str().to_string()),
            ("reason", self.reason.clone()),
            ("snapshot_rel_path", self.snapshot_rel_path.clone()),
            ("shadow_commit", self.shadow_commit.clone()),
            ("created_at", self.created_at.to_string()),
            ("resumability", self.resumability.as_str().to_string()),
            ("cwd", self.fingerprint.cwd.clone()),
            ("repo_root", self.fingerprint.repo_root.clone()),
            ("git_head", self.fingerprint.git_head.clone()),
            ("git_branch", self.fingerprint.git_branch.clone()),
            ("git_dirty", self.fingerprint.git_dirty.to_string()),
            ("git_version", self.fingerprint.git_version.clone()),
        ];
        if let Some(parent) = &self.parent_checkpoint_id {
            pairs.push(("parent_checkpoint_id", parent.clone()));
        }
        if let Some(trigger_tool_type) = &self.trigger_tool_type {
            pairs.push(("trigger_tool_type", trigger_tool_type.clone()));
        }
        if let Some(trigger_command) = &self.trigger_command {
            pairs.push(("trigger_command", trigger_command.clone()));
        }
        if let Some(runtime_name) = &self.runtime_name {
            pairs.push(("runtime_name", runtime_name.clone()));
        }
        if let Some(claude_session_id) = &self.claude_session_id {
            pairs.push(("claude_session_id", claude_session_id.clone()));
        }
        if let Some(claude_rewind_rel_path) = &self.claude_rewind_rel_path {
            pairs.push(("claude_rewind_rel_path", claude_rewind_rel_path.clone()));
        }
        write_pairs(path, &pairs)
    }
}

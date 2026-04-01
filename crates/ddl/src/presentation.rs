use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::ToolKind;
use crate::model::{CheckpointKind, CheckpointRecord, RunRecord, RunStatus, TimelineRecord};
use crate::runtime::SupportedRuntime;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum RecoveryCapability {
    Rewindable,
    RestoreOnly,
    Unavailable,
}

impl RecoveryCapability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rewindable => "Rewindable",
            Self::RestoreOnly => "Restore only",
            Self::Unavailable => "Unavailable",
        }
    }
}

pub fn recovery_capability(checkpoint: &CheckpointRecord) -> RecoveryCapability {
    if checkpoint.kind == CheckpointKind::SessionHead {
        return match (
            checkpoint.runtime_name.as_deref(),
            checkpoint.resumability.as_str(),
        ) {
            (_, "unavailable") => RecoveryCapability::Unavailable,
            (Some("claude"), "partial") => RecoveryCapability::RestoreOnly,
            (Some("claude"), "full") => RecoveryCapability::Rewindable,
            _ => RecoveryCapability::RestoreOnly,
        };
    }

    match (
        checkpoint.runtime_name.as_deref(),
        checkpoint.resumability.as_str(),
    ) {
        (_, "unavailable") => RecoveryCapability::Unavailable,
        (Some("claude"), "partial") => RecoveryCapability::RestoreOnly,
        (_, "full") | (_, "partial") => RecoveryCapability::Rewindable,
        _ => RecoveryCapability::Unavailable,
    }
}

pub fn session_title(timeline: &TimelineRecord, run: &RunRecord) -> String {
    format!(
        "{} session · {}",
        runtime_display_name(run),
        format_relative_time(timeline.created_at)
    )
}

pub fn runtime_display_name(run: &RunRecord) -> String {
    match SupportedRuntime::detect(&run.command) {
        Ok(SupportedRuntime::Claude) => "Claude".to_string(),
        Err(_) => run
            .command
            .first()
            .map(|value| humanize_token(value))
            .unwrap_or_else(|| "Unknown".to_string()),
    }
}

pub fn session_status_label(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "Active",
        RunStatus::Succeeded => "Finished",
        RunStatus::Failed => "Failed",
        RunStatus::Ready => "Ready",
    }
}

pub fn tool_event_label(checkpoint: &CheckpointRecord) -> String {
    if checkpoint.kind == CheckpointKind::SessionHead {
        return "Session Head".to_string();
    }

    match (
        checkpoint.trigger_tool_type.as_deref(),
        checkpoint.trigger_command.as_deref(),
    ) {
        (Some(tool_type), Some(command)) if !command.trim().is_empty() => {
            format!("{} {}", humanize_tool_type(tool_type), command.trim())
        }
        (Some(tool_type), _) => humanize_tool_type(tool_type),
        _ => reason_fallback_label(&checkpoint.reason),
    }
}

pub fn tool_event_preview(checkpoint: &CheckpointRecord) -> Option<String> {
    if checkpoint.kind == CheckpointKind::SessionHead {
        return Some("Latest workspace state when the session ended.".to_string());
    }

    let command = checkpoint.trigger_command.as_deref()?.trim();
    if command.is_empty() {
        return None;
    }

    match checkpoint.trigger_tool_type.as_deref() {
        Some("bash") => Some(format!("Command: {command}")),
        Some("edit") | Some("write") | Some("multiedit") => Some(format!("Target: {command}")),
        Some(tool_type) => Some(format!("{}: {command}", humanize_tool_type(tool_type))),
        None => Some(command.to_string()),
    }
}

pub fn latest_action_label(checkpoint: Option<&CheckpointRecord>) -> String {
    checkpoint
        .map(tool_event_label)
        .unwrap_or_else(|| "No protected actions yet".to_string())
}

pub fn continuation_label(checkpoint: Option<&CheckpointRecord>) -> Option<String> {
    checkpoint.map(|checkpoint| format!("Continued from {}", tool_event_label(checkpoint)))
}

pub fn format_relative_time(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let delta = now.saturating_sub(timestamp);

    if delta < 60 {
        format!("{delta}s ago")
    } else if delta < 3600 {
        format!("{}m ago", delta / 60)
    } else if delta < 86_400 {
        format!("{}h ago", delta / 3600)
    } else {
        format!("{}d ago", delta / 86_400)
    }
}

pub fn format_absolute_time(timestamp: u64) -> String {
    let seconds = timestamp.to_string();
    let output = Command::new("date")
        .arg("-r")
        .arg(&seconds)
        .arg("+%Y-%m-%d %H:%M:%S %Z")
        .output();

    match output {
        Ok(result) if result.status.success() => {
            let value = String::from_utf8_lossy(&result.stdout).trim().to_string();
            if value.is_empty() { seconds } else { value }
        }
        _ => seconds,
    }
}

pub fn format_runtime(started_at: u64, ended_at: Option<u64>) -> String {
    let end = ended_at.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    });
    format_duration(end.saturating_sub(started_at))
}

pub fn humanize_tool_type(value: &str) -> String {
    match value {
        "bash" => "Bash".to_string(),
        "edit" => "Edit".to_string(),
        "multiedit" => "MultiEdit".to_string(),
        "write" => "Write".to_string(),
        other => humanize_token(other),
    }
}

fn reason_fallback_label(reason: &str) -> String {
    if let Some(tool_name) = reason.strip_prefix("before-") {
        return format!("Before {}", humanize_tool_type(tool_name));
    }
    humanize_token(reason)
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else {
        format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3600)
    }
}

fn humanize_token(value: &str) -> String {
    let normalized = value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .trim()
        .replace(['_', '-'], " ");
    let mut output = String::new();
    for (index, word) in normalized.split_whitespace().enumerate() {
        if index > 0 {
            output.push(' ');
        }
        if let Some(tool) = parse_tool_kind(word) {
            output.push_str(&humanize_tool_type(&tool.to_string()));
            continue;
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            output.extend(first.to_uppercase());
            output.push_str(chars.as_str());
        }
    }
    if output.is_empty() {
        "Unknown".to_string()
    } else {
        output
    }
}

fn parse_tool_kind(value: &str) -> Option<ToolKind> {
    match value.to_ascii_lowercase().as_str() {
        "bash" => Some(ToolKind::Bash),
        "edit" => Some(ToolKind::Edit),
        "multiedit" => Some(ToolKind::MultiEdit),
        "write" => Some(ToolKind::Write),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::model::{CheckpointKind, CheckpointRecord, Resumability, RuntimeFingerprint};

    use super::{
        RecoveryCapability, recovery_capability, session_status_label, tool_event_label,
        tool_event_preview,
    };

    fn checkpoint(tool: Option<&str>, command: Option<&str>) -> CheckpointRecord {
        CheckpointRecord {
            id: "cp_test".to_string(),
            timeline_id: "tl_test".to_string(),
            run_id: "run_test".to_string(),
            kind: CheckpointKind::ProtectedAction,
            parent_checkpoint_id: None,
            reason: "before-shell".to_string(),
            snapshot_rel_path: "snapshots/cp_test".to_string(),
            shadow_commit: "deadbeef".to_string(),
            created_at: 1,
            resumability: Resumability::Full,
            trigger_tool_type: tool.map(ToOwned::to_owned),
            trigger_command: command.map(ToOwned::to_owned),
            runtime_name: Some("claude".to_string()),
            claude_session_id: None,
            claude_rewind_rel_path: None,
            fingerprint: RuntimeFingerprint {
                cwd: ".".to_string(),
                repo_root: ".".to_string(),
                git_head: "deadbeef".to_string(),
                git_branch: "main".to_string(),
                git_dirty: false,
                git_version: "git version".to_string(),
            },
        }
    }

    #[test]
    fn tool_event_label_prefers_tool_and_command() {
        assert_eq!(
            tool_event_label(&checkpoint(Some("bash"), Some("rm README.md"))),
            "Bash rm README.md"
        );
        assert_eq!(
            tool_event_label(&checkpoint(Some("write"), Some("src/main.rs"))),
            "Write src/main.rs"
        );
    }

    #[test]
    fn tool_event_preview_uses_human_context() {
        assert_eq!(
            tool_event_preview(&checkpoint(Some("bash"), Some("rm README.md"))),
            Some("Command: rm README.md".to_string())
        );
        assert_eq!(
            tool_event_preview(&checkpoint(Some("edit"), Some("src/main.rs"))),
            Some("Target: src/main.rs".to_string())
        );
    }

    #[test]
    fn recovery_capability_maps_partial_claude_to_restore_only() {
        let mut checkpoint = checkpoint(Some("edit"), Some("src/main.rs"));
        checkpoint.resumability = Resumability::Partial;
        assert_eq!(
            recovery_capability(&checkpoint),
            RecoveryCapability::RestoreOnly
        );
    }

    #[test]
    fn session_status_uses_user_facing_language() {
        assert_eq!(
            session_status_label(&crate::model::RunStatus::Running),
            "Active"
        );
        assert_eq!(
            session_status_label(&crate::model::RunStatus::Succeeded),
            "Finished"
        );
    }
}

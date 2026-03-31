use std::env;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::{DdlError, Result};

pub const ENV_RUN_ID: &str = "DAEDALUS_RUN_ID";
pub const ENV_TIMELINE_ID: &str = "DAEDALUS_TIMELINE_ID";
pub const ENV_RUNTIME: &str = "DAEDALUS_RUNTIME";
pub const ENV_REAL_SHELL: &str = "DAEDALUS_REAL_SHELL";
const CLAUDE_PRE_TOOL_USE_HOOK_NAME: &str = "claude-pre-tool-use";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupportedRuntime {
    Codex,
    Claude,
}

impl SupportedRuntime {
    pub fn detect(command: &[String]) -> Result<Self> {
        let Some(first) = command.first() else {
            return Err(DdlError::InvalidInput(
                "missing agent command after `ddl run --`".to_string(),
            ));
        };

        let name = Path::new(first)
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or(first);

        match name {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            other => Err(DdlError::UnsupportedRuntime(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ShellWrapperContext {
    pub run_id: String,
    pub timeline_id: String,
    pub runtime: SupportedRuntime,
}

pub fn apply_runtime_environment(
    command: &mut std::process::Command,
    repo_root: &Path,
    state_dir: &Path,
    context: &ShellWrapperContext,
) -> Result<()> {
    let shell_dir = state_dir.join("runtime").join(&context.run_id).join("bin");
    fs::create_dir_all(&shell_dir)?;

    let ddl_path = std::env::current_exe()?;
    let original_path = env::var_os("PATH").unwrap_or_default();

    create_shell_shim(&shell_dir, "bash", &ddl_path, &original_path)?;
    create_shell_shim(&shell_dir, "sh", &ddl_path, &original_path)?;
    create_shell_shim(&shell_dir, "zsh", &ddl_path, &original_path)?;

    let shell = preferred_shell_path(&shell_dir);
    let path = join_path_with_prefix(&shell_dir, &original_path);

    command.current_dir(repo_root);
    command.env("PATH", path);
    command.env("SHELL", shell);
    command.env(ENV_RUN_ID, &context.run_id);
    command.env(ENV_TIMELINE_ID, &context.timeline_id);
    command.env(ENV_RUNTIME, context.runtime.as_str());
    Ok(())
}

pub fn prepare_runtime_command(
    command: &[String],
    state_dir: &Path,
    context: &ShellWrapperContext,
) -> Result<Vec<String>> {
    match context.runtime {
        SupportedRuntime::Codex => Ok(command.to_vec()),
        SupportedRuntime::Claude => prepare_claude_command(command, state_dir, context),
    }
}

pub fn current_shell_context() -> Option<ShellWrapperContext> {
    let run_id = env::var(ENV_RUN_ID).ok()?;
    let timeline_id = env::var(ENV_TIMELINE_ID).ok()?;
    let runtime = match env::var(ENV_RUNTIME).ok()?.as_str() {
        "codex" => SupportedRuntime::Codex,
        "claude" => SupportedRuntime::Claude,
        _ => return None,
    };

    Some(ShellWrapperContext {
        run_id,
        timeline_id,
        runtime,
    })
}

fn preferred_shell_path(shell_dir: &Path) -> PathBuf {
    let original_shell = env::var_os("SHELL")
        .and_then(|value| {
            PathBuf::from(value)
                .file_name()
                .map(|name| name.to_os_string())
        })
        .unwrap_or_else(|| OsStr::new("bash").to_os_string());
    let candidate = shell_dir.join(original_shell);
    if candidate.exists() {
        candidate
    } else {
        shell_dir.join("bash")
    }
}

fn prepare_claude_command(
    command: &[String],
    state_dir: &Path,
    context: &ShellWrapperContext,
) -> Result<Vec<String>> {
    if command.iter().any(|item| item == "--bare") {
        return Err(DdlError::InvalidInput(
            "claude `--bare` disables hooks; remove `--bare` to use daedalus protection"
                .to_string(),
        ));
    }

    let hook_path = create_claude_pre_tool_use_hook(state_dir, &context.run_id)?;
    let settings = format!(
        "{{\"hooks\":{{\"PreToolUse\":[{{\"matcher\":\"Edit|MultiEdit|Write|Bash\",\"hooks\":[{{\"type\":\"command\",\"command\":{}}}]}}]}}}}",
        json_quote(&shell_quote(hook_path.as_os_str()))
    );

    let mut prepared = vec![command[0].clone(), "--settings".to_string(), settings];
    prepared.extend(command.iter().skip(1).cloned());
    Ok(prepared)
}

fn create_shell_shim(
    shell_dir: &Path,
    shell_name: &str,
    ddl_path: &Path,
    original_path: &OsStr,
) -> Result<()> {
    let real_shell = resolve_program(shell_name, original_path)
        .or_else(|| {
            let fallback = PathBuf::from(format!("/bin/{shell_name}"));
            fallback.exists().then_some(fallback)
        })
        .ok_or_else(|| {
            DdlError::InvalidState(format!(
                "unable to resolve real shell binary for `{shell_name}`"
            ))
        })?;

    let script = format!(
        "#!/bin/sh\nexport {}={}\nexec {} shell -- \"$@\"\n",
        ENV_REAL_SHELL,
        shell_quote(real_shell.as_os_str()),
        shell_quote(ddl_path.as_os_str()),
    );

    let path = shell_dir.join(shell_name);
    fs::write(&path, script)?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn create_claude_pre_tool_use_hook(state_dir: &Path, run_id: &str) -> Result<PathBuf> {
    let runtime_dir = state_dir.join("runtime").join(run_id).join("bin");
    fs::create_dir_all(&runtime_dir)?;

    let ddl_path = std::env::current_exe()?;
    let path = runtime_dir.join(CLAUDE_PRE_TOOL_USE_HOOK_NAME);
    let script = format!(
        "#!/bin/sh\nexec {} internal claude-pre-tool-use\n",
        shell_quote(ddl_path.as_os_str()),
    );

    fs::write(&path, script)?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}

fn resolve_program(program: &str, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.exists())
}

fn join_path_with_prefix(prefix: &Path, existing: &OsStr) -> std::ffi::OsString {
    let mut paths = vec![prefix.to_path_buf()];
    paths.extend(env::split_paths(existing));
    env::join_paths(paths).unwrap_or_else(|_| prefix.as_os_str().to_os_string())
}

fn shell_quote(value: &OsStr) -> String {
    let raw = value.to_string_lossy();
    let escaped = raw.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

fn json_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::{ShellWrapperContext, SupportedRuntime, prepare_runtime_command};

    #[test]
    fn claude_command_injects_pre_tool_use_settings() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ddl-runtime-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let command = vec![
            "claude".to_string(),
            "--print".to_string(),
            "hello".to_string(),
        ];
        let prepared = prepare_runtime_command(
            &command,
            &temp_dir,
            &ShellWrapperContext {
                run_id: "run_test".to_string(),
                timeline_id: "tl_test".to_string(),
                runtime: SupportedRuntime::Claude,
            },
        )
        .expect("prepare command");

        assert_eq!(prepared[0], "claude");
        assert_eq!(prepared[1], "--settings");
        assert!(prepared[2].contains("Edit|MultiEdit|Write|Bash"));
        assert!(prepared[2].contains("claude-pre-tool-use"));
        assert_eq!(prepared[3], "--print");
        assert_eq!(prepared[4], "hello");

        std::fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
    }

    #[test]
    fn codex_command_is_unchanged() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ddl-runtime-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let command = vec!["codex".to_string(), "--resume".to_string()];
        let prepared = prepare_runtime_command(
            &command,
            &temp_dir,
            &ShellWrapperContext {
                run_id: "run_test".to_string(),
                timeline_id: "tl_test".to_string(),
                runtime: SupportedRuntime::Codex,
            },
        )
        .expect("prepare command");

        assert_eq!(prepared, command);

        std::fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
    }

    #[test]
    fn claude_bare_is_rejected() {
        let temp_dir = std::env::temp_dir().join(format!(
            "ddl-runtime-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let error = prepare_runtime_command(
            &["claude".to_string(), "--bare".to_string()],
            &temp_dir,
            &ShellWrapperContext {
                run_id: "run_test".to_string(),
                timeline_id: "tl_test".to_string(),
                runtime: SupportedRuntime::Claude,
            },
        )
        .expect_err("reject bare");

        assert!(error.to_string().contains("claude `--bare` disables hooks"));

        std::fs::remove_dir_all(temp_dir).expect("cleanup temp dir");
    }
}

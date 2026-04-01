use std::fmt::{Display, Formatter};
use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, DdlError>;

#[derive(Debug)]
pub enum DdlError {
    Io(std::io::Error),
    ParseInt(std::num::ParseIntError),
    InvalidInput(String),
    InvalidConfig(String),
    InvalidState(String),
    NotInitialized(PathBuf),
    UnsupportedRuntime(String),
    NotFound {
        kind: &'static str,
        id: String,
    },
    CommandFailed {
        program: String,
        status: Option<i32>,
        stderr: String,
    },
}

impl Display for DdlError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::ParseInt(error) => write!(f, "{error}"),
            Self::InvalidInput(message) => write!(f, "{message}"),
            Self::InvalidConfig(message) => write!(f, "{message}"),
            Self::InvalidState(message) => write!(f, "{message}"),
            Self::NotInitialized(path) => write!(
                f,
                "repository is not initialized for daedalus: run `ddl init` in {}",
                path.display()
            ),
            Self::UnsupportedRuntime(runtime) => write!(
                f,
                "runtime `{runtime}` is not supported; supported runtime: claude"
            ),
            Self::NotFound { kind, id } => write!(f, "{kind} `{id}` was not found"),
            Self::CommandFailed {
                program,
                status,
                stderr,
            } => {
                if stderr.trim().is_empty() {
                    write!(f, "{program} failed with status {:?}", status)
                } else {
                    write!(
                        f,
                        "{program} failed with status {:?}: {}",
                        status,
                        stderr.trim()
                    )
                }
            }
        }
    }
}

impl std::error::Error for DdlError {}

impl From<std::io::Error> for DdlError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<std::num::ParseIntError> for DdlError {
    fn from(value: std::num::ParseIntError) -> Self {
        Self::ParseInt(value)
    }
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("ERR_LAUNCH_BAD_PARAMS: {0}")]
    BadParams(String),
    #[error("ERR_LAUNCH_NOT_FOUND: {0}")]
    NotFound(String),
    #[error("ERR_LAUNCH_NOT_SUPPORTED: {0}")]
    NotSupported(String),
    #[error("ERR_LAUNCH_PROVIDER_MISSING: {0}")]
    ProviderMissing(String),
    #[error("ERR_LAUNCH_BLOCKED: {0}")]
    Blocked(String),
    #[error("ERR_LAUNCH_SPAWN_FAILED: {0}")]
    SpawnFailed(String),
    #[error("ERR_LAUNCH_TIMEOUT: {0}")]
    Timeout(String),
    #[error("ERR_LAUNCH_BACKEND: {0}")]
    Backend(String),
}

impl LauncherError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadParams(_) => "ERR_LAUNCH_BAD_PARAMS",
            Self::NotFound(_) => "ERR_LAUNCH_NOT_FOUND",
            Self::NotSupported(_) => "ERR_LAUNCH_NOT_SUPPORTED",
            Self::ProviderMissing(_) => "ERR_LAUNCH_PROVIDER_MISSING",
            Self::Blocked(_) => "ERR_LAUNCH_BLOCKED",
            Self::SpawnFailed(_) => "ERR_LAUNCH_SPAWN_FAILED",
            Self::Timeout(_) => "ERR_LAUNCH_TIMEOUT",
            Self::Backend(_) => "ERR_LAUNCH_BACKEND",
        }
    }
}

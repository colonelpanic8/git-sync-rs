use thiserror::Error;

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("Not a git repository: {path}")]
    NotARepository { path: String },

    #[error("Repository in unsafe state: {state}")]
    UnsafeRepositoryState { state: String },

    #[error("Not on a branch (detached HEAD)")]
    DetachedHead,

    #[error("No remote configured for branch {branch}")]
    NoRemoteConfigured { branch: String },

    #[error("Remote branch not found: {remote}/{branch}")]
    RemoteBranchNotFound { remote: String, branch: String },

    #[error("Branch {branch} not configured for sync")]
    BranchNotConfigured { branch: String },

    #[error("Manual intervention required: {reason}")]
    ManualInterventionRequired { reason: String },

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Authentication failed during {operation}")]
    AuthenticationFailed { operation: String },

    #[error("Git command failed: {command}\n{stderr}")]
    GitCommandFailed { command: String, stderr: String },

    #[error("Git hooks rejected commit: {details}")]
    HookRejected { details: String },

    #[error("Git error: {0}")]
    GitError(#[from] git2::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Watch error: {0}")]
    WatchError(String),

    #[error("Task error: {0}")]
    TaskError(String),

    #[error("Other error: {0}")]
    Other(String),
}

impl SyncError {
    /// Get the exit code for this error type, matching the original git-sync
    pub fn exit_code(&self) -> i32 {
        match self {
            SyncError::NotARepository { .. } => 128, // Match git's error code
            SyncError::UnsafeRepositoryState { .. } => 2,
            SyncError::DetachedHead => 2,
            SyncError::NoRemoteConfigured { .. } => 2,
            SyncError::RemoteBranchNotFound { .. } => 2,
            SyncError::BranchNotConfigured { .. } => 1,
            SyncError::ManualInterventionRequired { .. } => 1,
            SyncError::NetworkError(_) => 3,
            SyncError::AuthenticationFailed { .. } => 3,
            SyncError::GitCommandFailed { .. } => 2,
            SyncError::HookRejected { .. } => 1,
            SyncError::GitError(e) => {
                // Map git2 errors to appropriate exit codes
                match e.code() {
                    git2::ErrorCode::NotFound => 128,
                    git2::ErrorCode::Conflict => 1,
                    git2::ErrorCode::Locked => 2,
                    _ => 2,
                }
            }
            SyncError::IoError(_) => 2,
            SyncError::WatchError(_) => 2,
            SyncError::TaskError(_) => 2,
            SyncError::Other(_) => 2,
        }
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;

impl From<notify::Error> for SyncError {
    fn from(err: notify::Error) -> Self {
        SyncError::WatchError(err.to_string())
    }
}

impl From<tokio::task::JoinError> for SyncError {
    fn from(err: tokio::task::JoinError) -> Self {
        SyncError::TaskError(err.to_string())
    }
}

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("Not a git repository")]
    NotARepository,

    #[error("Repository in unsafe state: {state}")]
    UnsafeRepositoryState { state: String },

    #[error("Not on a branch (detached HEAD)")]
    DetachedHead,

    #[error("No remote configured for branch {branch}")]
    NoRemoteConfigured { branch: String },

    #[error("Branch {branch} not configured for sync")]
    BranchNotConfigured { branch: String },

    #[error("Manual intervention required: {reason}")]
    ManualInterventionRequired { reason: String },

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Git error: {0}")]
    GitError(#[from] git2::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Other error: {0}")]
    Other(String),
}

impl SyncError {
    /// Get the exit code for this error type, matching the original git-sync
    pub fn exit_code(&self) -> i32 {
        match self {
            SyncError::NotARepository => 128, // Match git's error code
            SyncError::UnsafeRepositoryState { .. } => 2,
            SyncError::DetachedHead => 2,
            SyncError::NoRemoteConfigured { .. } => 2,
            SyncError::BranchNotConfigured { .. } => 1,
            SyncError::ManualInterventionRequired { .. } => 1,
            SyncError::NetworkError(_) => 3,
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
            SyncError::Other(_) => 2,
        }
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;
pub mod config;
pub mod error;
pub mod sync;
pub mod watch;

pub use config::{Config, ConfigLoader, DefaultConfig, RepositoryConfig};
pub use error::{Result, SyncError};
pub use sync::{
    FallbackState, RepositoryState, RepositorySynchronizer, SyncConfig, SyncState,
    UnhandledFileState, FALLBACK_BRANCH_PREFIX,
};
pub use watch::{watch_with_periodic_sync, WatchConfig, WatchManager};

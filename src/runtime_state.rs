use crate::{Result, SyncError};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Deserialize, Serialize)]
struct RuntimeState {
    #[serde(default)]
    suspended_repositories: BTreeSet<PathBuf>,
}

#[derive(Debug)]
struct SuspensionStoreInner {
    path: PathBuf,
    state: RuntimeState,
}

/// Process-shared access to persistent, user-controlled synchronization state.
///
/// This deliberately lives outside `config.toml`: suspension is an operational
/// choice made from the tray, not declarative repository configuration.
#[derive(Clone, Debug)]
pub struct SuspensionStore {
    inner: Arc<Mutex<SuspensionStoreInner>>,
}

impl SuspensionStore {
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let state = if path.exists() {
            let contents = fs::read_to_string(&path)?;
            toml::from_str(&contents).map_err(|error| {
                SyncError::Other(format!(
                    "Failed to parse runtime state at {}: {error}",
                    path.display()
                ))
            })?
        } else {
            RuntimeState::default()
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(SuspensionStoreInner { path, state })),
        })
    }

    pub fn default_path() -> Result<PathBuf> {
        let project_dirs = ProjectDirs::from("", "", "git-sync-rs").ok_or_else(|| {
            SyncError::Other("Could not determine git-sync-rs state directory".to_string())
        })?;
        let directory = project_dirs
            .state_dir()
            .unwrap_or_else(|| project_dirs.data_local_dir());
        Ok(directory.join("state.toml"))
    }

    pub fn is_suspended(&self, repo_path: &Path) -> bool {
        let key = repository_key(repo_path);
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state
            .suspended_repositories
            .contains(&key)
    }

    pub fn set_suspended(&self, repo_path: &Path, suspended: bool) -> Result<()> {
        let key = repository_key(repo_path);
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_suspended = inner.state.suspended_repositories.contains(&key);
        if was_suspended == suspended {
            return Ok(());
        }

        if suspended {
            inner.state.suspended_repositories.insert(key.clone());
        } else {
            inner.state.suspended_repositories.remove(&key);
        }

        if let Err(error) = save_atomically(&inner.path, &inner.state) {
            if was_suspended {
                inner.state.suspended_repositories.insert(key);
            } else {
                inner.state.suspended_repositories.remove(&key);
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn path(&self) -> PathBuf {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .path
            .clone()
    }
}

fn repository_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn save_atomically(path: &Path, state: &RuntimeState) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        SyncError::Other(format!(
            "Runtime state path has no parent: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent)?;

    let contents = toml::to_string_pretty(state)
        .map_err(|error| SyncError::Other(format!("Failed to serialize runtime state: {error}")))?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.toml");
    let temporary_path = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::SuspensionStore;
    use std::path::Path;

    #[test]
    fn suspension_survives_reload_and_preserves_other_repositories() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let state_path = temp.path().join("nested/state.toml");
        let first = Path::new("/repository/one");
        let second = Path::new("/repository/two");

        let store = SuspensionStore::load(state_path.clone()).expect("load empty state");
        store.set_suspended(first, true).expect("suspend first");
        store.set_suspended(second, true).expect("suspend second");

        let reloaded = SuspensionStore::load(state_path.clone()).expect("reload state");
        assert!(reloaded.is_suspended(first));
        assert!(reloaded.is_suspended(second));

        reloaded.set_suspended(first, false).expect("resume first");
        let final_state = SuspensionStore::load(state_path).expect("reload final state");
        assert!(!final_state.is_suspended(first));
        assert!(final_state.is_suspended(second));
    }
}

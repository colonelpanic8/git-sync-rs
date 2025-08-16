use crate::error::{Result, SyncError};
use chrono::Local;
use git2::{BranchType, Repository, Status, StatusOptions};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// Configuration for the synchronizer
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Whether to sync new/untracked files
    pub sync_new_files: bool,

    /// Whether to skip git hooks when committing
    pub skip_hooks: bool,

    /// Custom commit message (can include {hostname} and {timestamp} placeholders)
    pub commit_message: Option<String>,

    /// Remote name to sync with (e.g., "origin")
    pub remote_name: String,

    /// Branch name to sync
    pub branch_name: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            sync_new_files: true, // Default to syncing untracked files
            skip_hooks: false,
            commit_message: None,
            remote_name: "origin".to_string(),
            branch_name: "main".to_string(),
        }
    }
}

/// Repository state that might prevent syncing
#[derive(Debug, Clone, PartialEq)]
pub enum RepositoryState {
    /// Repository is clean and ready
    Clean,

    /// Repository has uncommitted changes
    Dirty,

    /// Repository is in the middle of a rebase
    Rebasing,

    /// Repository is in the middle of a merge
    Merging,

    /// Repository is cherry-picking
    CherryPicking,

    /// Repository is bisecting
    Bisecting,

    /// Repository is applying patches (git am)
    ApplyingPatches,

    /// HEAD is detached
    DetachedHead,
}

/// Sync state relative to remote
#[derive(Debug, Clone, PartialEq)]
pub enum SyncState {
    /// Local and remote are equal
    Equal,

    /// Local is ahead of remote
    Ahead(usize),

    /// Local is behind remote
    Behind(usize),

    /// Local and remote have diverged
    Diverged { ahead: usize, behind: usize },

    /// No upstream branch
    NoUpstream,
}

/// Unhandled file state that prevents sync
#[derive(Debug, Clone, PartialEq)]
pub enum UnhandledFileState {
    /// File has merge conflicts
    Conflicted { path: String },
}

/// Main synchronizer struct
pub struct RepositorySynchronizer {
    repo: Repository,
    config: SyncConfig,
    _repo_path: PathBuf,
}

impl RepositorySynchronizer {
    /// Create a new synchronizer for the given repository path
    pub fn new(repo_path: impl AsRef<Path>, config: SyncConfig) -> Result<Self> {
        let repo_path = repo_path.as_ref().to_path_buf();
        let repo = Repository::open(&repo_path).map_err(|_| SyncError::NotARepository {
            path: repo_path.display().to_string(),
        })?;

        Ok(Self {
            repo,
            config,
            _repo_path: repo_path,
        })
    }

    /// Create a new synchronizer with auto-detected branch name
    pub fn new_with_detected_branch(
        repo_path: impl AsRef<Path>,
        mut config: SyncConfig,
    ) -> Result<Self> {
        let repo_path = repo_path.as_ref().to_path_buf();
        let repo = Repository::open(&repo_path).map_err(|_| SyncError::NotARepository {
            path: repo_path.display().to_string(),
        })?;

        // Try to detect current branch
        if let Ok(head) = repo.head() {
            if head.is_branch() {
                if let Some(branch_name) = head.shorthand() {
                    config.branch_name = branch_name.to_string();
                }
            }
        }

        Ok(Self {
            repo,
            config,
            _repo_path: repo_path,
        })
    }

    /// Get the current repository state
    pub fn get_repository_state(&self) -> Result<RepositoryState> {
        // Check if HEAD is detached
        if self.repo.head_detached()? {
            return Ok(RepositoryState::DetachedHead);
        }

        // Check for various in-progress operations
        let state = self.repo.state();
        match state {
            git2::RepositoryState::Clean => {
                // Check if working directory is dirty
                let mut status_opts = StatusOptions::new();
                status_opts.include_untracked(true);
                let statuses = self.repo.statuses(Some(&mut status_opts))?;

                if statuses.is_empty() {
                    Ok(RepositoryState::Clean)
                } else {
                    Ok(RepositoryState::Dirty)
                }
            }
            git2::RepositoryState::Merge => Ok(RepositoryState::Merging),
            git2::RepositoryState::Rebase
            | git2::RepositoryState::RebaseInteractive
            | git2::RepositoryState::RebaseMerge => Ok(RepositoryState::Rebasing),
            git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
                Ok(RepositoryState::CherryPicking)
            }
            git2::RepositoryState::Bisect => Ok(RepositoryState::Bisecting),
            git2::RepositoryState::ApplyMailbox | git2::RepositoryState::ApplyMailboxOrRebase => {
                Ok(RepositoryState::ApplyingPatches)
            }
            _ => Ok(RepositoryState::Clean),
        }
    }

    /// Check if there are local changes that need to be committed
    pub fn has_local_changes(&self) -> Result<bool> {
        let mut status_opts = StatusOptions::new();
        status_opts.include_untracked(self.config.sync_new_files);

        let statuses = self.repo.statuses(Some(&mut status_opts))?;

        for entry in statuses.iter() {
            let status = entry.status();

            if self.config.sync_new_files {
                // Check for any changes including new files
                if status.intersects(
                    Status::WT_MODIFIED
                        | Status::WT_DELETED
                        | Status::WT_RENAMED
                        | Status::WT_TYPECHANGE
                        | Status::WT_NEW,
                ) {
                    return Ok(true);
                }
            } else {
                // Only check for modifications to tracked files
                if status.intersects(
                    Status::WT_MODIFIED
                        | Status::WT_DELETED
                        | Status::WT_RENAMED
                        | Status::WT_TYPECHANGE,
                ) {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Check if there are unhandled file states that should prevent sync
    pub fn check_unhandled_files(&self) -> Result<Option<UnhandledFileState>> {
        let mut status_opts = StatusOptions::new();
        status_opts.include_untracked(true);

        let statuses = self.repo.statuses(Some(&mut status_opts))?;

        for entry in statuses.iter() {
            let status = entry.status();
            let path = entry.path().unwrap_or("<unknown>").to_string();

            // Check for conflicted files
            if status.is_conflicted() {
                return Ok(Some(UnhandledFileState::Conflicted { path }));
            }
        }

        Ok(None)
    }

    /// Get the current branch name
    pub fn get_current_branch(&self) -> Result<String> {
        let head = self.repo.head()?;

        if !head.is_branch() {
            return Err(SyncError::DetachedHead);
        }

        let branch_name = head
            .shorthand()
            .ok_or_else(|| SyncError::Other("Could not get branch name".to_string()))?;

        Ok(branch_name.to_string())
    }

    /// Get the sync state relative to the remote
    pub fn get_sync_state(&self) -> Result<SyncState> {
        let branch_name = self.get_current_branch()?;
        let local_branch = self.repo.find_branch(&branch_name, BranchType::Local)?;

        // Get the upstream branch
        let upstream = match local_branch.upstream() {
            Ok(upstream) => upstream,
            Err(_) => return Ok(SyncState::NoUpstream),
        };

        // Get the OIDs for comparison
        let local_oid = local_branch
            .get()
            .target()
            .ok_or_else(|| SyncError::Other("Could not get local branch OID".to_string()))?;
        let upstream_oid = upstream
            .get()
            .target()
            .ok_or_else(|| SyncError::Other("Could not get upstream branch OID".to_string()))?;

        // If they're the same, we're in sync
        if local_oid == upstream_oid {
            return Ok(SyncState::Equal);
        }

        // Count commits ahead and behind
        let (ahead, behind) = self.repo.graph_ahead_behind(local_oid, upstream_oid)?;

        match (ahead, behind) {
            (0, 0) => Ok(SyncState::Equal),
            (a, 0) if a > 0 => Ok(SyncState::Ahead(a)),
            (0, b) if b > 0 => Ok(SyncState::Behind(b)),
            (a, b) if a > 0 && b > 0 => Ok(SyncState::Diverged {
                ahead: a,
                behind: b,
            }),
            _ => Ok(SyncState::Equal),
        }
    }

    /// Auto-commit local changes
    pub fn auto_commit(&self) -> Result<()> {
        info!("Auto-committing local changes");

        // Stage changes
        let mut index = self.repo.index()?;

        if self.config.sync_new_files {
            // Add all changes including new files
            index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        } else {
            // Only update tracked files
            index.update_all(["*"].iter(), None)?;
        }

        index.write()?;

        // Check if there's anything to commit
        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;

        let parent_commit = self.repo.head()?.peel_to_commit()?;
        if parent_commit.tree_id() == tree_id {
            debug!("No changes to commit");
            return Ok(());
        }

        // Prepare commit message
        let message = if let Some(ref msg) = self.config.commit_message {
            msg.replace("{hostname}", &hostname::get()?.to_string_lossy())
                .replace(
                    "{timestamp}",
                    &Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                )
        } else {
            format!(
                "changes from {} on {}",
                hostname::get()?.to_string_lossy(),
                Local::now().format("%Y-%m-%d %H:%M:%S")
            )
        };

        // Create signature
        let sig = self.repo.signature()?;

        // Create commit
        self.repo
            .commit(Some("HEAD"), &sig, &sig, &message, &tree, &[&parent_commit])?;

        info!("Created auto-commit: {}", message);
        Ok(())
    }

    /// Fetch from remote
    pub fn fetch(&self) -> Result<()> {
        info!("Fetching from remote: {}", self.config.remote_name);

        // Use git command directly as a workaround for SSH issues
        use std::process::Command;

        let output = Command::new("git")
            .arg("fetch")
            .arg(&self.config.remote_name)
            .arg(&self.config.branch_name)
            .current_dir(&self._repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git fetch: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Git fetch failed: {}", stderr);
            return Err(SyncError::Other(format!("Git fetch failed: {}", stderr)));
        }

        info!(
            "Fetch completed successfully from remote: {}",
            self.config.remote_name
        );
        return Ok(());

        // Original libgit2 implementation (keeping for reference)
        #[allow(unreachable_code)]
        {
            let mut remote = self.repo.find_remote(&self.config.remote_name)?;

            // Log the remote URL for debugging
            if let Some(url) = remote.url() {
                debug!("Remote URL: {}", url);
            }

            // Prepare callbacks for authentication
            let mut callbacks = git2::RemoteCallbacks::new();
            callbacks.credentials(|url, username_from_url, allowed_types| {
                debug!(
                    "Authentication callback: url={}, username={:?}, allowed_types={:?}",
                    url, username_from_url, allowed_types
                );

                let username = username_from_url.unwrap_or("git");

                // First try SSH agent
                debug!("Trying SSH key from agent with username: {}", username);
                match git2::Cred::ssh_key_from_agent(username) {
                    Ok(cred) => {
                        debug!("Successfully obtained SSH credentials from agent");
                        return Ok(cred);
                    }
                    Err(e) => {
                        debug!("SSH agent failed: {}, trying default SSH key", e);
                    }
                }

                // Fallback to default SSH key
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                let ssh_dir = std::path::Path::new(&home).join(".ssh");
                let private_key = ssh_dir.join("id_rsa");
                let public_key = ssh_dir.join("id_rsa.pub");

                // Try id_rsa first
                if private_key.exists() {
                    debug!("Trying SSH key from {:?}", private_key);
                    match git2::Cred::ssh_key(username, Some(&public_key), &private_key, None) {
                        Ok(cred) => {
                            debug!("Successfully using SSH key from disk");
                            return Ok(cred);
                        }
                        Err(e) => {
                            debug!("Failed to use id_rsa: {}", e);
                        }
                    }
                }

                // Try id_ed25519
                let private_key = ssh_dir.join("id_ed25519");
                let public_key = ssh_dir.join("id_ed25519.pub");
                if private_key.exists() {
                    debug!("Trying SSH key from {:?}", private_key);
                    match git2::Cred::ssh_key(username, Some(&public_key), &private_key, None) {
                        Ok(cred) => {
                            debug!("Successfully using ed25519 SSH key from disk");
                            return Ok(cred);
                        }
                        Err(e) => {
                            debug!("Failed to use id_ed25519: {}", e);
                        }
                    }
                }

                error!("No working SSH authentication method found");
                Err(git2::Error::from_str(
                    "No SSH authentication method available",
                ))
            });

            // Add progress callback
            callbacks.transfer_progress(|stats| {
                debug!(
                    "Fetch progress: {}/{} objects, {} bytes received",
                    stats.received_objects(),
                    stats.total_objects(),
                    stats.received_bytes()
                );
                true
            });

            let mut fetch_options = git2::FetchOptions::new();
            fetch_options.remote_callbacks(callbacks);

            // Try to set proxy options from git config
            let mut proxy_options = git2::ProxyOptions::new();
            proxy_options.auto();
            fetch_options.proxy_options(proxy_options);

            debug!("Starting fetch for branch: {}", self.config.branch_name);
            debug!(
                "Fetching refspec: refs/heads/{}:refs/remotes/{}/{}",
                self.config.branch_name, self.config.remote_name, self.config.branch_name
            );

            // Fetch the branch
            match remote.fetch(&[&self.config.branch_name], Some(&mut fetch_options), None) {
                Ok(_) => {
                    info!(
                        "Fetch completed successfully from remote: {}",
                        self.config.remote_name
                    );
                    Ok(())
                }
                Err(e) => {
                    error!(
                        "Fetch failed from remote {}: {}",
                        self.config.remote_name, e
                    );
                    Err(e.into())
                }
            }
        }
    }

    /// Push to remote
    pub fn push(&self) -> Result<()> {
        info!("Pushing to remote: {}", self.config.remote_name);

        // Use git command directly as a workaround for SSH issues
        use std::process::Command;

        let refspec = format!("{}:{}", self.config.branch_name, self.config.branch_name);

        let output = Command::new("git")
            .arg("push")
            .arg(&self.config.remote_name)
            .arg(&refspec)
            .current_dir(&self._repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git push: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Git push failed: {}", stderr);
            return Err(SyncError::Other(format!("Git push failed: {}", stderr)));
        }

        info!(
            "Push completed successfully to remote: {}",
            self.config.remote_name
        );
        return Ok(());

        // Original libgit2 implementation (keeping for reference)
        #[allow(unreachable_code)]
        {
            let mut remote = self.repo.find_remote(&self.config.remote_name)?;

            // Prepare callbacks for authentication
            let mut callbacks = git2::RemoteCallbacks::new();
            callbacks.credentials(|url, username_from_url, allowed_types| {
                debug!(
                    "Authentication callback: url={}, username={:?}, allowed_types={:?}",
                    url, username_from_url, allowed_types
                );

                let username = username_from_url.unwrap_or("git");

                // First try SSH agent
                debug!("Trying SSH key from agent with username: {}", username);
                match git2::Cred::ssh_key_from_agent(username) {
                    Ok(cred) => {
                        debug!("Successfully obtained SSH credentials from agent");
                        return Ok(cred);
                    }
                    Err(e) => {
                        debug!("SSH agent failed: {}, trying default SSH key", e);
                    }
                }

                // Fallback to default SSH key
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                let ssh_dir = std::path::Path::new(&home).join(".ssh");
                let private_key = ssh_dir.join("id_rsa");
                let public_key = ssh_dir.join("id_rsa.pub");

                // Try id_rsa first
                if private_key.exists() {
                    debug!("Trying SSH key from {:?}", private_key);
                    match git2::Cred::ssh_key(username, Some(&public_key), &private_key, None) {
                        Ok(cred) => {
                            debug!("Successfully using SSH key from disk");
                            return Ok(cred);
                        }
                        Err(e) => {
                            debug!("Failed to use id_rsa: {}", e);
                        }
                    }
                }

                // Try id_ed25519
                let private_key = ssh_dir.join("id_ed25519");
                let public_key = ssh_dir.join("id_ed25519.pub");
                if private_key.exists() {
                    debug!("Trying SSH key from {:?}", private_key);
                    match git2::Cred::ssh_key(username, Some(&public_key), &private_key, None) {
                        Ok(cred) => {
                            debug!("Successfully using ed25519 SSH key from disk");
                            return Ok(cred);
                        }
                        Err(e) => {
                            debug!("Failed to use id_ed25519: {}", e);
                        }
                    }
                }

                error!("No working SSH authentication method found");
                Err(git2::Error::from_str(
                    "No SSH authentication method available",
                ))
            });

            let mut push_options = git2::PushOptions::new();
            push_options.remote_callbacks(callbacks);

            // Try to set proxy options from git config
            let mut proxy_options = git2::ProxyOptions::new();
            proxy_options.auto();
            push_options.proxy_options(proxy_options);

            // Push the branch
            let refspec = format!(
                "refs/heads/{}:refs/heads/{}",
                self.config.branch_name, self.config.branch_name
            );

            debug!("Pushing refspec: {}", refspec);
            match remote.push(&[&refspec], Some(&mut push_options)) {
                Ok(_) => {
                    info!(
                        "Push completed successfully to remote: {}",
                        self.config.remote_name
                    );
                    Ok(())
                }
                Err(e) => {
                    error!("Push failed to remote {}: {}", self.config.remote_name, e);
                    Err(e.into())
                }
            }
        }
    }

    /// Perform a fast-forward merge
    pub fn fast_forward_merge(&self) -> Result<()> {
        info!("Performing fast-forward merge");

        let branch_name = self.get_current_branch()?;
        let local_branch = self.repo.find_branch(&branch_name, BranchType::Local)?;
        let upstream = local_branch.upstream()?;

        let upstream_oid = upstream
            .get()
            .target()
            .ok_or_else(|| SyncError::Other("Could not get upstream OID".to_string()))?;

        // Fast-forward by moving the reference
        let mut reference = self.repo.head()?;
        reference.set_target(upstream_oid, "fast-forward merge")?;

        // Checkout the new HEAD to update working directory
        let object = self.repo.find_object(upstream_oid, None)?;
        let mut checkout_builder = git2::build::CheckoutBuilder::new();
        checkout_builder.force(); // Force update working directory files
        self.repo
            .checkout_tree(&object, Some(&mut checkout_builder))?;

        // Update HEAD to point to the new commit
        self.repo.set_head(&format!("refs/heads/{}", branch_name))?;

        info!("Fast-forward merge completed - working tree updated");
        Ok(())
    }

    /// Perform a rebase
    pub fn rebase(&self) -> Result<()> {
        info!("Performing rebase");

        let branch_name = self.get_current_branch()?;
        let local_branch = self.repo.find_branch(&branch_name, BranchType::Local)?;
        let upstream = local_branch.upstream()?;

        let upstream_commit = upstream.get().peel_to_commit()?;
        let local_commit = local_branch.get().peel_to_commit()?;

        // Find merge base
        let merge_base = self
            .repo
            .merge_base(local_commit.id(), upstream_commit.id())?;
        let _merge_base_commit = self.repo.find_commit(merge_base)?;

        // Create signature
        let sig = self.repo.signature()?;

        // Get annotated commits from references
        let local_annotated = self
            .repo
            .reference_to_annotated_commit(local_branch.get())?;
        let upstream_annotated = self.repo.reference_to_annotated_commit(upstream.get())?;

        // Start rebase
        let mut rebase = self.repo.rebase(
            Some(&local_annotated),
            Some(&upstream_annotated),
            None,
            None,
        )?;

        // Process each commit
        while let Some(operation) = rebase.next() {
            let _operation = operation?;

            // Check if we can continue
            if self.repo.index()?.has_conflicts() {
                warn!("Conflicts detected during rebase");
                rebase.abort()?;
                return Err(SyncError::ManualInterventionRequired {
                    reason: "Rebase conflicts detected. Please resolve manually.".to_string(),
                });
            }

            // Continue with the rebase
            rebase.commit(None, &sig, None)?;
        }

        // Finish the rebase
        rebase.finish(Some(&sig))?;

        // Ensure working tree is properly updated after rebase
        let head = self.repo.head()?;
        let head_commit = head.peel_to_commit()?;
        let mut checkout_builder = git2::build::CheckoutBuilder::new();
        checkout_builder.force();
        self.repo
            .checkout_tree(head_commit.as_object(), Some(&mut checkout_builder))?;

        info!("Rebase completed successfully - working tree updated");
        Ok(())
    }

    /// Main sync operation
    pub fn sync(&self, check_only: bool) -> Result<()> {
        info!("Starting sync operation (check_only: {})", check_only);

        // Check repository state
        let repo_state = self.get_repository_state()?;
        match repo_state {
            RepositoryState::Clean | RepositoryState::Dirty => {
                // These states are OK to continue
            }
            RepositoryState::DetachedHead => {
                return Err(SyncError::DetachedHead);
            }
            _ => {
                return Err(SyncError::UnsafeRepositoryState {
                    state: format!("{:?}", repo_state),
                });
            }
        }

        // Check for unhandled files
        if let Some(unhandled) = self.check_unhandled_files()? {
            let reason = match unhandled {
                UnhandledFileState::Conflicted { path } => format!("Conflicted file: {}", path),
            };
            return Err(SyncError::ManualInterventionRequired { reason });
        }

        // If we're only checking, we're done
        if check_only {
            info!("Check passed, sync can proceed");
            return Ok(());
        }

        // Auto-commit if there are local changes
        if self.has_local_changes()? {
            self.auto_commit()?;
        }

        // Fetch from remote
        self.fetch()?;

        // Get sync state and handle accordingly
        let sync_state = self.get_sync_state()?;
        match sync_state {
            SyncState::Equal => {
                info!("Already in sync");
            }
            SyncState::Ahead(_) => {
                info!("Local is ahead, pushing");
                self.push()?;
            }
            SyncState::Behind(_) => {
                info!("Local is behind, fast-forwarding");
                self.fast_forward_merge()?;
            }
            SyncState::Diverged { .. } => {
                info!("Branches have diverged, rebasing");
                self.rebase()?;
                // After successful rebase, push the changes
                self.push()?;
            }
            SyncState::NoUpstream => {
                return Err(SyncError::NoRemoteConfigured {
                    branch: self.config.branch_name.clone(),
                });
            }
        }

        // Verify we're in sync
        let final_state = self.get_sync_state()?;
        if final_state != SyncState::Equal {
            warn!(
                "Sync completed but repository is not in sync: {:?}",
                final_state
            );
            return Err(SyncError::Other(
                "Sync completed but repository is not in sync".to_string(),
            ));
        }

        info!("Sync completed successfully");
        Ok(())
    }
}

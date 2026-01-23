use crate::error::{Result, SyncError};
use chrono::Local;
use git2::{BranchType, MergeOptions, Oid, Repository, Status, StatusOptions};
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// Prefix for fallback branches created by git-sync
pub const FALLBACK_BRANCH_PREFIX: &str = "git-sync/";

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

    /// Branch name to sync (current working branch)
    pub branch_name: String,

    /// When true, create a fallback branch on merge conflicts instead of failing
    pub conflict_branch: bool,

    /// The target branch we want to track (used for returning from fallback)
    /// If None, defaults to the repository's default branch
    pub target_branch: Option<String>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            sync_new_files: true, // Default to syncing untracked files
            skip_hooks: false,
            commit_message: None,
            remote_name: "origin".to_string(),
            branch_name: "main".to_string(),
            conflict_branch: false,
            target_branch: None,
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

/// State for tracking fallback branch return attempts (in-memory only)
#[derive(Debug, Clone, Default)]
pub struct FallbackState {
    /// The OID of the target branch when we last checked if return was possible
    /// Used to avoid redundant merge checks when target hasn't moved
    pub last_checked_target_oid: Option<Oid>,
}

/// Main synchronizer struct
pub struct RepositorySynchronizer {
    repo: Repository,
    config: SyncConfig,
    _repo_path: PathBuf,
    fallback_state: FallbackState,
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
            fallback_state: FallbackState::default(),
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
            fallback_state: FallbackState::default(),
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
                    &Local::now().format("%Y-%m-%d %I:%M:%S %p %Z").to_string(),
                )
        } else {
            format!(
                "changes from {} on {}",
                hostname::get()?.to_string_lossy(),
                Local::now().format("%Y-%m-%d %I:%M:%S %p %Z")
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

    /// Fetch a specific branch from remote
    pub fn fetch_branch(&self, branch: &str) -> Result<()> {
        info!("Fetching branch {} from remote: {}", branch, self.config.remote_name);

        use std::process::Command;

        let output = Command::new("git")
            .arg("fetch")
            .arg(&self.config.remote_name)
            .arg(branch)
            .current_dir(&self._repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git fetch: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Git fetch failed: {}", stderr);
            return Err(SyncError::Other(format!("Git fetch failed: {}", stderr)));
        }

        info!(
            "Fetch completed successfully for branch {} from remote: {}",
            branch, self.config.remote_name
        );
        Ok(())
    }

    /// Fetch from remote
    pub fn fetch(&self) -> Result<()> {
        self.fetch_branch(&self.config.branch_name)?;

        // If we're on a fallback branch and have a target branch, also fetch that
        if self.config.conflict_branch {
            if let Ok(target) = self.get_target_branch() {
                if target != self.config.branch_name {
                    // Ignore errors fetching target - it might not be necessary
                    let _ = self.fetch_branch(&target);
                }
            }
        }

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

                // If conflict_branch is enabled, create a fallback branch
                if self.config.conflict_branch {
                    return self.handle_conflict_with_fallback();
                }

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

    /// Detect the repository's default branch
    pub fn detect_default_branch(&self) -> Result<String> {
        // Try to get the default branch from origin/HEAD
        if let Ok(reference) = self.repo.find_reference("refs/remotes/origin/HEAD") {
            if let Ok(resolved) = reference.resolve() {
                if let Some(name) = resolved.shorthand() {
                    // name will be like "origin/main", extract just "main"
                    if let Some(branch) = name.strip_prefix("origin/") {
                        debug!("Detected default branch from origin/HEAD: {}", branch);
                        return Ok(branch.to_string());
                    }
                }
            }
        }

        // Fallback: check if main or master exists
        if self.repo.find_branch("main", BranchType::Local).is_ok()
            || self
                .repo
                .find_reference("refs/remotes/origin/main")
                .is_ok()
        {
            debug!("Falling back to 'main' as default branch");
            return Ok("main".to_string());
        }

        if self.repo.find_branch("master", BranchType::Local).is_ok()
            || self
                .repo
                .find_reference("refs/remotes/origin/master")
                .is_ok()
        {
            debug!("Falling back to 'master' as default branch");
            return Ok("master".to_string());
        }

        // Last resort: use current branch
        self.get_current_branch()
    }

    /// Get the target branch (the branch we want to be on)
    pub fn get_target_branch(&self) -> Result<String> {
        if let Some(ref target) = self.config.target_branch {
            if !target.is_empty() {
                return Ok(target.clone());
            }
        }
        self.detect_default_branch()
    }

    /// Check if we're currently on a fallback branch
    pub fn is_on_fallback_branch(&self) -> Result<bool> {
        let current = self.get_current_branch()?;
        Ok(current.starts_with(FALLBACK_BRANCH_PREFIX))
    }

    /// Generate a fallback branch name
    fn generate_fallback_branch_name() -> String {
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let timestamp = Local::now().format("%Y-%m-%d-%H%M%S");
        format!("{}{}-{}", FALLBACK_BRANCH_PREFIX, hostname, timestamp)
    }

    /// Create and switch to a fallback branch
    pub fn create_fallback_branch(&self) -> Result<String> {
        let branch_name = Self::generate_fallback_branch_name();
        info!("Creating fallback branch: {}", branch_name);

        // Get current HEAD commit
        let head_commit = self.repo.head()?.peel_to_commit()?;

        // Create the new branch
        self.repo
            .branch(&branch_name, &head_commit, false)
            .map_err(|e| SyncError::Other(format!("Failed to create fallback branch: {}", e)))?;

        // Checkout the new branch
        let refname = format!("refs/heads/{}", branch_name);
        self.repo.set_head(&refname)?;

        // Update working directory
        let mut checkout_builder = git2::build::CheckoutBuilder::new();
        checkout_builder.force();
        self.repo
            .checkout_head(Some(&mut checkout_builder))
            .map_err(|e| {
                SyncError::Other(format!("Failed to checkout fallback branch: {}", e))
            })?;

        info!("Switched to fallback branch: {}", branch_name);
        Ok(branch_name)
    }

    /// Push a branch to remote (used for fallback branches)
    pub fn push_branch(&self, branch_name: &str) -> Result<()> {
        info!("Pushing branch {} to remote", branch_name);

        use std::process::Command;

        let output = Command::new("git")
            .arg("push")
            .arg("-u")
            .arg(&self.config.remote_name)
            .arg(branch_name)
            .current_dir(&self._repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git push: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Git push failed: {}", stderr);
            return Err(SyncError::Other(format!("Git push failed: {}", stderr)));
        }

        info!("Successfully pushed branch {} to remote", branch_name);
        Ok(())
    }

    /// Check if merging target branch into current HEAD would succeed (in-memory, no working tree changes)
    pub fn can_merge_cleanly(&self, target_branch: &str) -> Result<bool> {
        // Get the target branch reference
        let target_ref = format!("refs/remotes/{}/{}", self.config.remote_name, target_branch);
        let target_reference = self.repo.find_reference(&target_ref).map_err(|e| {
            SyncError::Other(format!(
                "Failed to find target branch {}: {}",
                target_branch, e
            ))
        })?;
        let target_commit = target_reference.peel_to_commit()?;

        // Get current HEAD
        let head_commit = self.repo.head()?.peel_to_commit()?;

        // Check if we're already ancestors (fast-forward possible)
        if self
            .repo
            .graph_descendant_of(target_commit.id(), head_commit.id())?
        {
            debug!(
                "Target branch {} is descendant of current HEAD, clean merge possible",
                target_branch
            );
            return Ok(true);
        }

        // Perform in-memory merge to check for conflicts
        let merge_opts = MergeOptions::new();
        let index = self
            .repo
            .merge_commits(&head_commit, &target_commit, Some(&merge_opts))
            .map_err(|e| SyncError::Other(format!("Failed to perform merge check: {}", e)))?;

        let has_conflicts = index.has_conflicts();
        debug!(
            "In-memory merge check: has_conflicts={}",
            has_conflicts
        );

        Ok(!has_conflicts)
    }

    /// Get the OID of the target branch on remote
    fn get_target_branch_oid(&self, target_branch: &str) -> Result<Oid> {
        let target_ref = format!("refs/remotes/{}/{}", self.config.remote_name, target_branch);
        let reference = self.repo.find_reference(&target_ref)?;
        reference
            .target()
            .ok_or_else(|| SyncError::Other("Target branch has no OID".to_string()))
    }

    /// Attempt to return to the target branch from a fallback branch
    pub fn try_return_to_target(&mut self) -> Result<bool> {
        if !self.is_on_fallback_branch()? {
            return Ok(false);
        }

        let target_branch = self.get_target_branch()?;
        info!(
            "On fallback branch, checking if we can return to {}",
            target_branch
        );

        // Get current target branch OID
        let target_oid = match self.get_target_branch_oid(&target_branch) {
            Ok(oid) => oid,
            Err(e) => {
                warn!("Could not find target branch {}: {}", target_branch, e);
                return Ok(false);
            }
        };

        // Check if target has moved since last check
        if let Some(last_checked) = self.fallback_state.last_checked_target_oid {
            if last_checked == target_oid {
                debug!(
                    "Target branch {} hasn't changed since last check, skipping merge check",
                    target_branch
                );
                return Ok(false);
            }
        }

        // Target has moved, check if we can merge cleanly
        if !self.can_merge_cleanly(&target_branch)? {
            info!(
                "Cannot cleanly merge {} into current branch, staying on fallback",
                target_branch
            );
            self.fallback_state.last_checked_target_oid = Some(target_oid);
            return Ok(false);
        }

        info!(
            "Clean merge possible, returning to target branch {}",
            target_branch
        );

        // Get current branch commits that need to be rebased onto target
        let current_branch = self.get_current_branch()?;
        let current_oid = self.repo.head()?.target().ok_or_else(|| {
            SyncError::Other("Current HEAD has no OID".to_string())
        })?;

        // Find merge base between our fallback branch and target
        let merge_base = self.repo.merge_base(current_oid, target_oid)?;

        // Check if we have commits to rebase
        let (ahead, _) = self.repo.graph_ahead_behind(current_oid, merge_base)?;
        let has_commits_to_rebase = ahead > 0;

        // Checkout target branch
        let target_ref = format!("refs/heads/{}", target_branch);

        // First, make sure local target branch exists and is up to date
        let remote_target_ref = format!("refs/remotes/{}/{}", self.config.remote_name, target_branch);
        let remote_target = self.repo.find_reference(&remote_target_ref)?;
        let remote_target_oid = remote_target.target().ok_or_else(|| {
            SyncError::Other("Remote target has no OID".to_string())
        })?;

        // Update or create local target branch
        if self.repo.find_reference(&target_ref).is_ok() {
            // Update existing branch
            self.repo.reference(
                &target_ref,
                remote_target_oid,
                true,
                "git-sync: updating target branch before return",
            )?;
        } else {
            // Create local tracking branch
            let remote_commit = self.repo.find_commit(remote_target_oid)?;
            self.repo.branch(&target_branch, &remote_commit, false)?;
        }

        // Checkout target branch
        self.repo.set_head(&target_ref)?;
        let mut checkout_builder = git2::build::CheckoutBuilder::new();
        checkout_builder.force();
        self.repo.checkout_head(Some(&mut checkout_builder))?;

        // Update config to reflect we're on the target branch now
        self.config.branch_name = target_branch.clone();

        if has_commits_to_rebase {
            info!(
                "Rebasing {} commits from {} onto {}",
                ahead, current_branch, target_branch
            );

            // We need to rebase our commits from the fallback branch onto target
            // Get the commits from the fallback branch
            let fallback_ref = format!("refs/heads/{}", current_branch);
            let fallback_reference = self.repo.find_reference(&fallback_ref)?;
            let fallback_annotated = self.repo.reference_to_annotated_commit(&fallback_reference)?;

            let target_reference = self.repo.find_reference(&target_ref)?;
            let target_annotated = self.repo.reference_to_annotated_commit(&target_reference)?;

            let sig = self.repo.signature()?;

            // Start rebase
            let mut rebase = self.repo.rebase(
                Some(&fallback_annotated),
                Some(&target_annotated),
                None,
                None,
            )?;

            // Process each commit
            while let Some(operation) = rebase.next() {
                let _operation = operation?;

                if self.repo.index()?.has_conflicts() {
                    warn!("Conflicts during rebase back to target, aborting");
                    rebase.abort()?;
                    // Switch back to fallback branch
                    self.repo.set_head(&fallback_ref)?;
                    self.repo.checkout_head(Some(&mut checkout_builder))?;
                    self.config.branch_name = current_branch;
                    self.fallback_state.last_checked_target_oid = Some(target_oid);
                    return Ok(false);
                }

                rebase.commit(None, &sig, None)?;
            }

            rebase.finish(Some(&sig))?;

            // Update working tree
            let head = self.repo.head()?;
            let head_commit = head.peel_to_commit()?;
            self.repo
                .checkout_tree(head_commit.as_object(), Some(&mut checkout_builder))?;
        }

        // Clear fallback state
        self.fallback_state.last_checked_target_oid = None;

        info!("Successfully returned to target branch {}", target_branch);
        Ok(true)
    }

    /// Handle a rebase conflict by creating a fallback branch (when conflict_branch is enabled)
    fn handle_conflict_with_fallback(&self) -> Result<()> {
        if !self.config.conflict_branch {
            return Err(SyncError::ManualInterventionRequired {
                reason: "Rebase conflicts detected. Please resolve manually.".to_string(),
            });
        }

        info!("Conflict detected with conflict_branch enabled, creating fallback branch");

        // Create fallback branch from current state
        let fallback_branch = self.create_fallback_branch()?;

        // Commit any uncommitted changes on the fallback branch
        if self.has_local_changes()? {
            self.auto_commit()?;
        }

        // Push the fallback branch
        self.push_branch(&fallback_branch)?;

        info!(
            "Switched to fallback branch {} due to conflicts. \
             Will automatically return to target branch when conflicts are resolved.",
            fallback_branch
        );

        Ok(())
    }

    /// Main sync operation
    pub fn sync(&mut self, check_only: bool) -> Result<()> {
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

        // Fetch from remote first (needed for both normal sync and return-to-target check)
        self.fetch()?;

        // If we're on a fallback branch and conflict_branch is enabled,
        // try to return to the target branch
        if self.config.conflict_branch
            && self.is_on_fallback_branch()?
            && self.try_return_to_target()?
        {
            // Successfully returned to target, update branch name for sync state check
            info!("Returned to target branch, continuing with normal sync");
        }

        // Auto-commit if there are local changes
        if self.has_local_changes()? {
            self.auto_commit()?;
        }

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
                // After successful rebase (or fallback branch creation), push the changes
                // Note: if we switched to a fallback branch, we need to update our branch name
                let current_branch = self.get_current_branch()?;
                if current_branch != self.config.branch_name {
                    self.config.branch_name = current_branch;
                }
                self.push()?;
            }
            SyncState::NoUpstream => {
                // If we're on a fallback branch that doesn't have upstream yet, push it
                if self.is_on_fallback_branch()? {
                    info!("Fallback branch has no upstream, pushing");
                    let branch = self.get_current_branch()?;
                    self.push_branch(&branch)?;
                } else {
                    return Err(SyncError::NoRemoteConfigured {
                        branch: self.config.branch_name.clone(),
                    });
                }
            }
        }

        // Verify we're in sync (skip for fallback branches that may not have upstream yet)
        let final_state = self.get_sync_state()?;
        if final_state != SyncState::Equal && final_state != SyncState::NoUpstream {
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

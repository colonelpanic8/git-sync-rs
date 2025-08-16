use crate::error::Result;
use crate::sync::{RepositorySynchronizer, SyncConfig};
use git2::Repository;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, error, info};

/// Watch mode configuration
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// How long to wait after changes before syncing (milliseconds)
    pub debounce_ms: u64,

    /// Minimum interval between syncs (milliseconds)
    pub min_interval_ms: u64,

    /// Whether to sync on startup
    pub sync_on_start: bool,

    /// Dry run mode - detect changes but don't sync
    pub dry_run: bool,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            min_interval_ms: 1000,
            sync_on_start: true,
            dry_run: false,
        }
    }
}

/// Manages file system watching and automatic synchronization
pub struct WatchManager {
    repo_path: String,
    sync_config: SyncConfig,
    watch_config: WatchConfig,
    is_syncing: Arc<Mutex<bool>>,
}

impl WatchManager {
    /// Create a new watch manager
    pub fn new(
        repo_path: impl AsRef<Path>,
        sync_config: SyncConfig,
        watch_config: WatchConfig,
    ) -> Self {
        // Expand tilde in path
        let path_str = repo_path.as_ref().to_string_lossy();
        let expanded = shellexpand::tilde(&path_str).to_string();
        
        Self {
            repo_path: expanded,
            sync_config,
            watch_config,
            is_syncing: Arc::new(Mutex::new(false)),
        }
    }

    /// Start watching for changes
    pub async fn watch(&self) -> Result<()> {
        info!("Starting watch mode for: {}", self.repo_path);

        // Sync on startup if configured
        if self.watch_config.sync_on_start {
            info!("Performing initial sync");
            self.perform_sync().await?;
        }

        // Create channel for file events
        let (tx, mut rx) = mpsc::channel::<Event>(100);

        // Clone repo path for the callback
        let repo_path_clone = PathBuf::from(&self.repo_path);
        
        // Setup file watcher
        let mut watcher = RecommendedWatcher::new(
            move |res: std::result::Result<Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        debug!("Raw file event received: {:?}", event);
                        
                        // Ignore git directory changes
                        if is_git_internal(&event) {
                            debug!("Ignoring git internal event");
                            return;
                        }
                        
                        // Check gitignore for each path in the event
                        let repo = match Repository::open(&repo_path_clone) {
                            Ok(r) => r,
                            Err(e) => {
                                error!("Failed to open repository for gitignore check: {}", e);
                                return;
                            }
                        };
                        
                        // Check if any path in the event should be ignored
                        let should_ignore = event.paths.iter().any(|path| {
                            should_ignore_path(&repo, &repo_path_clone, path)
                        });
                        
                        if should_ignore {
                            debug!("Ignoring gitignored file event");
                            return;
                        }
                        
                        debug!("Event is relevant, sending to channel");
                        if let Err(e) = tx.blocking_send(event.clone()) {
                            error!("Failed to send event to channel: {}", e);
                        } else {
                            debug!("Event sent successfully: {:?}", event.kind);
                        }
                    }
                    Err(e) => error!("Watch error: {}", e),
                }
            },
            Config::default(),
        )?;

        // Watch the repository path
        watcher.watch(Path::new(&self.repo_path), RecursiveMode::Recursive)?;

        info!(
            "Watching for changes (debounce: {}s)",
            self.watch_config.debounce_ms as f64 / 1000.0
        );

        // Event processing loop
        let mut last_sync = time::Instant::now();
        let mut pending_sync = false;

        loop {
            // Wait for events or timeout
            let timeout = time::sleep(Duration::from_millis(self.watch_config.debounce_ms));
            tokio::pin!(timeout);

            tokio::select! {
                Some(event) = rx.recv() => {
                    debug!("Received event from channel: {:?}", event);
                    debug!("Event kind: {:?}, paths: {:?}", event.kind, event.paths);

                    // Check if this is a relevant change
                    if is_relevant_change(&event) {
                        info!("Relevant change detected, marking pending sync");
                        pending_sync = true;
                    } else {
                        debug!("Event not considered relevant: {:?}", event.kind);
                    }
                }
                _ = &mut timeout => {
                    // Debounce period expired
                    if pending_sync {
                        // Check minimum interval
                        let elapsed = last_sync.elapsed();
                        let min_interval = Duration::from_millis(self.watch_config.min_interval_ms);

                        if elapsed >= min_interval {
                            // Check if already syncing
                            let is_syncing = self.is_syncing.lock().await;
                            if !*is_syncing {
                                drop(is_syncing); // Release lock before syncing

                                info!("Changes detected, triggering sync");
                                if let Err(e) = self.perform_sync().await {
                                    error!("Sync failed: {}", e);
                                }

                                last_sync = time::Instant::now();
                                pending_sync = false;
                            } else {
                                debug!("Sync already in progress, skipping");
                            }
                        } else {
                            debug!("Too soon since last sync, waiting");
                        }
                    }
                }
            }
        }
    }

    /// Perform a synchronization
    async fn perform_sync(&self) -> Result<()> {
        // Set syncing flag
        {
            let mut is_syncing = self.is_syncing.lock().await;
            if *is_syncing {
                debug!("Sync already in progress");
                return Ok(());
            }
            *is_syncing = true;
        }

        if self.watch_config.dry_run {
            info!("DRY RUN: Would perform sync now");
            // Clear syncing flag
            {
                let mut is_syncing = self.is_syncing.lock().await;
                *is_syncing = false;
            }
            return Ok(());
        }

        // Perform sync in blocking task
        let repo_path = self.repo_path.clone();
        let sync_config = self.sync_config.clone();

        let result = tokio::task::spawn_blocking(move || {
            // Create synchronizer
            let synchronizer =
                RepositorySynchronizer::new_with_detected_branch(&repo_path, sync_config)?;

            // Perform sync
            synchronizer.sync(false)
        })
        .await??;

        // Clear syncing flag
        {
            let mut is_syncing = self.is_syncing.lock().await;
            *is_syncing = false;
        }

        Ok(result)
    }
}

/// Check if an event is related to git internals
fn is_git_internal(event: &Event) -> bool {
    event
        .paths
        .iter()
        .any(|path| path.components().any(|c| c.as_os_str() == ".git"))
}

/// Check if a path should be ignored according to gitignore rules
fn should_ignore_path(repo: &Repository, repo_path: &Path, file_path: &Path) -> bool {
    // Make path relative to repo root
    let relative_path = match file_path.strip_prefix(repo_path) {
        Ok(p) => p,
        Err(_) => {
            debug!("Path {:?} is outside repo, ignoring", file_path);
            return true;
        }
    };
    
    // Check if the path is ignored by git
    match repo.status_should_ignore(relative_path) {
        Ok(ignored) => {
            if ignored {
                debug!("Path {:?} is gitignored", relative_path);
            }
            ignored
        }
        Err(e) => {
            debug!("Error checking gitignore status for {:?}: {}", relative_path, e);
            false
        }
    }
}

/// Check if an event represents a relevant change
fn is_relevant_change(event: &Event) -> bool {
    let is_relevant = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    );
    
    debug!(
        "is_relevant_change: kind={:?}, relevant={}",
        event.kind, is_relevant
    );
    
    is_relevant
}

/// Run watch mode with periodic sync
pub async fn watch_with_periodic_sync(
    repo_path: impl AsRef<Path>,
    sync_config: SyncConfig,
    watch_config: WatchConfig,
    sync_interval_ms: Option<u64>,
) -> Result<()> {
    let manager = WatchManager::new(repo_path, sync_config, watch_config);

    if let Some(interval_ms) = sync_interval_ms {
        // Run with periodic sync
        info!(
            "Periodic sync enabled (interval: {}s)",
            interval_ms as f64 / 1000.0
        );

        let manager_clone = Arc::new(manager);
        let manager_watch = manager_clone.clone();

        // Start watch task
        let watch_handle = tokio::spawn(async move { manager_watch.watch().await });

        // Start periodic sync task
        let periodic_handle = tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_millis(interval_ms));
            interval.tick().await; // Skip first immediate tick

            loop {
                interval.tick().await;
                info!("Periodic sync triggered");
                if let Err(e) = manager_clone.perform_sync().await {
                    error!("Periodic sync failed: {}", e);
                }
            }
        });

        // Wait for either task to finish (they shouldn't normally)
        tokio::select! {
            result = watch_handle => result?,
            result = periodic_handle => result?,
        }
    } else {
        // Just run watch mode
        manager.watch().await
    }
}

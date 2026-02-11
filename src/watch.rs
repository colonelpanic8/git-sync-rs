use crate::error::{Result, SyncError};
use crate::sync::{RepositorySynchronizer, SyncConfig};
#[cfg(feature = "tray")]
use crate::tray::{GitSyncTray, TrayCommand, TrayState, TrayStatus};
use git2::Repository;
#[cfg(feature = "tray")]
use ksni::TrayMethods;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use tracing::{debug, error, info, warn};

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

    /// Enable system tray indicator (requires `tray` feature)
    pub enable_tray: bool,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            min_interval_ms: 1000,
            sync_on_start: true,
            dry_run: false,
            enable_tray: false,
        }
    }
}

/// Manages file system watching and automatic synchronization
pub struct WatchManager {
    repo_path: String,
    sync_config: SyncConfig,
    watch_config: WatchConfig,
    is_syncing: Arc<AtomicBool>,
}

/// Event handler for file system changes
struct FileEventHandler {
    repo_path: PathBuf,
    tx: mpsc::Sender<Event>,
}

impl FileEventHandler {
    fn new(repo_path: PathBuf, tx: mpsc::Sender<Event>) -> Self {
        Self { repo_path, tx }
    }

    fn handle_event(&self, res: std::result::Result<Event, notify::Error>) {
        let event = match res {
            Ok(event) => event,
            Err(e) => {
                error!("Watch error: {}", e);
                return;
            }
        };

        debug!("Raw file event received: {:?}", event);

        if !self.should_process_event(&event) {
            return;
        }

        debug!("Event is relevant, sending to channel");
        if let Err(e) = self.tx.blocking_send(event.clone()) {
            error!("Failed to send event to channel: {}", e);
        } else {
            debug!("Event sent successfully: {:?}", event.kind);
        }
    }

    fn should_process_event(&self, event: &Event) -> bool {
        // Ignore git directory changes
        if self.is_git_internal(event) {
            debug!("Ignoring git internal event");
            return false;
        }

        // Open repository for gitignore check
        let repo = match Repository::open(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to open repository for gitignore check: {}", e);
                return false;
            }
        };

        // Check if any path in the event should be ignored
        let should_ignore = event
            .paths
            .iter()
            .any(|path| self.should_ignore_path(&repo, path));

        if should_ignore {
            debug!("Ignoring gitignored file event");
            return false;
        }

        // Check if this is a relevant change type
        if !self.is_relevant_change(event) {
            debug!("Event not considered relevant: {:?}", event.kind);
            return false;
        }

        true
    }

    /// Check if an event is related to git internals
    fn is_git_internal(&self, event: &Event) -> bool {
        event
            .paths
            .iter()
            .any(|path| path.components().any(|c| c.as_os_str() == ".git"))
    }

    /// Check if a path should be ignored according to gitignore rules
    fn should_ignore_path(&self, repo: &Repository, file_path: &Path) -> bool {
        // Make path relative to repo root
        let relative_path = match file_path.strip_prefix(&self.repo_path) {
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
                debug!(
                    "Error checking gitignore status for {:?}: {}",
                    relative_path, e
                );
                false
            }
        }
    }

    /// Check if an event represents a relevant change
    fn is_relevant_change(&self, event: &Event) -> bool {
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
            is_syncing: Arc::new(AtomicBool::new(false)),
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
        let (tx, rx) = mpsc::channel::<Event>(100);

        // Setup file watcher
        let _watcher = self.setup_watcher(tx)?;

        info!(
            "Watching for changes (debounce: {}s)",
            self.watch_config.debounce_ms as f64 / 1000.0
        );

        // Process events
        self.process_events(rx).await
    }

    /// Setup the file system watcher
    fn setup_watcher(&self, tx: mpsc::Sender<Event>) -> Result<RecommendedWatcher> {
        let repo_path_clone = PathBuf::from(&self.repo_path);
        let handler = FileEventHandler::new(repo_path_clone, tx);

        let mut watcher =
            RecommendedWatcher::new(move |res| handler.handle_event(res), Config::default())?;

        // Watch the repository path
        watcher.watch(Path::new(&self.repo_path), RecursiveMode::Recursive)?;

        Ok(watcher)
    }

    /// Process file system events
    async fn process_events(&self, mut rx: mpsc::Receiver<Event>) -> Result<()> {
        let mut sync_state = SyncState::new(
            self.watch_config.debounce_ms,
            self.watch_config.min_interval_ms,
        );

        // Periodic interval prevents starvation under continuous events
        let tick_ms = self
            .watch_config
            .debounce_ms
            .min(self.watch_config.min_interval_ms)
            .max(50);
        let mut interval = time::interval(Duration::from_millis(tick_ms));
        interval.tick().await; // align first tick

        #[cfg(feature = "tray")]
        if self.watch_config.enable_tray {
            return self
                .process_events_with_tray(&mut rx, &mut sync_state, &mut interval)
                .await;
        }

        self.process_events_loop(&mut rx, &mut sync_state, &mut interval, false)
            .await
    }

    /// Core event loop without tray
    async fn process_events_loop(
        &self,
        rx: &mut mpsc::Receiver<Event>,
        sync_state: &mut SyncState,
        interval: &mut time::Interval,
        paused: bool,
    ) -> Result<()> {
        loop {
            tokio::select! {
                biased;
                _ = interval.tick() => {
                    if !paused {
                        self.handle_timeout(sync_state).await;
                    }
                }
                Some(event) = rx.recv() => {
                    if !paused {
                        self.handle_file_event(event, sync_state);
                    }
                }
            }
        }
    }

    /// Event loop with tray integration
    #[cfg(feature = "tray")]
    async fn process_events_with_tray(
        &self,
        rx: &mut mpsc::Receiver<Event>,
        sync_state: &mut SyncState,
        interval: &mut time::Interval,
    ) -> Result<()> {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let tray_state = TrayState::new(PathBuf::from(&self.repo_path));
        let tray = GitSyncTray::new(tray_state, cmd_tx);

        let handle = tray
            .spawn()
            .await
            .map_err(|e| SyncError::Other(format!("Failed to spawn tray: {}", e)))?;
        info!("System tray indicator started");

        let mut paused = false;

        loop {
            tokio::select! {
                biased;
                _ = interval.tick() => {
                    if !paused {
                        self.handle_timeout_with_tray(sync_state, &handle).await;
                    }
                }
                Some(event) = rx.recv() => {
                    if !paused {
                        self.handle_file_event(event, sync_state);
                    }
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        TrayCommand::SyncNow => {
                            info!("Tray: manual sync requested");
                            self.do_sync_with_tray_update(sync_state, &handle).await;
                        }
                        TrayCommand::Pause => {
                            info!("Tray: pausing sync");
                            paused = true;
                            let _ = handle.update(|t: &mut GitSyncTray| {
                                t.state.paused = true;
                                t.bump_icon_generation();
                            }).await;
                        }
                        TrayCommand::Resume => {
                            info!("Tray: resuming sync");
                            paused = false;
                            let _ = handle.update(|t: &mut GitSyncTray| {
                                t.state.paused = false;
                                t.bump_icon_generation();
                            }).await;
                        }
                        TrayCommand::Quit => {
                            info!("Tray: quit requested");
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    /// Handle timeout with tray state updates
    #[cfg(feature = "tray")]
    async fn handle_timeout_with_tray(
        &self,
        sync_state: &mut SyncState,
        handle: &ksni::Handle<GitSyncTray>,
    ) {
        if !sync_state.should_sync() {
            return;
        }

        if self.is_syncing.load(Ordering::Acquire) {
            debug!("Sync already in progress, skipping");
            return;
        }

        self.do_sync_with_tray_update(sync_state, handle).await;
    }

    /// Perform sync and update tray state accordingly
    #[cfg(feature = "tray")]
    async fn do_sync_with_tray_update(
        &self,
        sync_state: &mut SyncState,
        handle: &ksni::Handle<GitSyncTray>,
    ) {
        info!("Tray: setting status to Syncing");
        let update_result = handle
            .update(|t: &mut GitSyncTray| {
                t.state.status = TrayStatus::Syncing;
                t.bump_icon_generation();
            })
            .await;
        if update_result.is_none() {
            warn!("Tray: handle.update (Syncing) returned None - tray service may be dead");
        }

        let span = tracing::info_span!(
            "perform_sync_attempt",
            repo = %self.repo_path,
            branch = %self.sync_config.branch_name,
            remote = %self.sync_config.remote_name,
            dry_run = self.watch_config.dry_run
        );
        let _guard = span.enter();

        match self.perform_sync().await {
            Ok(()) => {
                info!("Tray: perform_sync succeeded, setting status to Idle");
                sync_state.record_sync();
                let now = std::time::Instant::now();
                let update_result = handle
                    .update(move |t: &mut GitSyncTray| {
                        t.state.status = TrayStatus::Idle;
                        t.state.last_sync = Some(now);
                        t.state.last_error = None;
                        t.bump_icon_generation();
                    })
                    .await;
                if update_result.is_none() {
                    warn!("Tray: handle.update (Idle) returned None - tray service may be dead");
                }
            }
            Err(e) => {
                let err_msg = e.to_string();
                self.log_sync_error(&e);
                info!("Tray: perform_sync failed, setting status to Error");
                let update_result = handle
                    .update(move |t: &mut GitSyncTray| {
                        t.state.status = TrayStatus::Error(err_msg.clone());
                        t.state.last_error = Some(err_msg);
                        t.bump_icon_generation();
                    })
                    .await;
                if update_result.is_none() {
                    warn!("Tray: handle.update (Error) returned None - tray service may be dead");
                }
            }
        }
    }

    /// Handle a file system event
    fn handle_file_event(&self, event: Event, sync_state: &mut SyncState) {
        debug!("Received event from channel: {:?}", event);
        debug!("Event kind: {:?}, paths: {:?}", event.kind, event.paths);

        // Use FileEventHandler's method to check relevance
        // We can't easily share this without restructuring, so for now keep it simple
        if self.is_relevant_change(&event) {
            info!("Relevant change detected, marking pending sync");
            sync_state.mark_pending();
        } else {
            debug!("Event not considered relevant: {:?}", event.kind);
        }
    }

    /// Check if an event represents a relevant change
    fn is_relevant_change(&self, event: &Event) -> bool {
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

    /// Handle timeout expiration
    async fn handle_timeout(&self, sync_state: &mut SyncState) {
        if !sync_state.should_sync() {
            return;
        }

        // Check if already syncing
        if self.is_syncing.load(Ordering::Acquire) {
            debug!("Sync already in progress, skipping");
            return;
        }

        info!("Changes detected, triggering sync");
        let span = tracing::info_span!(
            "perform_sync_attempt",
            repo = %self.repo_path,
            branch = %self.sync_config.branch_name,
            remote = %self.sync_config.remote_name,
            dry_run = self.watch_config.dry_run
        );
        let _guard = span.enter();
        match self.perform_sync().await {
            Ok(()) => {
                debug!("perform_sync succeeded");
                sync_state.record_sync();
            }
            Err(e) => {
                self.log_sync_error(&e);
            }
        }
    }

    fn log_sync_error(&self, e: &SyncError) {
        match e {
            SyncError::DetachedHead => {
                error!("Sync failed: detached HEAD. Repository must be on a branch; will retry.")
            }
            SyncError::UnsafeRepositoryState { state } => error!(
                state = %state,
                "Sync failed: repository in unsafe state; will retry"
            ),
            SyncError::ManualInterventionRequired { reason } => warn!(
                reason = %reason,
                "Sync requires manual intervention; pending will remain set"
            ),
            SyncError::NoRemoteConfigured { branch } => error!(
                branch = %branch,
                "Sync failed: no remote configured for branch"
            ),
            SyncError::NetworkError(msg) => error!(
                error = %msg,
                "Network error during sync; will retry"
            ),
            SyncError::TaskError(msg) => error!(
                error = %msg,
                "Background task error during sync; will retry"
            ),
            SyncError::GitError(err) => error!(
                code = ?err.code(),
                klass = ?err.class(),
                message = %err.message(),
                "Git error during sync; will retry"
            ),
            other => error!(error = %other, "Sync failed; will retry"),
        }
    }

    /// Perform a synchronization
    async fn perform_sync(&self) -> Result<()> {
        // Set syncing flag (lock-free)
        if self.is_syncing.swap(true, Ordering::AcqRel) {
            debug!("Sync already in progress");
            return Ok(());
        }

        // Run the sync and ensure we clear the syncing flag regardless of outcome
        let result: Result<()> = if self.watch_config.dry_run {
            info!("DRY RUN: Would perform sync now");
            Ok(())
        } else {
            // Perform sync in blocking task
            let repo_path = self.repo_path.clone();
            let sync_config = self.sync_config.clone();

            debug!("Spawning blocking sync task");
            match tokio::task::spawn_blocking(move || {
                // Create synchronizer
                let mut synchronizer =
                    RepositorySynchronizer::new_with_detected_branch(&repo_path, sync_config)?;

                // Perform sync
                synchronizer.sync(false)
            })
            .await
            {
                Ok(inner) => inner,
                Err(e) => {
                    error!("Join error waiting for sync task: {}", e);
                    Err(e.into())
                }
            }
        };

        // Clear syncing flag (finally-like)
        self.is_syncing.store(false, Ordering::Release);

        if let Err(ref err) = result {
            error!(error = %err, "perform_sync finished with error");
        } else {
            debug!("perform_sync finished successfully");
        }
        result
    }
}

/// State for managing sync timing
struct SyncState {
    last_sync: time::Instant,
    pending_sync: bool,
    min_interval: Duration,
    debounce: Duration,
    last_event: Option<time::Instant>,
}

impl SyncState {
    fn new(debounce_ms: u64, min_interval_ms: u64) -> Self {
        Self {
            last_sync: time::Instant::now(),
            pending_sync: false,
            min_interval: Duration::from_millis(min_interval_ms),
            debounce: Duration::from_millis(debounce_ms),
            last_event: None,
        }
    }

    fn mark_pending(&mut self) {
        self.pending_sync = true;
        self.last_event = Some(time::Instant::now());
    }

    fn should_sync(&self) -> bool {
        if !self.pending_sync {
            return false;
        }

        // Enforce minimum interval between syncs (throttle ensures progress)
        let since_last_sync = self.last_sync.elapsed();
        if since_last_sync < self.min_interval {
            debug!("Too soon since last sync, waiting");
            return false;
        }

        // Prefer to wait for quiet period, but do not starve: if min_interval
        // has elapsed, allow sync even if events keep arriving.
        if let Some(t) = self.last_event {
            let since_last_event = t.elapsed();
            if since_last_event < self.debounce {
                debug!("Debounce active, but proceeding due to min-interval");
            }
        }

        true
    }

    fn record_sync(&mut self) {
        self.last_sync = time::Instant::now();
        self.pending_sync = false;
        self.last_event = None;
    }
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

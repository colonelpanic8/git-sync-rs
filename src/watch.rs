use crate::error::{Result, SyncError};
use crate::sync::{RepositorySynchronizer, SyncConfig};
#[cfg(feature = "tray")]
use crate::tray::{GitSyncTray, TrayCommand, TrayState, TrayStatus};
use git2::Repository;
#[cfg(feature = "tray")]
use ksni::TrayMethods;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use tracing::{debug, error, info, warn};

#[cfg(feature = "tray")]
const TRAY_RETRY_FALLBACK_DELAY: Duration = Duration::from_secs(15);

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

    /// Custom tray icon: a freedesktop icon name or a path to an image file
    pub tray_icon: Option<String>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            min_interval_ms: 1000,
            sync_on_start: true,
            dry_run: false,
            enable_tray: false,
            tray_icon: None,
        }
    }
}

/// Manages file system watching and automatic synchronization
pub struct WatchManager {
    repo_path: String,
    sync_config: SyncConfig,
    watch_config: WatchConfig,
    is_syncing: Arc<AtomicBool>,
    last_successful_sync_unix_secs: Arc<AtomicI64>,
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
            last_successful_sync_unix_secs: Arc::new(AtomicI64::new(0)),
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
                .process_events_with_tray_resilient(&mut rx, &mut sync_state, &mut interval)
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

    /// Event loop with tray integration.
    ///
    /// This must never fail startup: the tray is best-effort. If we can't connect
    /// to the graphical session / StatusNotifierWatcher, we keep running headless
    /// and retry periodically.
    #[cfg(feature = "tray")]
    async fn process_events_with_tray_resilient(
        &self,
        rx: &mut mpsc::Receiver<Event>,
        sync_state: &mut SyncState,
        interval: &mut time::Interval,
    ) -> Result<()> {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tray_state = TrayState::new(PathBuf::from(&self.repo_path));
        let tray_icon = self.watch_config.tray_icon.clone();
        let mut tray_handle: Option<ksni::Handle<GitSyncTray>> = None;

        let mut tray_next_attempt = time::Instant::now();
        let mut tray_spawn_task: Option<
            tokio::task::JoinHandle<std::result::Result<ksni::Handle<GitSyncTray>, ksni::Error>>,
        > = None;
        let mut dbus_bus_watch = Self::setup_dbus_session_bus_watch();

        loop {
            tokio::select! {
                biased;
                _ = interval.tick() => {
                    // If a spawn attempt finished, harvest it (non-blocking: is_finished checked).
                    if let Some(task) = tray_spawn_task.as_ref() {
                        if task.is_finished() {
                            match tray_spawn_task.take().expect("checked Some above").await {
                                Ok(Ok(handle)) => {
                                    info!("System tray indicator started");
                                    tray_handle = Some(handle);
                                    tray_next_attempt = time::Instant::now();
                                    // Ensure the tray reflects current state even if it changed during spawn.
                                    self.tray_apply_state(&mut tray_handle, &tray_state).await;
                                }
                                Ok(Err(e)) => {
                                    warn!(
                                        error = %e,
                                        delay_s = TRAY_RETRY_FALLBACK_DELAY.as_secs_f64(),
                                        "Tray unavailable; will retry"
                                    );
                                    tray_next_attempt = time::Instant::now() + TRAY_RETRY_FALLBACK_DELAY;
                                }
                                Err(e) => {
                                    warn!(
                                        error = %e,
                                        delay_s = TRAY_RETRY_FALLBACK_DELAY.as_secs_f64(),
                                        "Tray spawn task failed; will retry"
                                    );
                                    tray_next_attempt = time::Instant::now() + TRAY_RETRY_FALLBACK_DELAY;
                                }
                            }
                        }
                    }

                    // If we don't have a tray yet, and no spawn is in flight, try again when due.
                    if tray_handle.is_none()
                        && tray_spawn_task.is_none()
                        && time::Instant::now() >= tray_next_attempt
                    {
                        tray_spawn_task = Some(Self::spawn_tray_task(
                            tray_state.clone(),
                            cmd_tx.clone(),
                            tray_icon.clone(),
                        ));
                    }

                    if !tray_state.paused {
                        self.handle_timeout_with_optional_tray(sync_state, &mut tray_state, &mut tray_handle).await;
                    }
                    self.refresh_tray_last_sync_from_global(&mut tray_state, &mut tray_handle)
                        .await;
                }
                Some(event) = rx.recv() => {
                    if !tray_state.paused {
                        self.handle_file_event(event, sync_state);
                    }
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        TrayCommand::SyncNow => {
                            if tray_state.paused {
                                debug!("Tray: manual sync requested while paused; ignoring");
                            } else {
                                info!("Tray: manual sync requested");
                                self.do_sync_with_optional_tray_update(sync_state, &mut tray_state, &mut tray_handle).await;
                            }
                        }
                        TrayCommand::Pause => {
                            info!("Tray: pausing sync");
                            tray_state.paused = true;
                            self.tray_apply_state(&mut tray_handle, &tray_state).await;
                        }
                        TrayCommand::Resume => {
                            info!("Tray: resuming sync");
                            tray_state.paused = false;
                            self.tray_apply_state(&mut tray_handle, &tray_state).await;
                        }
                        TrayCommand::Quit => {
                            info!("Tray: quit requested");
                            if let Some(handle) = &tray_handle {
                                // Best-effort shutdown before exiting watch mode.
                                handle.shutdown().await;
                            }
                            return Ok(());
                        }
                    }
                }
                dbus_event = async {
                    if let Some((_, rx)) = dbus_bus_watch.as_mut() {
                        rx.recv().await
                    } else {
                        None
                    }
                }, if dbus_bus_watch.is_some() => {
                    match dbus_event {
                        Some(()) => {
                            info!("Detected D-Bus session bus socket activity; retrying tray startup now");
                            tray_next_attempt = time::Instant::now();

                            if tray_handle.is_none() && tray_spawn_task.is_none() {
                                tray_spawn_task = Some(Self::spawn_tray_task(
                                    tray_state.clone(),
                                    cmd_tx.clone(),
                                    tray_icon.clone(),
                                ));
                            }
                        }
                        None => {
                            warn!("D-Bus session bus watcher channel closed; falling back to periodic retry");
                            dbus_bus_watch = None;
                        }
                    }
                }
            }
        }
    }

    #[cfg(feature = "tray")]
    fn spawn_tray_task(
        tray_state: TrayState,
        cmd_tx: tokio::sync::mpsc::UnboundedSender<TrayCommand>,
        tray_icon: Option<String>,
    ) -> tokio::task::JoinHandle<std::result::Result<ksni::Handle<GitSyncTray>, ksni::Error>> {
        tokio::spawn(async move {
            let tray = GitSyncTray::new(tray_state, cmd_tx, tray_icon);
            // Treat missing watcher/host as a soft error and keep the service alive.
            tray.assume_sni_available(true).spawn().await
        })
    }

    #[cfg(feature = "tray")]
    fn setup_dbus_session_bus_watch(
    ) -> Option<(RecommendedWatcher, tokio::sync::mpsc::UnboundedReceiver<()>)> {
        let Some(socket_path) = Self::dbus_session_bus_socket_path() else {
            debug!("DBUS_SESSION_BUS_ADDRESS not watchable (no unix:path=...); using periodic tray retry");
            return None;
        };
        Self::setup_dbus_socket_watch(socket_path)
    }

    #[cfg(feature = "tray")]
    fn setup_dbus_socket_watch(
        socket_path: PathBuf,
    ) -> Option<(RecommendedWatcher, tokio::sync::mpsc::UnboundedReceiver<()>)> {
        let Some(parent_dir) = socket_path.parent() else {
            warn!(
                path = %socket_path.display(),
                "Unable to watch D-Bus session bus socket parent directory; using periodic tray retry"
            );
            return None;
        };

        let watched_name = socket_path.file_name().map(|n| n.to_os_string());
        let socket_path_for_cb = socket_path.clone();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let mut watcher = match RecommendedWatcher::new(
            move |res: std::result::Result<Event, notify::Error>| match res {
                Ok(event) => {
                    let matches_path = event.paths.iter().any(|path| {
                        if *path == socket_path_for_cb {
                            return true;
                        }
                        match (&watched_name, path.file_name()) {
                            (Some(name), Some(file_name)) => file_name == name,
                            _ => false,
                        }
                    });
                    if matches_path {
                        let _ = tx.send(());
                    }
                }
                Err(e) => {
                    warn!(error = %e, "D-Bus session bus watcher error");
                }
            },
            Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                warn!(
                    path = %socket_path.display(),
                    error = %e,
                    "Failed to create D-Bus session bus watcher; using periodic tray retry"
                );
                return None;
            }
        };

        if let Err(e) = watcher.watch(parent_dir, RecursiveMode::NonRecursive) {
            warn!(
                path = %parent_dir.display(),
                error = %e,
                "Failed to watch D-Bus session bus directory; using periodic tray retry"
            );
            return None;
        }

        info!(
            path = %socket_path.display(),
            "Watching D-Bus session bus socket for tray reconnection triggers"
        );
        Some((watcher, rx))
    }

    #[cfg(feature = "tray")]
    fn dbus_session_bus_socket_path() -> Option<PathBuf> {
        let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()?;
        Self::parse_dbus_unix_path(&address)
    }

    #[cfg(feature = "tray")]
    fn parse_dbus_unix_path(address: &str) -> Option<PathBuf> {
        // Address list format: "transport:key=value,...;transport:key=value,..."
        for segment in address.split(';') {
            if !segment.starts_with("unix:") {
                continue;
            }

            let params = &segment["unix:".len()..];
            for param in params.split(',') {
                let Some((key, value)) = param.split_once('=') else {
                    continue;
                };
                if key == "path" && !value.is_empty() {
                    return Some(PathBuf::from(value));
                }
            }
        }

        None
    }

    #[cfg(feature = "tray")]
    async fn tray_apply_state(
        &self,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
        tray_state: &TrayState,
    ) {
        let Some(handle) = tray_handle.as_ref() else {
            return;
        };

        let state = tray_state.clone();
        let update_result = handle
            .update(move |t: &mut GitSyncTray| {
                t.state = state;
                t.bump_icon_generation();
            })
            .await;

        if update_result.is_none() {
            warn!("Tray: handle.update returned None - tray service may be dead; will attempt to respawn");
            *tray_handle = None;
        }
    }

    #[cfg(feature = "tray")]
    async fn refresh_tray_last_sync_from_global(
        &self,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
    ) {
        let latest_sync = self.latest_successful_sync_datetime();
        if tray_state.last_sync == latest_sync {
            return;
        }

        tray_state.last_sync = latest_sync;
        self.tray_apply_state(tray_handle, tray_state).await;
    }

    #[cfg(feature = "tray")]
    fn latest_successful_sync_datetime(&self) -> Option<chrono::DateTime<chrono::Local>> {
        let unix_secs = self.last_successful_sync_unix_secs.load(Ordering::Acquire);
        if unix_secs <= 0 {
            return None;
        }
        use chrono::TimeZone;
        chrono::Local.timestamp_opt(unix_secs, 0).single()
    }

    /// Handle timeout with optional tray state updates
    #[cfg(feature = "tray")]
    async fn handle_timeout_with_optional_tray(
        &self,
        sync_state: &mut SyncState,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
    ) {
        if !sync_state.should_sync() {
            return;
        }

        if self.is_syncing.load(Ordering::Acquire) {
            debug!("Sync already in progress, skipping");
            return;
        }

        self.do_sync_with_optional_tray_update(sync_state, tray_state, tray_handle)
            .await;
    }

    /// Perform sync and update tray state accordingly (if available)
    #[cfg(feature = "tray")]
    async fn do_sync_with_optional_tray_update(
        &self,
        sync_state: &mut SyncState,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
    ) {
        info!("Tray: setting status to Syncing");
        tray_state.status = TrayStatus::Syncing;
        self.tray_apply_state(tray_handle, tray_state).await;

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
                tray_state.status = TrayStatus::Idle;
                tray_state.last_error = None;
                self.refresh_tray_last_sync_from_global(tray_state, tray_handle)
                    .await;
                self.tray_apply_state(tray_handle, tray_state).await;
            }
            Err(e) => {
                let err_msg = e.to_string();
                self.log_sync_error(&e);
                info!("Tray: perform_sync failed, setting status to Error");
                tray_state.status = TrayStatus::Error(err_msg.clone());
                tray_state.last_error = Some(err_msg);
                self.tray_apply_state(tray_handle, tray_state).await;
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

        if result.is_ok() {
            self.last_successful_sync_unix_secs
                .store(chrono::Utc::now().timestamp(), Ordering::Release);
        }

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

#[cfg(all(test, feature = "tray"))]
mod tests {
    use super::WatchManager;
    use std::fs::File;
    use tempfile::tempdir;
    use tokio::time::{timeout, Duration};

    #[test]
    fn parse_dbus_unix_path_finds_path_with_extra_parameters() {
        let address =
            "unix:abstract=/tmp/dbus-XXXX,guid=abcdef;unix:path=/tmp/dbus-test-socket,guid=1234";
        let parsed = WatchManager::parse_dbus_unix_path(address);
        assert_eq!(
            parsed.as_deref(),
            Some(std::path::Path::new("/tmp/dbus-test-socket"))
        );
    }

    #[test]
    fn parse_dbus_unix_path_ignores_malformed_parts() {
        let address = "unix:guid=abc,broken,other=123,another;unix:path=/tmp/dbus-test";
        let parsed = WatchManager::parse_dbus_unix_path(address);
        assert_eq!(
            parsed.as_deref(),
            Some(std::path::Path::new("/tmp/dbus-test"))
        );
    }

    #[tokio::test]
    async fn setup_dbus_socket_watch_emits_on_socket_file_activity() {
        let dir = tempdir().expect("create tempdir");
        let socket_path = dir.path().join("bus");

        let (_watcher, mut rx) = WatchManager::setup_dbus_socket_watch(socket_path.clone())
            .expect("watcher should initialize for valid path");

        // Creating the bus socket path (or regular file in tests) should trigger a retry signal.
        File::create(&socket_path).expect("create watched file");

        let received = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for watcher event");
        assert_eq!(received, Some(()));
    }
}

mod event_filter;

use self::event_filter::EventFilter;
use crate::error::{Result, SyncError};
use crate::sync::{RepositorySynchronizer, SyncConfig};
#[cfg(feature = "tray")]
use crate::tray::{GitSyncTray, TrayCommand, TrayState, TrayStatus};
#[cfg(feature = "tray")]
use ksni::TrayMethods;
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::future::pending;
use std::path::{Path, PathBuf};
#[cfg(feature = "tray")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
#[cfg(feature = "tray")]
use tokio::sync::watch as tokio_watch;
#[cfg(feature = "tray")]
use tokio::sync::RwLock;
use tokio::time;
use tracing::{debug, error, info, warn};

#[cfg(feature = "tray")]
const TRAY_RETRY_FALLBACK_DELAY: Duration = Duration::from_secs(15);
#[cfg(feature = "tray")]
const TRAY_RETRY_SOON_DELAY: Duration = Duration::from_secs(1);

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

    /// Optional periodic sync interval in milliseconds.
    /// When set, sync attempts are triggered even without filesystem events.
    pub periodic_sync_interval_ms: Option<u64>,
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
            periodic_sync_interval_ms: None,
        }
    }
}

/// Manages file system watching and automatic synchronization
pub struct WatchManager {
    repo_path: String,
    sync_config: SyncConfig,
    watch_config: WatchConfig,
    is_syncing: Arc<AtomicBool>,
    sync_suspended: Arc<AtomicBool>,
    last_successful_sync_unix_secs: Arc<AtomicI64>,
    #[cfg(feature = "tray")]
    last_sync_error: Arc<RwLock<Option<String>>>,
    #[cfg(feature = "tray")]
    sync_state_change_tx: tokio_watch::Sender<u64>,
    #[cfg(feature = "tray")]
    sync_state_change_seq: Arc<AtomicU64>,
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
        EventFilter::should_process_event(&self.repo_path, event)
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
        #[cfg(feature = "tray")]
        let (sync_state_change_tx, _) = tokio_watch::channel(0);

        Self {
            repo_path: expanded,
            sync_config,
            watch_config,
            is_syncing: Arc::new(AtomicBool::new(false)),
            sync_suspended: Arc::new(AtomicBool::new(false)),
            last_successful_sync_unix_secs: Arc::new(AtomicI64::new(0)),
            #[cfg(feature = "tray")]
            last_sync_error: Arc::new(RwLock::new(None)),
            #[cfg(feature = "tray")]
            sync_state_change_tx,
            #[cfg(feature = "tray")]
            sync_state_change_seq: Arc::new(AtomicU64::new(0)),
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
        let mut sync_state = SyncScheduler::new(
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

        let mut periodic_interval =
            self.watch_config
                .periodic_sync_interval_ms
                .map(|interval_ms| {
                    info!(
                        "Periodic sync enabled (interval: {}s)",
                        interval_ms as f64 / 1000.0
                    );
                    time::interval(Duration::from_millis(interval_ms))
                });
        if let Some(interval) = periodic_interval.as_mut() {
            interval.tick().await; // Skip first immediate tick
        }

        #[cfg(feature = "tray")]
        if self.watch_config.enable_tray {
            return self
                .process_events_with_tray_resilient(
                    &mut rx,
                    &mut sync_state,
                    &mut interval,
                    &mut periodic_interval,
                )
                .await;
        }

        self.process_events_loop(
            &mut rx,
            &mut sync_state,
            &mut interval,
            &mut periodic_interval,
            false,
        )
        .await
    }

    /// Core event loop without tray
    async fn process_events_loop(
        &self,
        rx: &mut mpsc::Receiver<Event>,
        sync_state: &mut SyncScheduler,
        interval: &mut time::Interval,
        periodic_interval: &mut Option<time::Interval>,
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
                _ = Self::tick_optional_interval(periodic_interval) => {
                    if !paused {
                        sync_state.request_sync_now();
                        self.handle_timeout(sync_state).await;
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
        sync_state: &mut SyncScheduler,
        interval: &mut time::Interval,
        periodic_interval: &mut Option<time::Interval>,
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
        let mut sync_state_change_rx = self.sync_state_change_tx.subscribe();
        let mut last_sync_text_snapshot = tray_state.last_sync_text();

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
                                    self.reconcile_tray_state_from_global(
                                        &mut tray_state,
                                        &mut tray_handle,
                                    )
                                    .await;
                                }
                                Ok(Err(e)) => {
                                    let retry_delay = match &e {
                                        ksni::Error::WontShow => TRAY_RETRY_SOON_DELAY,
                                        ksni::Error::Watcher(fdo_err)
                                            if format!("{fdo_err:?}").contains("UnknownObject") =>
                                        {
                                            TRAY_RETRY_SOON_DELAY
                                        }
                                        _ => TRAY_RETRY_FALLBACK_DELAY,
                                    };
                                    warn!(
                                        error = %e,
                                        delay_s = retry_delay.as_secs_f64(),
                                        "Tray unavailable; will retry"
                                    );
                                    tray_next_attempt = time::Instant::now() + retry_delay;
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

                    self.handle_timeout_with_optional_tray(sync_state, &mut tray_state, &mut tray_handle).await;
                    self.reconcile_tray_state_from_global(&mut tray_state, &mut tray_handle)
                        .await;
                    self.refresh_tray_relative_time_display(
                        &mut tray_state,
                        &mut tray_handle,
                        &mut last_sync_text_snapshot,
                    )
                    .await;
                }
                Some(event) = rx.recv() => {
                    self.handle_file_event(event, sync_state);
                }
                _ = Self::tick_optional_interval(periodic_interval) => {
                    sync_state.request_sync_now();
                    self.handle_timeout_with_optional_tray(sync_state, &mut tray_state, &mut tray_handle).await;
                }
                Some(cmd) = cmd_rx.recv() => {
                    match cmd {
                        TrayCommand::SyncNow => {
                            if tray_state.paused {
                                debug!("Tray: manual sync requested while suspended; ignoring");
                            } else {
                                info!("Tray: manual sync requested");
                                self.do_sync_with_optional_tray_update(sync_state, &mut tray_state, &mut tray_handle).await;
                            }
                        }
                        TrayCommand::Suspend => {
                            info!("Tray: suspending all sync activity");
                            self.set_sync_suspended(true);
                            self.reconcile_tray_state_from_global(&mut tray_state, &mut tray_handle)
                                .await;
                        }
                        TrayCommand::Resume => {
                            info!("Tray: resuming sync activity");
                            self.set_sync_suspended(false);
                            self.reconcile_tray_state_from_global(&mut tray_state, &mut tray_handle)
                                .await;
                        }
                        TrayCommand::Quit => {
                            info!("Tray: quit requested");
                            if let Some(handle) = &tray_handle {
                                // Best-effort shutdown before exiting watch mode.
                                handle.shutdown().await;
                            }
                            return Ok(());
                        }
                        TrayCommand::Respawn { reason } => {
                            warn!(reason = %reason, "Tray: respawn requested");

                            if let Some(task) = tray_spawn_task.take() {
                                task.abort();
                            }

                            if let Some(handle) = tray_handle.take() {
                                // Best-effort shutdown; if the service already stopped this is a no-op.
                                handle.shutdown().await;
                            }

                            tray_next_attempt = time::Instant::now() + TRAY_RETRY_SOON_DELAY;
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
                sync_state_change = sync_state_change_rx.changed() => {
                    match sync_state_change {
                        Ok(()) => {
                            self.reconcile_tray_state_from_global(&mut tray_state, &mut tray_handle)
                                .await;
                            self.refresh_tray_relative_time_display(
                                &mut tray_state,
                                &mut tray_handle,
                                &mut last_sync_text_snapshot,
                            )
                            .await;
                        }
                        Err(e) => {
                            warn!(error = %e, "Tray sync-state update channel closed");
                        }
                    }
                }
            }
        }
    }

    async fn tick_optional_interval(interval: &mut Option<time::Interval>) {
        match interval {
            Some(i) => {
                i.tick().await;
            }
            None => pending::<()>().await,
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
            // Keep the service alive even if the watcher/host isn't ready yet.
            // We handle reconnection by listening for watcher state changes and, on
            // certain errors, respawning the tray service from the outer loop.
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
    async fn reconcile_tray_state_from_global(
        &self,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
    ) {
        let mut changed = false;
        let paused = self.is_sync_suspended();

        if tray_state.paused != paused {
            tray_state.paused = paused;
            changed = true;
        }

        if !tray_state.paused {
            let desired_status = self.desired_tray_status().await;
            if tray_state.status != desired_status {
                tray_state.status = desired_status.clone();
                changed = true;
            }

            let desired_last_error = match desired_status {
                TrayStatus::Error(msg) => Some(msg),
                _ => None,
            };
            if tray_state.last_error != desired_last_error {
                tray_state.last_error = desired_last_error;
                changed = true;
            }
        }

        let latest_sync = self.latest_successful_sync_datetime();
        if tray_state.last_sync != latest_sync {
            tray_state.last_sync = latest_sync;
            changed = true;
        }

        if changed {
            self.tray_apply_state(tray_handle, tray_state).await;
        }
    }

    #[cfg(feature = "tray")]
    async fn refresh_tray_relative_time_display(
        &self,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
        last_sync_text_snapshot: &mut String,
    ) {
        let current = tray_state.last_sync_text();
        if &current == last_sync_text_snapshot {
            return;
        }

        *last_sync_text_snapshot = current;
        // State payload is unchanged, but tooltip/menu text derived from `Local::now()`
        // advanced. Emitting an update keeps the displayed relative age fresh.
        self.tray_apply_state(tray_handle, tray_state).await;
    }

    #[cfg(feature = "tray")]
    async fn desired_tray_status(&self) -> TrayStatus {
        if self.is_syncing.load(Ordering::Acquire) {
            return TrayStatus::Syncing;
        }

        let last_error = self.last_sync_error.read().await.clone();
        match last_error {
            Some(msg) => TrayStatus::Error(msg),
            None => TrayStatus::Idle,
        }
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

    #[cfg(feature = "tray")]
    fn notify_sync_state_changed(&self) {
        let seq = self.sync_state_change_seq.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.sync_state_change_tx.send(seq);
    }

    /// Handle timeout with optional tray state updates
    #[cfg(feature = "tray")]
    async fn handle_timeout_with_optional_tray(
        &self,
        sync_state: &mut SyncScheduler,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
    ) {
        if self.is_sync_suspended() {
            return;
        }

        if !sync_state.should_sync_now() {
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
        sync_state: &mut SyncScheduler,
        tray_state: &mut TrayState,
        tray_handle: &mut Option<ksni::Handle<GitSyncTray>>,
    ) {
        if self.is_sync_suspended() {
            debug!("Sync is suspended, skipping tray-triggered sync");
            return;
        }

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
                sync_state.on_sync_success();
                tray_state.status = TrayStatus::Idle;
                tray_state.last_error = None;
                self.reconcile_tray_state_from_global(tray_state, tray_handle)
                    .await;
            }
            Err(e) => {
                sync_state.on_sync_failure(&e);
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
    fn handle_file_event(&self, event: Event, sync_state: &mut SyncScheduler) {
        debug!("Received event from channel: {:?}", event);
        debug!("Event kind: {:?}, paths: {:?}", event.kind, event.paths);

        if EventFilter::is_relevant_change(&event) {
            info!("Relevant change detected, marking pending sync");
            sync_state.mark_file_event();
        } else {
            debug!("Event not considered relevant: {:?}", event.kind);
        }
    }

    /// Handle timeout expiration
    async fn handle_timeout(&self, sync_state: &mut SyncScheduler) {
        if self.is_sync_suspended() {
            return;
        }

        if !sync_state.should_sync_now() {
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
                sync_state.on_sync_success();
            }
            Err(e) => {
                sync_state.on_sync_failure(&e);
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
        if self.is_sync_suspended() {
            debug!("Sync is suspended, skipping sync attempt");
            return Ok(());
        }

        // Set syncing flag (lock-free)
        if self.is_syncing.swap(true, Ordering::AcqRel) {
            debug!("Sync already in progress");
            return Ok(());
        }
        #[cfg(feature = "tray")]
        self.notify_sync_state_changed();

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
        #[cfg(feature = "tray")]
        {
            let mut last_error = self.last_sync_error.write().await;
            *last_error = result.as_ref().err().map(ToString::to_string);
        }
        #[cfg(feature = "tray")]
        self.notify_sync_state_changed();

        if let Err(ref err) = result {
            error!(error = %err, "perform_sync finished with error");
        } else {
            debug!("perform_sync finished successfully");
        }
        result
    }

    fn is_sync_suspended(&self) -> bool {
        self.sync_suspended.load(Ordering::Acquire)
    }

    #[cfg(feature = "tray")]
    fn set_sync_suspended(&self, suspended: bool) {
        self.sync_suspended.store(suspended, Ordering::Release);
        self.notify_sync_state_changed();
    }
}

/// Deadline/backoff based scheduler for watch-triggered sync attempts.
///
/// Behavior:
/// - Coalesce events via quiet debounce window.
/// - Prevent starvation with max batch latency under continuous event streams.
/// - Apply retry backoff by error class on failures.
struct SyncScheduler {
    last_sync: time::Instant,
    pending_sync: bool,
    immediate_requested: bool,
    min_interval: Duration,
    debounce: Duration,
    max_batch_latency: Duration,
    first_event: Option<time::Instant>,
    last_event: Option<time::Instant>,
    next_retry_at: Option<time::Instant>,
    retry_backoff: Duration,
}

impl SyncScheduler {
    const RETRY_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
    const RETRY_BACKOFF_MAX: Duration = Duration::from_secs(60);
    const RETRY_DELAY_MANUAL: Duration = Duration::from_secs(30);
    const RETRY_DELAY_CONFIG: Duration = Duration::from_secs(60);
    const RETRY_DELAY_STATE: Duration = Duration::from_secs(5);

    fn new(debounce_ms: u64, min_interval_ms: u64) -> Self {
        let debounce = Duration::from_millis(debounce_ms);
        let min_interval = Duration::from_millis(min_interval_ms);
        let max_batch_latency = debounce
            .saturating_mul(8)
            .max(min_interval)
            .max(Duration::from_millis(500));

        Self {
            last_sync: time::Instant::now(),
            pending_sync: false,
            immediate_requested: false,
            min_interval,
            debounce,
            max_batch_latency,
            first_event: None,
            last_event: None,
            next_retry_at: None,
            retry_backoff: Self::RETRY_BACKOFF_INITIAL,
        }
    }

    fn mark_file_event(&mut self) {
        self.mark_file_event_at(time::Instant::now());
    }

    fn mark_file_event_at(&mut self, now: time::Instant) {
        self.pending_sync = true;
        self.immediate_requested = false;
        self.first_event.get_or_insert(now);
        self.last_event = Some(now);
    }

    fn request_sync_now(&mut self) {
        self.request_sync_now_at(time::Instant::now());
    }

    fn request_sync_now_at(&mut self, now: time::Instant) {
        self.pending_sync = true;
        self.immediate_requested = true;
        self.first_event.get_or_insert(now);
        self.last_event.get_or_insert(now);
    }

    fn should_sync_now(&self) -> bool {
        self.should_sync_at(time::Instant::now())
    }

    fn should_sync_at(&self, now: time::Instant) -> bool {
        if !self.pending_sync {
            return false;
        }

        if let Some(next_retry_at) = self.next_retry_at {
            if now < next_retry_at {
                return false;
            }
        }

        if now.duration_since(self.last_sync) < self.min_interval {
            return false;
        }

        if self.immediate_requested {
            return true;
        }

        let quiet_ready = self
            .last_event
            .map(|last| now.duration_since(last) >= self.debounce)
            .unwrap_or(false);
        if quiet_ready {
            return true;
        }

        self.first_event
            .map(|first| now.duration_since(first) >= self.max_batch_latency)
            .unwrap_or(false)
    }

    fn on_sync_success(&mut self) {
        self.on_sync_success_at(time::Instant::now());
    }

    fn on_sync_success_at(&mut self, now: time::Instant) {
        self.last_sync = now;
        self.pending_sync = false;
        self.immediate_requested = false;
        self.first_event = None;
        self.last_event = None;
        self.next_retry_at = None;
        self.retry_backoff = Self::RETRY_BACKOFF_INITIAL;
    }

    fn on_sync_failure(&mut self, error: &SyncError) {
        self.on_sync_failure_at(error, time::Instant::now());
    }

    fn on_sync_failure_at(&mut self, error: &SyncError, now: time::Instant) {
        self.last_sync = now;
        self.pending_sync = true;
        self.immediate_requested = false;

        let delay = self.retry_delay_for(error);
        self.next_retry_at = Some(now + delay);
        debug!(
            delay_s = delay.as_secs_f64(),
            error = %error,
            "Sync failure scheduled with retry backoff"
        );
    }

    fn retry_delay_for(&mut self, error: &SyncError) -> Duration {
        match error {
            SyncError::ManualInterventionRequired { .. } | SyncError::HookRejected { .. } => {
                Self::RETRY_DELAY_MANUAL
            }
            SyncError::NoRemoteConfigured { .. }
            | SyncError::RemoteBranchNotFound { .. }
            | SyncError::NotARepository { .. } => Self::RETRY_DELAY_CONFIG,
            SyncError::DetachedHead | SyncError::UnsafeRepositoryState { .. } => {
                Self::RETRY_DELAY_STATE
            }
            _ => {
                let delay = self.retry_backoff;
                self.retry_backoff = self
                    .retry_backoff
                    .saturating_mul(2)
                    .min(Self::RETRY_BACKOFF_MAX);
                delay
            }
        }
    }
}

/// Run watch mode with periodic sync.
pub async fn watch_with_periodic_sync(
    repo_path: impl AsRef<Path>,
    sync_config: SyncConfig,
    mut watch_config: WatchConfig,
    sync_interval_ms: Option<u64>,
) -> Result<()> {
    watch_config.periodic_sync_interval_ms = sync_interval_ms;
    let manager = WatchManager::new(repo_path, sync_config, watch_config);
    manager.watch().await
}

#[cfg(test)]
mod scheduler_tests {
    use super::SyncScheduler;
    use crate::error::SyncError;
    use tokio::time::{Duration, Instant};

    #[test]
    fn scheduler_waits_for_quiet_period_before_syncing() {
        let mut scheduler = SyncScheduler::new(200, 100);
        let base = Instant::now();
        scheduler.last_sync = base;
        scheduler.mark_file_event_at(base);

        assert!(!scheduler.should_sync_at(base));
        assert!(!scheduler.should_sync_at(base + Duration::from_millis(120)));
        assert!(scheduler.should_sync_at(base + Duration::from_millis(220)));
    }

    #[test]
    fn scheduler_uses_max_batch_latency_to_prevent_starvation() {
        let mut scheduler = SyncScheduler::new(500, 100);
        let base = Instant::now();
        scheduler.last_sync = base;
        scheduler.mark_file_event_at(base);

        // Keep sending events faster than debounce; sync should still eventually fire.
        for i in 1..40 {
            let t = base + Duration::from_millis(100 * i);
            scheduler.mark_file_event_at(t);
            assert!(
                !scheduler.should_sync_at(t),
                "Scheduler should still wait before max-batch threshold"
            );
        }

        let ready_at = base + Duration::from_millis(4000);
        scheduler.mark_file_event_at(ready_at);
        assert!(
            scheduler.should_sync_at(ready_at),
            "Scheduler should fire at max-batch latency under continuous events"
        );
    }

    #[test]
    fn scheduler_applies_retry_backoff_and_resets_on_success() {
        let mut scheduler = SyncScheduler::new(0, 0);
        let base = Instant::now();
        scheduler.last_sync = base;
        scheduler.mark_file_event_at(base);
        assert!(scheduler.should_sync_at(base));

        scheduler.on_sync_failure_at(&SyncError::NetworkError("transient".to_string()), base);
        assert!(!scheduler.should_sync_at(base + Duration::from_millis(999)));
        assert!(scheduler.should_sync_at(base + Duration::from_millis(1000)));

        let second = base + Duration::from_millis(1000);
        scheduler.on_sync_failure_at(&SyncError::NetworkError("transient".to_string()), second);
        assert!(!scheduler.should_sync_at(second + Duration::from_secs(1)));
        assert!(scheduler.should_sync_at(second + Duration::from_secs(2)));

        scheduler.on_sync_success_at(second + Duration::from_secs(2));
        let next = second + Duration::from_secs(2);
        scheduler.mark_file_event_at(next);
        assert!(scheduler.should_sync_at(next));
    }

    #[test]
    fn scheduler_uses_longer_retry_for_manual_intervention_errors() {
        let mut scheduler = SyncScheduler::new(0, 0);
        let base = Instant::now();
        scheduler.last_sync = base;
        scheduler.mark_file_event_at(base);
        assert!(scheduler.should_sync_at(base));

        scheduler.on_sync_failure_at(
            &SyncError::ManualInterventionRequired {
                reason: "conflict".to_string(),
            },
            base,
        );
        assert!(!scheduler.should_sync_at(base + Duration::from_secs(29)));
        assert!(scheduler.should_sync_at(base + Duration::from_secs(30)));
    }

    #[test]
    fn request_sync_now_bypasses_debounce_but_respects_min_interval() {
        let mut scheduler = SyncScheduler::new(10_000, 500);
        let base = Instant::now();
        scheduler.last_sync = base;

        scheduler.request_sync_now_at(base + Duration::from_millis(100));
        assert!(!scheduler.should_sync_at(base + Duration::from_millis(499)));
        assert!(scheduler.should_sync_at(base + Duration::from_millis(500)));
    }

    #[test]
    fn request_sync_now_does_not_bypass_retry_backoff() {
        let mut scheduler = SyncScheduler::new(0, 0);
        let base = Instant::now();
        scheduler.last_sync = base;
        scheduler.mark_file_event_at(base);
        assert!(scheduler.should_sync_at(base));

        scheduler.on_sync_failure_at(&SyncError::NetworkError("transient".to_string()), base);
        scheduler.request_sync_now_at(base + Duration::from_millis(100));
        assert!(!scheduler.should_sync_at(base + Duration::from_millis(999)));
        assert!(scheduler.should_sync_at(base + Duration::from_millis(1000)));
    }
}

#[cfg(all(test, feature = "tray"))]
mod tests {
    use super::{WatchConfig, WatchManager};
    use crate::sync::SyncConfig;
    use std::fs::File;
    use std::sync::atomic::Ordering;
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

    #[tokio::test]
    async fn perform_sync_is_noop_when_suspended() {
        let manager = WatchManager::new(
            "/tmp/not-a-repo",
            SyncConfig::default(),
            WatchConfig::default(),
        );
        manager.set_sync_suspended(true);

        manager
            .perform_sync()
            .await
            .expect("suspended sync should be a no-op");

        assert_eq!(
            manager
                .last_successful_sync_unix_secs
                .load(Ordering::Acquire),
            0
        );
        assert!(!manager.is_syncing.load(Ordering::Acquire));
    }
}

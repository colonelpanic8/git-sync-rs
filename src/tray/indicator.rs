use ksni::menu::*;
use ksni::MenuItem;
use std::path::Path;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{info, warn};

use super::state::{AggregateTrayState, TrayCommand, TrayState, TrayStatus};

const ICON_32_PNG: &[u8] = include_bytes!("../../assets/git-icon-32.png");
const ICON_48_PNG: &[u8] = include_bytes!("../../assets/git-icon-48.png");
const ICON_64_PNG: &[u8] = include_bytes!("../../assets/git-icon-64.png");

const OVERLAY_SYNC_32: &[u8] = include_bytes!("../../assets/overlay-sync-32.png");
const OVERLAY_SYNC_48: &[u8] = include_bytes!("../../assets/overlay-sync-48.png");
const OVERLAY_SYNC_64: &[u8] = include_bytes!("../../assets/overlay-sync-64.png");

const OVERLAY_ERROR_32: &[u8] = include_bytes!("../../assets/overlay-error-32.png");
const OVERLAY_ERROR_48: &[u8] = include_bytes!("../../assets/overlay-error-48.png");
const OVERLAY_ERROR_64: &[u8] = include_bytes!("../../assets/overlay-error-64.png");

const OVERLAY_PAUSE_32: &[u8] = include_bytes!("../../assets/overlay-pause-32.png");
const OVERLAY_PAUSE_48: &[u8] = include_bytes!("../../assets/overlay-pause-48.png");
const OVERLAY_PAUSE_64: &[u8] = include_bytes!("../../assets/overlay-pause-64.png");

fn png_to_argb32(png_data: &[u8]) -> ksni::Icon {
    let img = image::load_from_memory_with_format(png_data, image::ImageFormat::Png)
        .expect("embedded PNG is valid")
        .into_rgba8();
    let width = img.width() as i32;
    let height = img.height() as i32;
    // Convert RGBA → ARGB (network byte order for StatusNotifierItem)
    let data: Vec<u8> = img
        .pixels()
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect();
    ksni::Icon {
        width,
        height,
        data,
    }
}

fn load_icon_set(png_32: &[u8], png_48: &[u8], png_64: &[u8]) -> Vec<ksni::Icon> {
    vec![
        png_to_argb32(png_32),
        png_to_argb32(png_48),
        png_to_argb32(png_64),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Sync,
    Error,
    Pause,
}

/// Pre-decoded overlay icon sets for each status.
struct OverlayIcons {
    sync: Vec<ksni::Icon>,
    error: Vec<ksni::Icon>,
    pause: Vec<ksni::Icon>,
}

/// How the base icon is resolved.
#[derive(Debug, Clone)]
enum IconSource {
    /// Use the bundled git icon pixmaps (default).
    Bundled,
    /// Use a freedesktop icon name resolved from the system icon theme.
    Name(String),
    /// Use a file path: `icon_theme_path` is set to the parent directory
    /// and `icon_name` to the file stem, letting the SNI host resolve it.
    Path {
        icon_theme_path: String,
        icon_name: String,
    },
}

#[derive(Debug)]
pub struct GitSyncTray {
    pub state: AggregateTrayState,
    pub cmd_tx: UnboundedSender<TrayCommand>,
    /// How the base icon is provided to the tray.
    icon_source: IconSource,
    /// Decoded main icon pixmaps (only used when icon_source is Bundled).
    icons: Vec<ksni::Icon>,
    /// Decoded overlay icon pixmaps per status (computed once at startup).
    overlays: OverlayIcons,
    /// Counter incremented whenever the overlay changes to ensure ksni
    /// detects the update even if a D-Bus property query races with the
    /// update signal processing.
    icon_generation: u64,
    /// Overlay the current `icon_generation` was issued for.
    generation_overlay: Overlay,
}

// OverlayIcons contains Vec<ksni::Icon> which is not Debug, so derive won't work.
impl std::fmt::Debug for OverlayIcons {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayIcons").finish_non_exhaustive()
    }
}

impl GitSyncTray {
    pub fn new(
        state: TrayState,
        cmd_tx: UnboundedSender<TrayCommand>,
        custom_icon: Option<String>,
    ) -> Self {
        Self::new_aggregate(AggregateTrayState::single(state), cmd_tx, custom_icon)
    }

    pub fn new_aggregate(
        state: AggregateTrayState,
        cmd_tx: UnboundedSender<TrayCommand>,
        custom_icon: Option<String>,
    ) -> Self {
        let icon_source = match custom_icon {
            Some(value) => {
                let path = Path::new(&value);
                if path.is_absolute() || value.contains('/') || value.contains('.') {
                    let parent = path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| value.clone());
                    IconSource::Path {
                        icon_theme_path: parent,
                        icon_name: stem,
                    }
                } else {
                    IconSource::Name(value)
                }
            }
            None => IconSource::Bundled,
        };
        let icons = match &icon_source {
            IconSource::Bundled => load_icon_set(ICON_32_PNG, ICON_48_PNG, ICON_64_PNG),
            _ => Vec::new(),
        };

        let generation_overlay = Self::overlay_for(&state);
        Self {
            state,
            cmd_tx,
            icon_source,
            icons,
            overlays: OverlayIcons {
                sync: load_icon_set(OVERLAY_SYNC_32, OVERLAY_SYNC_48, OVERLAY_SYNC_64),
                error: load_icon_set(OVERLAY_ERROR_32, OVERLAY_ERROR_48, OVERLAY_ERROR_64),
                pause: load_icon_set(OVERLAY_PAUSE_32, OVERLAY_PAUSE_48, OVERLAY_PAUSE_64),
            },
            icon_generation: 0,
            generation_overlay,
        }
    }

    /// Bump the icon generation counter if the overlay changed, forcing ksni
    /// to emit NewOverlayIcon even if a D-Bus property query race would
    /// otherwise cause the change to go undetected. Bumping on every state
    /// change would make hosts reload the icon on each menu text refresh.
    pub fn bump_icon_generation(&mut self) {
        let overlay = Self::overlay_for(&self.state);
        if overlay == self.generation_overlay {
            return;
        }
        self.generation_overlay = overlay;
        self.icon_generation = self.icon_generation.wrapping_add(1);
    }

    fn overlay_for(state: &AggregateTrayState) -> Overlay {
        match state.aggregate_status() {
            TrayStatus::Error(_) => Overlay::Error,
            TrayStatus::Syncing => Overlay::Sync,
            TrayStatus::Starting | TrayStatus::Idle if state.any_paused() => Overlay::Pause,
            TrayStatus::Starting | TrayStatus::Idle => Overlay::None,
        }
    }

    fn overlay_pixmaps_for_status(&self) -> Vec<ksni::Icon> {
        let base = match Self::overlay_for(&self.state) {
            Overlay::Error => &self.overlays.error,
            Overlay::Sync => &self.overlays.sync,
            Overlay::Pause => &self.overlays.pause,
            Overlay::None => return self.generation_only_pixmap(),
        };
        let mut icons = base.clone();
        // Append a tiny unique pixmap so ksni's property-change detector
        // always sees a hash difference (race condition workaround).
        icons.push(self.generation_pixel());
        icons
    }

    /// 1×1 transparent pixel whose data varies with `icon_generation`.
    fn generation_pixel(&self) -> ksni::Icon {
        let gen = self.icon_generation;
        let r = (gen & 0xFF) as u8;
        let g = ((gen >> 8) & 0xFF) as u8;
        let b = ((gen >> 16) & 0xFF) as u8;
        ksni::Icon {
            width: 1,
            height: 1,
            data: vec![0, r, g, b], // ARGB: fully transparent
        }
    }

    /// For idle state: no real overlay, just the generation pixel.
    fn generation_only_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.generation_pixel()]
    }
}

impl ksni::Tray for GitSyncTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "git-sync-rs".into()
    }

    fn icon_name(&self) -> String {
        match &self.icon_source {
            IconSource::Bundled => String::new(),
            IconSource::Name(name) => name.clone(),
            IconSource::Path { icon_name, .. } => icon_name.clone(),
        }
    }

    fn icon_theme_path(&self) -> String {
        match &self.icon_source {
            IconSource::Path {
                icon_theme_path, ..
            } => icon_theme_path.clone(),
            _ => String::new(),
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn overlay_icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.overlay_pixmaps_for_status()
    }

    fn title(&self) -> String {
        "git-sync-rs".into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "git-sync-rs".into(),
            description: self.state.status_summary(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = vec![
            StandardItem {
                label: "git-sync-rs".into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: self.state.status_summary(),
                enabled: false,
                ..Default::default()
            }
            .into(),
        ];

        for repo in self.state.repositories.values() {
            let sync_path = repo.repo_path.clone();
            let pause_path = repo.repo_path.clone();
            let mut submenu = vec![
                StandardItem {
                    label: format!("Status: {}", repo.status_text()),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: repo.last_sync_text(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            ];
            if let Some(error) = &repo.last_error {
                submenu.push(
                    StandardItem {
                        label: format!("Error: {error}"),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            }
            submenu.push(MenuItem::Separator);
            submenu.push(
                StandardItem {
                    label: "Sync Now".into(),
                    icon_name: "view-refresh".into(),
                    enabled: !repo.paused,
                    activate: Box::new(move |this: &mut Self| {
                        let _ = this
                            .cmd_tx
                            .send(TrayCommand::SyncRepository(sync_path.clone()));
                    }),
                    ..Default::default()
                }
                .into(),
            );
            let paused = repo.paused;
            submenu.push(
                StandardItem {
                    label: if paused { "Resume" } else { "Suspend" }.into(),
                    icon_name: if paused {
                        "media-playback-start"
                    } else {
                        "media-playback-pause"
                    }
                    .into(),
                    activate: Box::new(move |this: &mut Self| {
                        let command = if paused {
                            TrayCommand::ResumeRepository(pause_path.clone())
                        } else {
                            TrayCommand::SuspendRepository(pause_path.clone())
                        };
                        let _ = this.cmd_tx.send(command);
                    }),
                    ..Default::default()
                }
                .into(),
            );
            items.push(
                SubMenu {
                    label: format!("{} — {}", repo.repo_name(), repo.status_text()),
                    icon_name: match repo.status {
                        TrayStatus::Error(_) => "dialog-error",
                        TrayStatus::Syncing => "view-refresh",
                        TrayStatus::Starting => "content-loading",
                        TrayStatus::Idle if repo.paused => "media-playback-pause",
                        TrayStatus::Idle => "emblem-default",
                    }
                    .into(),
                    submenu,
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Sync All".into(),
                icon_name: "view-refresh".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd_tx.send(TrayCommand::SyncAll);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Suspend All".into(),
                icon_name: "media-playback-pause".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd_tx.send(TrayCommand::SuspendAll);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "Resume All".into(),
                icon_name: "media-playback-start".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd_tx.send(TrayCommand::ResumeAll);
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd_tx.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );

        items
    }

    fn watcher_online(&self) {
        info!("Tray: StatusNotifierWatcher is online");
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        warn!(reason = ?reason, "Tray: StatusNotifierWatcher is offline; tray may not be visible yet");
        // Keep running if the watcher is simply absent. In that case ksni will
        // wait for `NameOwnerChanged` and (re)register when the watcher appears.
        if let ksni::OfflineReason::Error(ksni::Error::Watcher(fdo_err)) = &reason {
            let fdo_dbg = format!("{fdo_err:?}");
            if fdo_dbg.contains("ServiceUnknown") {
                return true;
            }

            // If we encountered `UnknownObject` while (re)registering with the watcher,
            // restart the tray service. This recovers from watcher-restart races where the
            // bus name exists but `/StatusNotifierWatcher` is not exported yet and no
            // further `NameOwnerChanged` will be emitted to trigger another retry.
            if fdo_dbg.contains("UnknownObject") {
                let _ = self.cmd_tx.send(TrayCommand::Respawn {
                    reason: format!("{reason:?}"),
                });
                return false;
            }

            return true;
        }

        // For D-Bus errors, restart the tray service so the outer loop can attempt
        // a fresh connection.
        if matches!(reason, ksni::OfflineReason::Error(ksni::Error::Dbus(_))) {
            let _ = self.cmd_tx.send(TrayCommand::Respawn {
                reason: format!("{reason:?}"),
            });
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::GitSyncTray;
    use crate::tray::{TrayState, TrayStatus};
    use std::path::PathBuf;

    #[test]
    fn icon_generation_only_changes_with_overlay() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = TrayState::new(PathBuf::from("/tmp/repo"));
        state.status = TrayStatus::Idle;
        let mut tray = GitSyncTray::new(state.clone(), tx, None);

        state.last_sync = Some(chrono::Local::now());
        tray.state.update_repository(state.clone());
        tray.bump_icon_generation();
        assert_eq!(tray.icon_generation, 0);

        state.status = TrayStatus::Syncing;
        tray.state.update_repository(state);
        tray.bump_icon_generation();
        assert_eq!(tray.icon_generation, 1);
    }
}

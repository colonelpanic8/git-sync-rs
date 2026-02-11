use ksni::menu::*;
use ksni::MenuItem;
use tokio::sync::mpsc::UnboundedSender;

use super::state::{TrayCommand, TrayState, TrayStatus};

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

/// Pre-decoded overlay icon sets for each status.
struct OverlayIcons {
    sync: Vec<ksni::Icon>,
    error: Vec<ksni::Icon>,
    pause: Vec<ksni::Icon>,
}

#[derive(Debug)]
pub struct GitSyncTray {
    pub state: TrayState,
    pub cmd_tx: UnboundedSender<TrayCommand>,
    /// Decoded main icon pixmaps (computed once at startup).
    icons: Vec<ksni::Icon>,
    /// Decoded overlay icon pixmaps per status (computed once at startup).
    overlays: OverlayIcons,
    /// Counter incremented on each state change to ensure ksni detects
    /// overlay icon updates even if D-Bus property queries race with the
    /// update signal processing.
    icon_generation: u64,
}

// OverlayIcons contains Vec<ksni::Icon> which is not Debug, so derive won't work.
impl std::fmt::Debug for OverlayIcons {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayIcons").finish_non_exhaustive()
    }
}

impl GitSyncTray {
    pub fn new(state: TrayState, cmd_tx: UnboundedSender<TrayCommand>) -> Self {
        Self {
            state,
            cmd_tx,
            icons: load_icon_set(ICON_32_PNG, ICON_48_PNG, ICON_64_PNG),
            overlays: OverlayIcons {
                sync: load_icon_set(OVERLAY_SYNC_32, OVERLAY_SYNC_48, OVERLAY_SYNC_64),
                error: load_icon_set(OVERLAY_ERROR_32, OVERLAY_ERROR_48, OVERLAY_ERROR_64),
                pause: load_icon_set(OVERLAY_PAUSE_32, OVERLAY_PAUSE_48, OVERLAY_PAUSE_64),
            },
            icon_generation: 0,
        }
    }

    /// Bump the icon generation counter to force ksni to emit a
    /// NewOverlayIcon signal on the next update, even if a D-Bus property
    /// query race would otherwise cause the change to go undetected.
    pub fn bump_icon_generation(&mut self) {
        self.icon_generation = self.icon_generation.wrapping_add(1);
    }

    fn overlay_pixmaps_for_status(&self) -> Vec<ksni::Icon> {
        let base = if self.state.paused {
            &self.overlays.pause
        } else {
            match &self.state.status {
                TrayStatus::Idle => return self.generation_only_pixmap(),
                TrayStatus::Syncing => &self.overlays.sync,
                TrayStatus::Error(_) => &self.overlays.error,
            }
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

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn overlay_icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.overlay_pixmaps_for_status()
    }

    fn title(&self) -> String {
        format!("git-sync-rs - {}", self.state.repo_name())
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: format!("git-sync-rs - {}", self.state.repo_name()),
            description: format!(
                "{}\n{}",
                self.state.status_text(),
                self.state.last_sync_text()
            ),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = vec![
            StandardItem {
                label: format!("git-sync-rs - {}", self.state.repo_name()),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: format!("Status: {}", self.state.status_text()),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: self.state.last_sync_text(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Sync Now".into(),
                icon_name: "view-refresh".into(),
                enabled: !self.state.paused,
                activate: Box::new(|this: &mut Self| {
                    let _ = this.cmd_tx.send(TrayCommand::SyncNow);
                }),
                ..Default::default()
            }
            .into(),
        ];

        if self.state.paused {
            items.push(
                StandardItem {
                    label: "Resume".into(),
                    icon_name: "media-playback-start".into(),
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.cmd_tx.send(TrayCommand::Resume);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        } else {
            items.push(
                StandardItem {
                    label: "Pause".into(),
                    icon_name: "media-playback-pause".into(),
                    activate: Box::new(|this: &mut Self| {
                        let _ = this.cmd_tx.send(TrayCommand::Pause);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

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
}

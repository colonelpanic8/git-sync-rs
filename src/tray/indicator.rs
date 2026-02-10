use ksni::menu::*;
use ksni::MenuItem;
use tokio::sync::mpsc::UnboundedSender;

use super::state::{TrayCommand, TrayState, TrayStatus};

#[derive(Debug)]
pub struct GitSyncTray {
    pub state: TrayState,
    pub cmd_tx: UnboundedSender<TrayCommand>,
    /// Counter incremented on each state change to ensure ksni detects
    /// icon updates even if D-Bus property queries race with the update
    /// signal processing. Used in `icon_pixmap()` to produce a unique hash.
    icon_generation: u64,
}

impl GitSyncTray {
    pub fn new(state: TrayState, cmd_tx: UnboundedSender<TrayCommand>) -> Self {
        Self {
            state,
            cmd_tx,
            icon_generation: 0,
        }
    }

    /// Bump the icon generation counter to force ksni to emit a NewIcon signal
    /// on the next update, even if a D-Bus property query race would otherwise
    /// cause the icon_name change to go undetected.
    pub fn bump_icon_generation(&mut self) {
        self.icon_generation = self.icon_generation.wrapping_add(1);
    }

    fn icon_for_status(&self) -> &str {
        if self.state.paused {
            "media-playback-pause"
        } else {
            match &self.state.status {
                TrayStatus::Idle => "folder-sync",
                TrayStatus::Syncing => "view-refresh",
                TrayStatus::Error(_) => "dialog-error",
            }
        }
    }
}

impl ksni::Tray for GitSyncTray {
    fn id(&self) -> String {
        "git-sync-rs".into()
    }

    fn icon_name(&self) -> String {
        self.icon_for_status().into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // Return a unique 1x1 transparent pixmap whose data varies with
        // icon_generation.  Tray hosts prefer icon_name over icon_pixmap,
        // so this pixel is never displayed.  Its purpose is to ensure
        // ksni's property-change detector sees a hash difference and emits
        // a NewIcon D-Bus signal, even when a race condition causes the
        // icon_name change to go undetected.
        let gen = self.icon_generation;
        let r = (gen & 0xFF) as u8;
        let g = ((gen >> 8) & 0xFF) as u8;
        let b = ((gen >> 16) & 0xFF) as u8;
        vec![ksni::Icon {
            width: 1,
            height: 1,
            data: vec![0, r, g, b], // ARGB: fully transparent
        }]
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

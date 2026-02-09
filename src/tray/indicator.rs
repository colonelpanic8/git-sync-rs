use ksni::menu::*;
use ksni::MenuItem;
use tokio::sync::mpsc::UnboundedSender;

use super::state::{TrayCommand, TrayState, TrayStatus};

#[derive(Debug)]
pub struct GitSyncTray {
    pub state: TrayState,
    pub cmd_tx: UnboundedSender<TrayCommand>,
}

impl GitSyncTray {
    pub fn new(state: TrayState, cmd_tx: UnboundedSender<TrayCommand>) -> Self {
        Self { state, cmd_tx }
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

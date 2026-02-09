use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct TrayState {
    pub repo_path: PathBuf,
    pub status: TrayStatus,
    pub last_sync: Option<Instant>,
    pub last_error: Option<String>,
    pub paused: bool,
}

#[derive(Debug, Clone)]
pub enum TrayStatus {
    Idle,
    Syncing,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum TrayCommand {
    SyncNow,
    Pause,
    Resume,
    Quit,
}

impl TrayState {
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            repo_path,
            status: TrayStatus::Idle,
            last_sync: None,
            last_error: None,
            paused: false,
        }
    }

    pub fn status_text(&self) -> String {
        if self.paused {
            return "Paused".to_string();
        }
        match &self.status {
            TrayStatus::Idle => "Idle".to_string(),
            TrayStatus::Syncing => "Syncing...".to_string(),
            TrayStatus::Error(msg) => format!("Error: {}", msg),
        }
    }

    pub fn last_sync_text(&self) -> String {
        match self.last_sync {
            Some(t) => {
                let elapsed = t.elapsed();
                let secs = elapsed.as_secs();
                if secs < 60 {
                    format!("Last sync: {}s ago", secs)
                } else if secs < 3600 {
                    format!("Last sync: {}m ago", secs / 60)
                } else {
                    format!("Last sync: {}h ago", secs / 3600)
                }
            }
            None => "Last sync: never".to_string(),
        }
    }

    pub fn repo_name(&self) -> String {
        self.repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.repo_path.to_string_lossy().to_string())
    }
}

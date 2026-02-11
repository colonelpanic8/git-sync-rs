use chrono::{DateTime, Local};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct TrayState {
    pub repo_path: PathBuf,
    pub status: TrayStatus,
    pub last_sync: Option<DateTime<Local>>,
    pub last_error: Option<String>,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
            TrayStatus::Error(msg) => format!("Error: {msg}"),
        }
    }

    pub fn last_sync_text(&self) -> String {
        match &self.last_sync {
            Some(t) => {
                let literal = t.format("%Y-%m-%d %H:%M:%S %Z");
                let relative = Self::relative_time_text(t);
                format!("Last sync: {literal} ({relative})")
            }
            None => "Last sync: never".to_string(),
        }
    }

    fn relative_time_text(sync_time: &DateTime<Local>) -> String {
        let elapsed_secs = Local::now().signed_duration_since(*sync_time).num_seconds();

        if elapsed_secs < 0 {
            return "in the future".to_string();
        }
        if elapsed_secs < 60 {
            return format!("{elapsed_secs}s ago");
        }
        if elapsed_secs < 3600 {
            return format!("{}m ago", elapsed_secs / 60);
        }
        if elapsed_secs < 86_400 {
            return format!("{}h ago", elapsed_secs / 3600);
        }
        format!("{}d ago", elapsed_secs / 86_400)
    }

    pub fn repo_name(&self) -> String {
        self.repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.repo_path.to_string_lossy().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::TrayState;
    use chrono::Local;
    use std::path::PathBuf;

    #[test]
    fn last_sync_text_includes_literal_and_relative_time() {
        let mut state = TrayState::new(PathBuf::from("/tmp/repo"));
        state.last_sync = Some(Local::now() - chrono::Duration::minutes(5));

        let text = state.last_sync_text();
        assert!(text.starts_with("Last sync: "));
        assert!(text.contains(" ("));
        assert!(text.ends_with("ago)"));
    }

    #[test]
    fn relative_time_text_uses_expected_units() {
        let now = Local::now();

        assert!(
            TrayState::relative_time_text(&(now - chrono::Duration::seconds(30)))
                .ends_with("s ago")
        );
        assert!(
            TrayState::relative_time_text(&(now - chrono::Duration::minutes(2))).ends_with("m ago")
        );
        assert!(
            TrayState::relative_time_text(&(now - chrono::Duration::hours(3))).ends_with("h ago")
        );
        assert!(
            TrayState::relative_time_text(&(now - chrono::Duration::days(4))).ends_with("d ago")
        );
    }
}

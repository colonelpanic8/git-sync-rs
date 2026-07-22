use chrono::{DateTime, Local};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayState {
    pub repo_path: PathBuf,
    pub display_name: Option<String>,
    pub status: TrayStatus,
    pub last_sync: Option<DateTime<Local>>,
    pub last_error: Option<String>,
    pub paused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayStatus {
    Starting,
    Idle,
    Syncing,
    Error(String),
}

#[derive(Debug, Clone)]
pub enum TrayCommand {
    SyncNow,
    Suspend,
    Resume,
    SyncAll,
    SuspendAll,
    ResumeAll,
    SyncRepository(PathBuf),
    SuspendRepository(PathBuf),
    ResumeRepository(PathBuf),
    Quit,
    /// Internal command: request the tray service be restarted.
    ///
    /// Used to recover from transient SNI watcher restart races where ksni's
    /// re-register attempt fails (e.g. `UnknownObject`) and no further
    /// `NameOwnerChanged` events will be emitted to trigger another retry.
    Respawn {
        reason: String,
    },
}

impl TrayState {
    pub fn new(repo_path: PathBuf) -> Self {
        Self {
            repo_path,
            display_name: None,
            status: TrayStatus::Starting,
            last_sync: None,
            last_error: None,
            paused: false,
        }
    }

    pub fn status_text(&self) -> String {
        if self.paused {
            return "Suspended".to_string();
        }
        match &self.status {
            TrayStatus::Starting => "Starting...".to_string(),
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
        if let Some(name) = &self.display_name {
            return name.clone();
        }
        self.repo_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.repo_path.to_string_lossy().to_string())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AggregateTrayState {
    pub repositories: BTreeMap<PathBuf, TrayState>,
}

impl AggregateTrayState {
    pub fn single(state: TrayState) -> Self {
        let mut repositories = BTreeMap::new();
        repositories.insert(state.repo_path.clone(), state);
        Self { repositories }
    }

    pub fn update_repository(&mut self, state: TrayState) {
        self.repositories.insert(state.repo_path.clone(), state);
    }

    pub fn aggregate_status(&self) -> TrayStatus {
        if let Some(error) = self.repositories.values().find_map(|state| {
            if let TrayStatus::Error(message) = &state.status {
                Some(message.clone())
            } else {
                None
            }
        }) {
            return TrayStatus::Error(error);
        }
        if self
            .repositories
            .values()
            .any(|state| state.status == TrayStatus::Syncing)
        {
            return TrayStatus::Syncing;
        }
        if self
            .repositories
            .values()
            .any(|state| state.status == TrayStatus::Starting)
        {
            return TrayStatus::Starting;
        }
        TrayStatus::Idle
    }

    pub fn any_paused(&self) -> bool {
        self.repositories.values().any(|state| state.paused)
    }

    pub fn status_summary(&self) -> String {
        let mut starting = 0;
        let mut idle = 0;
        let mut syncing = 0;
        let mut suspended = 0;
        let mut errors = 0;
        for state in self.repositories.values() {
            if state.paused {
                suspended += 1;
            }
            match state.status {
                TrayStatus::Starting if !state.paused => starting += 1,
                TrayStatus::Idle if !state.paused => idle += 1,
                TrayStatus::Syncing => syncing += 1,
                TrayStatus::Error(_) => errors += 1,
                TrayStatus::Starting | TrayStatus::Idle => {}
            }
        }
        format!(
            "{} repositories: {idle} idle, {syncing} syncing, {starting} starting, {suspended} suspended, {errors} error(s)",
            self.repositories.len()
        )
    }

    pub fn render_signature(&self) -> String {
        self.repositories
            .values()
            .map(|state| {
                format!(
                    "{}\u{1f}{}\u{1f}{}\u{1f}{:?}",
                    state.repo_name(),
                    state.status_text(),
                    state.last_sync_text(),
                    state.last_error
                )
            })
            .collect::<Vec<_>>()
            .join("\u{1e}")
    }
}

#[cfg(test)]
mod tests {
    use super::{AggregateTrayState, TrayState, TrayStatus};
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

    #[test]
    fn aggregate_status_prioritizes_errors_then_syncing() {
        let mut state = AggregateTrayState::default();
        let mut idle = TrayState::new(PathBuf::from("/tmp/idle"));
        idle.status = TrayStatus::Idle;
        state.update_repository(idle);
        let mut syncing = TrayState::new(PathBuf::from("/tmp/syncing"));
        syncing.status = TrayStatus::Syncing;
        state.update_repository(syncing);
        assert_eq!(state.aggregate_status(), TrayStatus::Syncing);

        let mut error = TrayState::new(PathBuf::from("/tmp/error"));
        error.status = TrayStatus::Error("boom".into());
        state.update_repository(error);
        assert_eq!(state.aggregate_status(), TrayStatus::Error("boom".into()));
    }
}

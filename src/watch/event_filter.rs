use git2::Repository;
use notify::{Event, EventKind};
use std::path::Path;
use tracing::debug;

pub(super) struct EventFilter;

impl EventFilter {
    pub(super) fn should_process_event(repo_path: &Path, event: &Event) -> bool {
        if Self::is_git_internal(event) {
            debug!("Ignoring git internal event");
            return false;
        }

        let repo = match Repository::open(repo_path) {
            Ok(r) => r,
            Err(e) => {
                debug!("Failed to open repository for gitignore check: {}", e);
                return false;
            }
        };

        let should_ignore = event
            .paths
            .iter()
            .any(|path| Self::should_ignore_path(&repo, repo_path, path));

        if should_ignore {
            debug!("Ignoring gitignored file event");
            return false;
        }

        if !Self::is_relevant_change(event) {
            debug!("Event not considered relevant: {:?}", event.kind);
            return false;
        }

        true
    }

    pub(super) fn is_relevant_change(event: &Event) -> bool {
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

    fn is_git_internal(event: &Event) -> bool {
        event
            .paths
            .iter()
            .any(|path| path.components().any(|c| c.as_os_str() == ".git"))
    }

    fn should_ignore_path(repo: &Repository, repo_path: &Path, file_path: &Path) -> bool {
        let relative_path = match file_path.strip_prefix(repo_path) {
            Ok(p) => p,
            Err(_) => {
                debug!("Path {:?} is outside repo, ignoring", file_path);
                return true;
            }
        };

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
}

use crate::error::{Result, SyncError};
use std::path::Path;
use std::process::{Command, Output};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Created,
    NoChanges,
}

pub trait GitTransport: Send + Sync {
    fn fetch_branch(&self, repo_path: &Path, remote: &str, branch: &str) -> Result<()>;
    fn push_refspec(&self, repo_path: &Path, remote: &str, refspec: &str) -> Result<()>;
    fn push_branch_upstream(&self, repo_path: &Path, remote: &str, branch: &str) -> Result<()>;
    fn commit(&self, repo_path: &Path, message: &str, skip_hooks: bool) -> Result<CommitOutcome>;
}

#[derive(Debug, Default)]
pub struct CommandGitTransport;

impl CommandGitTransport {
    fn run_commit_command(
        &self,
        repo_path: &Path,
        message: &str,
        skip_hooks: bool,
        identity: Option<(&str, &str)>,
    ) -> std::io::Result<Output> {
        let mut command = Command::new("git");
        command.arg("commit");
        if skip_hooks {
            command.arg("--no-verify");
        }
        command.arg("-m").arg(message).current_dir(repo_path);

        if let Some((name, email)) = identity {
            command
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_NAME", name)
                .env("GIT_COMMITTER_EMAIL", email);
        }

        command.output()
    }

    fn is_missing_identity_error(output: &str) -> bool {
        let lower = output.to_lowercase();
        lower.contains("author identity unknown")
            || lower.contains("committer identity unknown")
            || lower.contains("please tell me who you are")
            || lower.contains("unable to auto-detect email address")
            || lower.contains("empty ident name")
            || lower.contains("empty ident email")
    }

    fn sanitize_email_component(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for ch in value.chars() {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
                out.push(ch.to_ascii_lowercase());
            } else {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }

    fn fallback_commit_identity() -> (String, String) {
        let user = std::env::var("USER")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "git-sync-rs".to_string());
        let hostname = hostname::get()
            .ok()
            .map(|h| h.to_string_lossy().to_string())
            .filter(|h| !h.trim().is_empty())
            .unwrap_or_else(|| "localhost".to_string());

        let local = Self::sanitize_email_component(&user);
        let domain = Self::sanitize_email_component(&hostname);
        let local = if local.is_empty() {
            "git-sync-rs".to_string()
        } else {
            local
        };
        let domain = if domain.is_empty() {
            "localhost".to_string()
        } else {
            domain
        };

        ("git-sync-rs".to_string(), format!("{local}@{domain}"))
    }

    fn parse_commit_output(output: &Output) -> std::result::Result<CommitOutcome, String> {
        if output.status.success() {
            return Ok(CommitOutcome::Created);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{stderr}\n{stdout}");
        let lower = combined.to_lowercase();

        if lower.contains("nothing to commit")
            || lower.contains("nothing added to commit")
            || lower.contains("no changes added to commit")
        {
            return Ok(CommitOutcome::NoChanges);
        }

        Err(combined)
    }

    fn classify_git_error(
        &self,
        command: &str,
        stderr: &str,
        remote: Option<&str>,
        branch: Option<&str>,
    ) -> SyncError {
        let stderr_lower = stderr.to_lowercase();

        if stderr.contains("couldn't find remote ref")
            || stderr.contains("fatal: couldn't find remote ref")
        {
            return SyncError::RemoteBranchNotFound {
                remote: remote.unwrap_or("origin").to_string(),
                branch: branch.unwrap_or("<unknown>").to_string(),
            };
        }

        if stderr_lower.contains("authentication failed")
            || stderr_lower.contains("permission denied")
            || stderr_lower.contains("could not read from remote repository")
        {
            return SyncError::AuthenticationFailed {
                operation: command.to_string(),
            };
        }

        if command.contains("commit")
            && (stderr_lower.contains("hook declined")
                || stderr_lower.contains("pre-commit")
                || stderr_lower.contains("pre-commit hook failed")
                || stderr_lower.contains("commit-msg")
                || stderr_lower.contains("commit-msg hook failed"))
        {
            return SyncError::HookRejected {
                details: stderr.trim().to_string(),
            };
        }

        SyncError::GitCommandFailed {
            command: command.to_string(),
            stderr: stderr.trim().to_string(),
        }
    }
}

impl GitTransport for CommandGitTransport {
    fn fetch_branch(&self, repo_path: &Path, remote: &str, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .arg("fetch")
            .arg(remote)
            .arg(branch)
            .current_dir(repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git fetch: {}", e)))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(self.classify_git_error(
            &format!("git fetch {} {}", remote, branch),
            &stderr,
            Some(remote),
            Some(branch),
        ))
    }

    fn push_refspec(&self, repo_path: &Path, remote: &str, refspec: &str) -> Result<()> {
        let output = Command::new("git")
            .arg("push")
            .arg(remote)
            .arg(refspec)
            .current_dir(repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git push: {}", e)))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(self.classify_git_error(
            &format!("git push {} {}", remote, refspec),
            &stderr,
            Some(remote),
            None,
        ))
    }

    fn push_branch_upstream(&self, repo_path: &Path, remote: &str, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .arg("push")
            .arg("-u")
            .arg(remote)
            .arg(branch)
            .current_dir(repo_path)
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git push: {}", e)))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(self.classify_git_error(
            &format!("git push -u {} {}", remote, branch),
            &stderr,
            Some(remote),
            Some(branch),
        ))
    }

    fn commit(&self, repo_path: &Path, message: &str, skip_hooks: bool) -> Result<CommitOutcome> {
        let output = self
            .run_commit_command(repo_path, message, skip_hooks, None)
            .map_err(|e| SyncError::Other(format!("Failed to run git commit: {}", e)))?;

        match Self::parse_commit_output(&output) {
            Ok(outcome) => return Ok(outcome),
            Err(combined) => {
                if Self::is_missing_identity_error(&combined) {
                    let (name, email) = Self::fallback_commit_identity();
                    let retry = self
                        .run_commit_command(repo_path, message, skip_hooks, Some((&name, &email)))
                        .map_err(|e| {
                            SyncError::Other(format!("Failed to rerun git commit: {}", e))
                        })?;

                    match Self::parse_commit_output(&retry) {
                        Ok(outcome) => return Ok(outcome),
                        Err(retry_combined) => {
                            let combined_errors = format!(
                                "git commit failed due to missing identity, fallback identity retry also failed.\n\ninitial:\n{}\n\nfallback retry:\n{}",
                                combined.trim(),
                                retry_combined.trim()
                            );
                            return Err(self.classify_git_error(
                                "git commit",
                                &combined_errors,
                                None,
                                None,
                            ));
                        }
                    }
                }

                Err(self.classify_git_error("git commit", &combined, None, None))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandGitTransport, CommitOutcome, GitTransport};
    use crate::error::SyncError;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[test]
    fn classifies_missing_remote_ref_errors() {
        let transport = CommandGitTransport;
        let err = transport.classify_git_error(
            "git fetch origin feature",
            "fatal: couldn't find remote ref feature",
            Some("origin"),
            Some("feature"),
        );
        assert!(matches!(
            err,
            SyncError::RemoteBranchNotFound {
                ref remote,
                ref branch
            } if remote == "origin" && branch == "feature"
        ));
    }

    #[test]
    fn classifies_authentication_errors() {
        let transport = CommandGitTransport;
        let err = transport.classify_git_error(
            "git push origin main:main",
            "Permission denied (publickey).",
            Some("origin"),
            Some("main"),
        );
        assert!(matches!(err, SyncError::AuthenticationFailed { .. }));
    }

    #[test]
    fn classifies_hook_rejections() {
        let transport = CommandGitTransport;
        let err = transport.classify_git_error(
            "git commit",
            "error: failed to push some refs\npre-commit hook failed",
            None,
            None,
        );
        assert!(matches!(err, SyncError::HookRejected { .. }));
    }

    #[test]
    fn detects_missing_identity_errors() {
        assert!(CommandGitTransport::is_missing_identity_error(
            "Author identity unknown\nfatal: unable to auto-detect email address"
        ));
        assert!(CommandGitTransport::is_missing_identity_error(
            "Please tell me who you are."
        ));
        assert!(CommandGitTransport::is_missing_identity_error(
            "fatal: empty ident name (for <>) not allowed"
        ));
        assert!(!CommandGitTransport::is_missing_identity_error(
            "nothing to commit, working tree clean"
        ));
    }

    #[test]
    fn commit_retries_with_fallback_identity_when_git_identity_missing() {
        let _guard = ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("acquire environment mutation lock");

        let temp = tempdir().expect("create tempdir");
        let repo_path = temp.path().join("repo");
        std::fs::create_dir(&repo_path).expect("create repo dir");

        run_git(
            temp.path(),
            &["init", repo_path.to_str().expect("path utf8")],
        );
        std::fs::write(repo_path.join("file.txt"), "hello\n").expect("write test file");
        run_git(&repo_path, &["add", "file.txt"]);

        let old_home = std::env::var("HOME").ok();
        let old_xdg_config_home = std::env::var("XDG_CONFIG_HOME").ok();
        let old_git_config_global = std::env::var("GIT_CONFIG_GLOBAL").ok();
        let old_git_config_nosystem = std::env::var("GIT_CONFIG_NOSYSTEM").ok();
        let old_author_name = std::env::var("GIT_AUTHOR_NAME").ok();
        let old_author_email = std::env::var("GIT_AUTHOR_EMAIL").ok();
        let old_committer_name = std::env::var("GIT_COMMITTER_NAME").ok();
        let old_committer_email = std::env::var("GIT_COMMITTER_EMAIL").ok();

        let no_config_home = temp.path().join("empty-home");
        std::fs::create_dir(&no_config_home).expect("create empty HOME");
        std::env::set_var("HOME", &no_config_home);
        std::env::set_var("XDG_CONFIG_HOME", &no_config_home);
        std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        std::env::remove_var("GIT_AUTHOR_NAME");
        std::env::remove_var("GIT_AUTHOR_EMAIL");
        std::env::remove_var("GIT_COMMITTER_NAME");
        std::env::remove_var("GIT_COMMITTER_EMAIL");

        let transport = CommandGitTransport;
        let expected_identity = CommandGitTransport::fallback_commit_identity();
        let result = transport
            .commit(&repo_path, "auto commit message", false)
            .expect("commit should succeed with fallback identity");
        assert_eq!(result, CommitOutcome::Created);

        let log_out = Command::new("git")
            .args(["log", "-1", "--pretty=format:%an|%ae"])
            .current_dir(&repo_path)
            .output()
            .expect("read commit identity");
        assert!(log_out.status.success(), "git log failed");
        let observed = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
        assert_eq!(
            observed,
            format!("{}|{}", expected_identity.0, expected_identity.1)
        );

        restore_env("HOME", old_home);
        restore_env("XDG_CONFIG_HOME", old_xdg_config_home);
        restore_env("GIT_CONFIG_GLOBAL", old_git_config_global);
        restore_env("GIT_CONFIG_NOSYSTEM", old_git_config_nosystem);
        restore_env("GIT_AUTHOR_NAME", old_author_name);
        restore_env("GIT_AUTHOR_EMAIL", old_author_email);
        restore_env("GIT_COMMITTER_NAME", old_committer_name);
        restore_env("GIT_COMMITTER_EMAIL", old_committer_email);
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git command");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn restore_env(name: &str, value: Option<String>) {
        if let Some(v) = value {
            std::env::set_var(name, v);
        } else {
            std::env::remove_var(name);
        }
    }
}

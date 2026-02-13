use crate::error::{Result, SyncError};
use std::path::Path;
use std::process::Command;

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
        let mut command = Command::new("git");
        command.arg("commit");
        if skip_hooks {
            command.arg("--no-verify");
        }
        command.arg("-m").arg(message).current_dir(repo_path);

        let output = command
            .output()
            .map_err(|e| SyncError::Other(format!("Failed to run git commit: {}", e)))?;

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

        Err(self.classify_git_error("git commit", &combined, None, None))
    }
}

#[cfg(test)]
mod tests {
    use super::CommandGitTransport;
    use crate::error::SyncError;

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
}

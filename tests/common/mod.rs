use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// Test repository setup that creates a bare remote and a local clone
pub struct TestRepoSetup {
    pub temp_dir: TempDir,
    pub remote_path: PathBuf,
    pub local_path: PathBuf,
}

impl TestRepoSetup {
    /// Create a new test repository setup with a bare remote and local clone
    pub fn new() -> Result<Self> {
        let temp_dir = TempDir::new()?;
        let base_path = temp_dir.path();

        // Create paths
        let remote_path = base_path.join("remote.git");
        let local_path = base_path.join("local");

        // Initialize bare repository (acts as remote)
        run_git_command(
            base_path,
            &["init", "--bare", remote_path.to_str().unwrap()],
        )?;

        // Clone to create local repository
        run_git_command(
            base_path,
            &[
                "clone",
                remote_path.to_str().unwrap(),
                local_path.to_str().unwrap(),
            ],
        )?;

        // Configure local repository
        run_git_command(&local_path, &["config", "user.email", "test@example.com"])?;
        run_git_command(&local_path, &["config", "user.name", "Test User"])?;

        Ok(Self {
            temp_dir,
            remote_path,
            local_path,
        })
    }

    /// Create a second clone of the remote (simulates another user)
    pub fn create_second_clone(&self, name: &str) -> Result<PathBuf> {
        let second_path = self.temp_dir.path().join(name);
        run_git_command(
            self.temp_dir.path(),
            &[
                "clone",
                self.remote_path.to_str().unwrap(),
                second_path.to_str().unwrap(),
            ],
        )?;

        // Configure the second clone
        run_git_command(&second_path, &["config", "user.email", "test2@example.com"])?;
        run_git_command(&second_path, &["config", "user.name", "Test User 2"])?;

        Ok(second_path)
    }

    /// Make a commit in the local repository
    #[allow(dead_code)]
    pub fn commit_file(&self, filename: &str, content: &str, message: &str) -> Result<()> {
        let file_path = self.local_path.join(filename);
        fs::write(&file_path, content)?;

        run_git_command(&self.local_path, &["add", filename])?;
        run_git_command(&self.local_path, &["commit", "-m", message])?;

        Ok(())
    }

    /// Make a commit in a specific repository
    #[allow(dead_code)]
    pub fn commit_file_in(
        &self,
        repo_path: &Path,
        filename: &str,
        content: &str,
        message: &str,
    ) -> Result<()> {
        let file_path = repo_path.join(filename);
        fs::write(&file_path, content)?;

        run_git_command(repo_path, &["add", filename])?;
        run_git_command(repo_path, &["commit", "-m", message])?;

        Ok(())
    }

    /// Push changes from local to remote
    pub fn push(&self) -> Result<()> {
        run_git_command(&self.local_path, &["push", "origin", "master"])
    }

    /// Push changes from a specific repository
    #[allow(dead_code)]
    pub fn push_from(&self, repo_path: &Path) -> Result<()> {
        run_git_command(repo_path, &["push", "origin", "master"])
    }

    /// Pull changes from remote to local
    #[allow(dead_code)]
    pub fn pull(&self) -> Result<()> {
        run_git_command(&self.local_path, &["pull", "origin", "master"])
    }

    /// Pull changes in a specific repository
    #[allow(dead_code)]
    pub fn pull_in(&self, repo_path: &Path) -> Result<()> {
        run_git_command(repo_path, &["pull", "origin", "master"])
    }

    /// Check if a file exists with expected content
    #[allow(dead_code)]
    pub fn assert_file_content(&self, filename: &str, expected: &str) -> Result<()> {
        let content = fs::read_to_string(self.local_path.join(filename))?;
        assert_eq!(content, expected, "File content mismatch for {}", filename);
        Ok(())
    }

    /// Check if a file exists with expected content in a specific repository
    #[allow(dead_code)]
    pub fn assert_file_content_in(
        &self,
        repo_path: &Path,
        filename: &str,
        expected: &str,
    ) -> Result<()> {
        let content = fs::read_to_string(repo_path.join(filename))?;
        assert_eq!(content, expected, "File content mismatch for {}", filename);
        Ok(())
    }

    /// Get the current branch name
    #[allow(dead_code)]
    pub fn get_current_branch(&self) -> Result<String> {
        get_current_branch(&self.local_path)
    }

    /// Get the current branch name in a specific repository
    #[allow(dead_code)]
    pub fn get_current_branch_in(&self, repo_path: &Path) -> Result<String> {
        get_current_branch(repo_path)
    }

    /// Create a new branch in local repository
    #[allow(dead_code)]
    pub fn create_branch(&self, branch_name: &str) -> Result<()> {
        run_git_command(&self.local_path, &["checkout", "-b", branch_name])
    }

    /// Switch to a branch in local repository
    #[allow(dead_code)]
    pub fn checkout_branch(&self, branch_name: &str) -> Result<()> {
        run_git_command(&self.local_path, &["checkout", branch_name])
    }
}

/// Helper function to run git commands
fn run_git_command(cwd: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").current_dir(cwd).args(args).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Git command failed: git {} in {:?}\nError: {}",
            args.join(" "),
            cwd,
            stderr
        );
    }

    Ok(())
}

/// Get the current branch name
#[allow(dead_code)]
fn get_current_branch(repo_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to get current branch: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Helper to create a .gitignore file
#[allow(dead_code)]
pub fn create_gitignore(repo_path: &Path, patterns: &[&str]) -> Result<()> {
    let content = patterns.join("\n");
    fs::write(repo_path.join(".gitignore"), content)?;
    Ok(())
}

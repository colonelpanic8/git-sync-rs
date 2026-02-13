mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};
use std::fs;
use std::path::Path;
use std::process::Command;

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

#[test]
fn nested_git_repo_directory_does_not_break_auto_commit() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Seed the remote with an initial commit.
    setup.commit_file("README.md", "initial\n", "Initial commit")?;
    setup.push()?;

    // Create an untracked nested git repository under a directory name that is commonly ignored
    // in real-world repos (e.g. git worktrees). This is similar to:
    //   ?? .worktrees/some-worktree/
    let nested_root = setup
        .local_path
        .join(".worktrees")
        .join("fix-org-categories");
    fs::create_dir_all(&nested_root)?;
    run_git_command(&nested_root, &["init"])?;
    fs::write(nested_root.join("nested.txt"), "nested\n")?;

    // Also make a real tracked change so an auto-commit should be created.
    fs::write(setup.local_path.join("README.md"), "changed\n")?;

    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Auto-commit: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    let second_clone = setup.create_second_clone("second")?;

    setup.assert_file_content_in(&second_clone, "README.md", "changed\n")?;

    // The nested repo should not have been synced into the remote.
    assert!(
        !second_clone.join(".worktrees").exists(),
        "Nested repository directory should not be synced"
    );

    Ok(())
}

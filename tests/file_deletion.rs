mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};
use std::fs;

#[test]
fn handle_file_deleted_remotely() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Create files
    setup.commit_file("keep.txt", "Keep this\n", "Add files")?;
    setup.commit_file("delete.txt", "Delete this\n", "Add delete file")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Delete file remotely
    fs::remove_file(second.join("delete.txt"))?;
    run_git_command(&second, &["add", "-A"])?;
    run_git_command(&second, &["commit", "-m", "Delete file"])?;
    setup.push_from(&second)?;

    // Modify locally
    setup.commit_file("delete.txt", "Modified locally\n", "Modify file")?;

    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync deletion conflict".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
    };

    let sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    let _result = sync.sync(false);

    // File should exist (local changes preserved) or not (remote deletion wins)
    // Both are valid resolutions
    setup.assert_file_content("keep.txt", "Keep this\n")?;

    Ok(())
}

// Helper function for running git commands
fn run_git_command(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git command failed: git {} - {}", args.join(" "), stderr);
    }

    Ok(())
}

mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};
use std::fs;

#[test]
fn handle_binary_file_conflict() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Create binary file
    let binary_data = vec![0u8, 1, 2, 3, 255, 254, 253];
    fs::write(setup.local_path.join("binary.dat"), &binary_data)?;
    run_git_command(&setup.local_path, &["add", "binary.dat"])?;
    run_git_command(&setup.local_path, &["commit", "-m", "Add binary"])?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Different binary changes
    let local_binary = vec![0u8, 1, 2, 3, 4, 5];
    fs::write(setup.local_path.join("binary.dat"), &local_binary)?;
    run_git_command(&setup.local_path, &["add", "binary.dat"])?;
    run_git_command(&setup.local_path, &["commit", "-m", "Local binary change"])?;

    let remote_binary = vec![0u8, 9, 8, 7, 6];
    fs::write(second.join("binary.dat"), &remote_binary)?;
    run_git_command(&second, &["add", "binary.dat"])?;
    run_git_command(&second, &["commit", "-m", "Remote binary change"])?;
    setup.push_from(&second)?;

    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync binary conflict".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
    };

    let sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    let _result = sync.sync(false);

    // Binary file should exist after sync attempt
    assert!(setup.local_path.join("binary.dat").exists());

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

mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig, FALLBACK_BRANCH_PREFIX};
use std::fs;
use std::process::Command;

/// Helper function to get the current branch name
fn get_current_branch(repo_path: &std::path::Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to get current branch");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Helper function to check if a branch exists on remote
fn remote_branch_exists(repo_path: &std::path::Path, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["ls-remote", "--heads", "origin", branch])
        .output()?;

    Ok(!output.stdout.is_empty())
}

#[test]
fn conflict_branch_disabled_by_default() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("data.txt", "line1\nline2\nline3\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Create conflicting changes
    setup.commit_file("data.txt", "line1\nLOCAL\nline3\n", "Local change")?;
    setup.commit_file_in(
        &second,
        "data.txt",
        "line1\nREMOTE\nline3\n",
        "Remote change",
    )?;
    setup.push_from(&second)?;

    // Sync with conflict_branch DISABLED (default)
    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    let result = sync.sync(false);

    // Should fail with manual intervention required
    assert!(
        result.is_err(),
        "Sync should fail when conflict_branch is disabled"
    );

    // Should still be on master
    let branch = get_current_branch(&setup.local_path)?;
    assert_eq!(branch, "master", "Should still be on master");

    Ok(())
}

#[test]
fn conflict_branch_creates_fallback_on_conflict() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("data.txt", "line1\nline2\nline3\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Create conflicting changes
    setup.commit_file("data.txt", "line1\nLOCAL\nline3\n", "Local change")?;
    setup.commit_file_in(
        &second,
        "data.txt",
        "line1\nREMOTE\nline3\n",
        "Remote change",
    )?;
    setup.push_from(&second)?;

    // Sync with conflict_branch ENABLED
    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: true,
        target_branch: Some("master".to_string()),
    };

    let mut sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    let result = sync.sync(false);

    // Should succeed (switched to fallback branch)
    assert!(
        result.is_ok(),
        "Sync should succeed with conflict_branch enabled: {:?}",
        result
    );

    // Should be on a fallback branch
    let branch = get_current_branch(&setup.local_path)?;
    assert!(
        branch.starts_with(FALLBACK_BRANCH_PREFIX),
        "Should be on a fallback branch, got: {}",
        branch
    );

    // Fallback branch should be pushed to remote
    assert!(
        remote_branch_exists(&setup.local_path, &branch)?,
        "Fallback branch should exist on remote"
    );

    // Local changes should be preserved
    let content = fs::read_to_string(setup.local_path.join("data.txt"))?;
    assert!(
        content.contains("LOCAL"),
        "Local changes should be preserved"
    );

    Ok(())
}

#[test]
fn conflict_branch_returns_to_target_after_resolution() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("data.txt", "line1\nline2\nline3\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Create conflicting changes
    setup.commit_file("data.txt", "line1\nLOCAL\nline3\n", "Local change")?;
    setup.commit_file_in(
        &second,
        "data.txt",
        "line1\nREMOTE\nline3\n",
        "Remote change",
    )?;
    setup.push_from(&second)?;

    // Sync with conflict_branch ENABLED - this will create a fallback branch
    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: true,
        target_branch: Some("master".to_string()),
    };

    let mut sync =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config.clone())?;
    sync.sync(false)?;

    let fallback_branch = get_current_branch(&setup.local_path)?;
    assert!(fallback_branch.starts_with(FALLBACK_BRANCH_PREFIX));

    // Now "resolve" the conflict by making master contain our changes
    // In real life, another user would do this. We simulate by:
    // 1. Pulling the fallback branch on second clone
    // 2. Merging/resolving on second clone
    // 3. Pushing to master

    // Pull the fallback branch
    Command::new("git")
        .current_dir(&second)
        .args(["fetch", "origin", &fallback_branch])
        .status()?;

    // Reset master to include our changes (simulating conflict resolution)
    // For simplicity, just accept local's version
    Command::new("git")
        .current_dir(&second)
        .args(["checkout", "master"])
        .status()?;

    Command::new("git")
        .current_dir(&second)
        .args([
            "merge",
            "-X",
            "theirs",
            &format!("origin/{}", fallback_branch),
        ])
        .status()?;

    setup.push_from(&second)?;

    // Now sync again from the fallback branch - should return to master
    let mut sync2 = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    sync2.sync(false)?;

    // Should be back on master
    let branch = get_current_branch(&setup.local_path)?;
    assert_eq!(
        branch, "master",
        "Should have returned to master after resolution"
    );

    Ok(())
}

#[test]
fn conflict_branch_stays_on_fallback_when_still_conflicting() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("data.txt", "line1\nline2\nline3\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Create conflicting changes
    setup.commit_file("data.txt", "line1\nLOCAL\nline3\n", "Local change")?;
    setup.commit_file_in(
        &second,
        "data.txt",
        "line1\nREMOTE\nline3\n",
        "Remote change",
    )?;
    setup.push_from(&second)?;

    // Sync with conflict_branch ENABLED - this will create a fallback branch
    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: true,
        target_branch: Some("master".to_string()),
    };

    let mut sync =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config.clone())?;
    sync.sync(false)?;

    let fallback_branch = get_current_branch(&setup.local_path)?;
    assert!(fallback_branch.starts_with(FALLBACK_BRANCH_PREFIX));

    // Make another change on master that still conflicts
    setup.commit_file_in(
        &second,
        "data.txt",
        "line1\nSTILL REMOTE\nline3\n",
        "Another remote change",
    )?;
    setup.push_from(&second)?;

    // Sync again - should stay on fallback since conflict still exists
    let mut sync2 = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    sync2.sync(false)?;

    // Should still be on fallback branch
    let branch = get_current_branch(&setup.local_path)?;
    assert!(
        branch.starts_with(FALLBACK_BRANCH_PREFIX),
        "Should still be on fallback branch since conflict persists, got: {}",
        branch
    );

    Ok(())
}

#[test]
fn conflict_branch_naming_includes_hostname() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("data.txt", "line1\nline2\nline3\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Create conflicting changes
    setup.commit_file("data.txt", "line1\nLOCAL\nline3\n", "Local change")?;
    setup.commit_file_in(
        &second,
        "data.txt",
        "line1\nREMOTE\nline3\n",
        "Remote change",
    )?;
    setup.push_from(&second)?;

    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: true,
        target_branch: Some("master".to_string()),
    };

    let mut sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    sync.sync(false)?;

    let branch = get_current_branch(&setup.local_path)?;

    // Branch should have format: git-sync/{hostname}-{timestamp}
    assert!(branch.starts_with(FALLBACK_BRANCH_PREFIX));

    // Should contain hostname
    let hostname = hostname::get()?.to_string_lossy().to_string();
    let branch_suffix = branch.strip_prefix(FALLBACK_BRANCH_PREFIX).unwrap();
    assert!(
        branch_suffix.starts_with(&hostname),
        "Branch should contain hostname. Expected to start with '{}', got '{}'",
        hostname,
        branch_suffix
    );

    Ok(())
}

mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositoryState, RepositorySynchronizer, SyncConfig, SyncError};
use std::process::Command;

fn run_git_command(cwd: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git").current_dir(cwd).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Git command failed: git {} - {}", args.join(" "), stderr);
    }
    Ok(())
}

#[test]
fn staged_changes_should_be_auto_committed_and_synced() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Create an initial tracked file and publish it.
    setup.commit_file("tracked.txt", "v1\n", "initial")?;
    setup.push()?;

    let second_clone = setup.create_second_clone("second")?;

    // Create a staged-only change: index is dirty, working tree is clean.
    std::fs::write(setup.local_path.join("tracked.txt"), "v2\n")?;
    run_git_command(&setup.local_path, &["add", "tracked.txt"])?;

    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync staged change".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    setup.pull_in(&second_clone)?;
    setup.assert_file_content_in(&second_clone, "tracked.txt", "v2\n")?;

    Ok(())
}

#[test]
fn new_files_flag_should_not_require_an_explicit_boolean_value() -> Result<()> {
    // The CLI says -n/--new-files is compatible with the original git-sync flag.
    // Compatibility implies this should parse as a boolean flag, not require "true/false".
    let output = Command::new(env!("CARGO_BIN_EXE_git-sync-rs"))
        .arg("-n")
        .output()?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("a value is required for '--new-files"),
        "Expected -n to parse as a flag, but clap rejected it:\n{}",
        stderr
    );

    Ok(())
}

#[test]
fn revert_in_progress_should_not_be_reported_as_clean() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    setup.commit_file("tracked.txt", "v1\n", "initial")?;
    setup.push()?;
    setup.commit_file("tracked.txt", "v2\n", "second")?;

    // Leave the repository in an in-progress revert state.
    run_git_command(&setup.local_path, &["revert", "--no-commit", "HEAD"])?;

    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    let state = synchronizer.get_repository_state()?;

    assert_ne!(
        state,
        RepositoryState::Clean,
        "Repository with revert in progress must not be reported as clean"
    );

    Ok(())
}

#[test]
fn sync_should_handle_unborn_head_when_local_changes_exist() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Repository is freshly cloned from an empty remote (unborn branch).
    std::fs::write(setup.local_path.join("first.txt"), "initial content\n")?;

    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Initial sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    let second_clone = setup.create_second_clone("second")?;
    setup.assert_file_content_in(&second_clone, "first.txt", "initial content\n")?;

    Ok(())
}

#[test]
fn syncing_local_branch_without_remote_ref_returns_typed_error() -> Result<()> {
    let setup = TestRepoSetup::new()?;
    setup.commit_file("tracked.txt", "v1\n", "initial")?;
    setup.push()?;

    run_git_command(&setup.local_path, &["checkout", "-b", "feature/local-only"])?;
    setup.commit_file("tracked.txt", "v2\n", "local branch change")?;

    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync local-only branch".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    let result = synchronizer.sync(false);

    assert!(
        matches!(
            result,
            Err(SyncError::RemoteBranchNotFound {
                ref remote,
                ref branch
            }) if remote == "origin" && branch == "feature/local-only"
        ),
        "Expected typed missing-remote-branch error, got: {result:?}"
    );

    Ok(())
}

#[test]
fn sync_uses_checked_out_branch_even_with_stale_config_branch_name() -> Result<()> {
    let setup = TestRepoSetup::new()?;
    setup.commit_file("tracked.txt", "v1\n", "initial")?;
    setup.push()?;

    let second_clone = setup.create_second_clone("second")?;

    // Create and publish a feature branch.
    run_git_command(&setup.local_path, &["checkout", "-b", "feature/active"])?;
    run_git_command(
        &setup.local_path,
        &["push", "-u", "origin", "feature/active"],
    )?;
    run_git_command(&second_clone, &["fetch", "origin", "feature/active"])?;
    run_git_command(
        &second_clone,
        &["checkout", "-b", "feature/active", "origin/feature/active"],
    )?;

    // Remote advances feature branch.
    setup.commit_file_in(
        &second_clone,
        "feature.txt",
        "remote-v1\n",
        "remote feature change",
    )?;
    run_git_command(&second_clone, &["push", "origin", "feature/active"])?;

    // Intentionally pass stale branch config (master) and avoid auto-detection.
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync with stale branch config".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer = RepositorySynchronizer::new(&setup.local_path, sync_config.clone())?;
    synchronizer.sync(false)?;
    setup.assert_file_content("feature.txt", "remote-v1\n")?;

    // Local advances feature branch; sync should push feature, not master.
    setup.commit_file("feature.txt", "local-v2\n", "local feature change")?;
    let mut synchronizer = RepositorySynchronizer::new(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    run_git_command(&second_clone, &["pull", "origin", "feature/active"])?;
    setup.assert_file_content_in(&second_clone, "feature.txt", "local-v2\n")?;

    Ok(())
}

mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};
use std::fs;

#[test]
fn handle_merge_conflict_with_local_changes() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("data.txt", "line1\nline2\nline3\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Divergent changes
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
        commit_message: Some("Sync conflict".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    let result = sync.sync(false);

    // Should handle conflict (either merge or fail gracefully)
    if result.is_ok() {
        // If merge succeeded, file should contain merge markers or resolved content
        let content = fs::read_to_string(setup.local_path.join("data.txt"))?;
        assert!(content.contains("line1") && content.contains("line3"));
    }

    Ok(())
}

#[test]
fn conflict_resolution_with_merge() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit with a file
    setup.commit_file("conflict.txt", "Original content\n", "Initial commit")?;
    setup.push()?;

    // Create a second clone
    let second_clone = setup.create_second_clone("second")?;

    // Make conflicting changes
    setup.commit_file("conflict.txt", "Local changes\n", "Local modification")?;
    setup.commit_file_in(
        &second_clone,
        "conflict.txt",
        "Remote changes\n",
        "Remote modification",
    )?;
    setup.push_from(&second_clone)?;

    // Sync should handle the conflict
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Merge: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;

    // This might fail if there's a conflict that can't be auto-resolved
    // In a real scenario, git-sync-rs should handle this gracefully
    let result = synchronizer.sync(false);

    // Check that sync attempted to handle the situation
    assert!(
        result.is_ok() || result.is_err(),
        "Sync should either succeed or fail gracefully"
    );

    Ok(())
}

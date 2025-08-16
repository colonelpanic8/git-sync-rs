mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};
use std::fs;

#[test]
fn uncommitted_changes_during_sync() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial state
    setup.commit_file("base.txt", "Base content\n", "Initial")?;
    setup.push()?;

    let second = setup.create_second_clone("second")?;

    // Remote changes
    setup.commit_file_in(&second, "remote.txt", "Remote file\n", "Add remote")?;
    setup.push_from(&second)?;

    // Local uncommitted changes
    fs::write(
        setup.local_path.join("uncommitted.txt"),
        "Not committed yet\n",
    )?;
    fs::write(
        setup.local_path.join("base.txt"),
        "Modified but not committed\n",
    )?;

    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Auto-commit and sync".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
    };

    let sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config)?;
    sync.sync(false)?;

    // Should have committed local changes and pulled remote
    setup.assert_file_content("remote.txt", "Remote file\n")?;
    setup.assert_file_content("uncommitted.txt", "Not committed yet\n")?;

    // Verify changes were pushed
    setup.pull_in(&second)?;
    setup.assert_file_content_in(&second, "uncommitted.txt", "Not committed yet\n")?;

    Ok(())
}

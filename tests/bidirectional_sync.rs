mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};

#[test]
fn bidirectional_sync() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit and push
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    setup.push()?;

    // Create a second clone
    let second_clone = setup.create_second_clone("second")?;

    // Make changes in both repositories
    setup.commit_file("local_file.txt", "Changes from local\n", "Add local file")?;
    setup.commit_file_in(
        &second_clone,
        "remote_file.txt",
        "Changes from remote\n",
        "Add remote file",
    )?;
    setup.push_from(&second_clone)?;

    // Sync should handle both pushing local changes and pulling remote changes
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    // Verify both files exist after sync
    setup.assert_file_content("local_file.txt", "Changes from local\n")?;
    setup.assert_file_content("remote_file.txt", "Changes from remote\n")?;

    // Pull in the second clone to verify local changes were pushed
    setup.pull_in(&second_clone)?;
    setup.assert_file_content_in(&second_clone, "local_file.txt", "Changes from local\n")?;

    Ok(())
}

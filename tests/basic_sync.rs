mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};

#[test]
fn basic_sync() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit and push
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    setup.push()?;

    // Create a second clone (simulates another user)
    let second_clone = setup.create_second_clone("second")?;

    // Make changes in the second clone and push
    setup.commit_file_in(
        &second_clone,
        "file1.txt",
        "Hello from second clone\n",
        "Add file1",
    )?;
    setup.push_from(&second_clone)?;

    // Now use git-sync-rs to sync the local repository
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
    };

    let synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    // Verify the file was synced
    setup.assert_file_content("file1.txt", "Hello from second clone\n")?;

    Ok(())
}

mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};

#[test]
fn auto_commit_new_files() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit and push
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    setup.push()?;

    // Add a new untracked file
    std::fs::write(
        setup.local_path.join("new_file.txt"),
        "This is a new file\n",
    )?;

    // Sync with auto-commit enabled
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

    // Create a second clone and pull to verify the file was committed and pushed
    let second_clone = setup.create_second_clone("second")?;
    setup.assert_file_content_in(&second_clone, "new_file.txt", "This is a new file\n")?;

    Ok(())
}

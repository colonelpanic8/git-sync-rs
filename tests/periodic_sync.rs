mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::time::Duration;

#[tokio::test]
async fn periodic_sync_works() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit
    setup.commit_file("README.md", "# Periodic Sync\n", "Initial")?;
    setup.push()?;

    // Create a second clone and make a change there
    let second_clone = setup.create_second_clone("second")?;

    // Start watch mode with periodic sync
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Periodic sync: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let watch_config = WatchConfig {
        debounce_ms: 100,
        min_interval_ms: 200,
        sync_on_start: false,
        dry_run: false,
    };

    let local_path = setup.local_path.clone();
    let watch_handle = tokio::spawn(async move {
        watch_with_periodic_sync(
            &local_path,
            sync_config,
            watch_config,
            Some(1000), // Periodic sync every 1 second
        )
        .await
    });

    // Give watcher time to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Make a change in the second clone and push
    setup.commit_file_in(
        &second_clone,
        "remote_change.txt",
        "From remote\n",
        "Remote change",
    )?;
    setup.push_from(&second_clone)?;

    // Wait for periodic sync to pull the change
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Verify the change was pulled
    setup.assert_file_content("remote_change.txt", "From remote\n")?;

    watch_handle.abort();

    Ok(())
}

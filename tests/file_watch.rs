mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::fs;
use std::path::Path;
use std::time::Duration;

#[tokio::test]
async fn file_changes_trigger_sync() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit and push
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    setup.push()?;

    // Create a second clone to verify syncs
    let second_clone = setup.create_second_clone("second")?;

    // Start watch mode in a background task
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Auto-sync: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let watch_config = WatchConfig {
        debounce_ms: 100, // Short debounce for testing
        min_interval_ms: 200,
        sync_on_start: false,
        dry_run: false,
        ..Default::default()
    };

    let local_path = setup.local_path.clone();
    let watch_handle = tokio::spawn(async move {
        watch_with_periodic_sync(
            &local_path,
            sync_config,
            watch_config,
            None, // No periodic sync
        )
        .await
    });

    // Give watcher time to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create a new file
    fs::write(
        setup.local_path.join("newfile.txt"),
        "Content added during watch\n",
    )?;

    wait_for_file_in_clone(
        &setup,
        &second_clone,
        "newfile.txt",
        "Content added during watch\n",
    )
    .await?;

    // Cancel the watch task
    watch_handle.abort();

    Ok(())
}

async fn wait_for_file_in_clone(
    setup: &TestRepoSetup,
    clone_path: &Path,
    filename: &str,
    expected: &str,
) -> Result<()> {
    for _ in 0..50 {
        let _ = setup.pull_in(clone_path);

        match fs::read_to_string(clone_path.join(filename)) {
            Ok(content) if content == expected => return Ok(()),
            Ok(_) | Err(_) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }

    setup.pull_in(clone_path)?;
    setup.assert_file_content_in(clone_path, filename, expected)
}

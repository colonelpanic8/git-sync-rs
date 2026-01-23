mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::fs;
use std::time::Duration;

#[tokio::test]
async fn gitignored_files_not_synced() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create gitignore
    setup.commit_file(".gitignore", "*.log\nbuild/\nsecret*\n", "Add gitignore")?;
    setup.push()?;

    // Create a second clone
    let second_clone = setup.create_second_clone("second")?;

    // Start watch mode
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
        debounce_ms: 100,
        min_interval_ms: 200,
        sync_on_start: false,
        dry_run: false,
    };

    let local_path = setup.local_path.clone();
    let watch_handle = tokio::spawn(async move {
        watch_with_periodic_sync(&local_path, sync_config, watch_config, None).await
    });

    // Give watcher time to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Create both ignored and non-ignored files
    fs::write(setup.local_path.join("debug.log"), "Log content\n")?;
    fs::write(setup.local_path.join("secret_key.txt"), "Secret\n")?;
    fs::create_dir_all(setup.local_path.join("build"))?;
    fs::write(setup.local_path.join("build/output.txt"), "Build output\n")?;
    fs::write(setup.local_path.join("normal.txt"), "Normal file\n")?;

    // Wait for sync
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Pull in second clone
    setup.pull_in(&second_clone)?;

    // Verify only non-ignored file was synced
    setup.assert_file_content_in(&second_clone, "normal.txt", "Normal file\n")?;
    assert!(
        !second_clone.join("debug.log").exists(),
        "Log file should not be synced"
    );
    assert!(
        !second_clone.join("secret_key.txt").exists(),
        "Secret file should not be synced"
    );
    assert!(
        !second_clone.join("build").exists(),
        "Build directory should not be synced"
    );

    watch_handle.abort();

    Ok(())
}

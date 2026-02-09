mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

#[tokio::test]
async fn continuous_changes_starve_debounce() -> Result<()> {
    // Set up repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit and push
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    setup.push()?;

    // Create a second clone to observe remote state
    let second_clone = setup.create_second_clone("second")?;

    // Start watch mode in a background task with small debounce
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
        debounce_ms: 100, // short debounce to make behavior visible
        min_interval_ms: 200,
        sync_on_start: false,
        dry_run: false,
        ..Default::default()
    };

    let local_path = setup.local_path.clone();
    let watch_handle = tokio::spawn(async move {
        watch_with_periodic_sync(&local_path, sync_config, watch_config, None).await
    });

    // Give watcher time to start
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Continuously modify a file faster than the debounce window
    let burst_file = setup.local_path.join("burst.txt");
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1200) {
        fs::write(&burst_file, format!("tick at {:?}\n", Instant::now()))?;
        tokio::time::sleep(Duration::from_millis(40)).await; // keep events flowing
    }

    // Even while changes are continuous, with throttle we should make progress.
    // After some time, a sync should have occurred despite ongoing events.
    tokio::time::sleep(Duration::from_millis(600)).await;
    setup.pull_in(&second_clone)?;
    assert!(
        Path::new(&second_clone).join("burst.txt").exists(),
        "File not synced during continuous change stream with throttle"
    );

    // After changes stop, debounce should fire and a sync should happen shortly.
    tokio::time::sleep(Duration::from_millis(500)).await;
    setup.pull_in(&second_clone)?;
    assert!(
        Path::new(&second_clone).join("burst.txt").exists(),
        "File was not synced after change stream ended"
    );

    // Stop the watcher
    watch_handle.abort();

    Ok(())
}

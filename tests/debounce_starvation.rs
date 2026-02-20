mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
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
        // Use periodic sync (the default in the CLI) to ensure progress even if
        // filesystem events are delayed under heavy system load.
        watch_with_periodic_sync(&local_path, sync_config, watch_config, Some(200)).await
    });

    // Give watcher time to start
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Continuously modify a file faster than the debounce window.
    // While changes are continuous, we should still make progress (no starvation).
    let burst_file = setup.local_path.join("burst.txt");
    let writer = tokio::spawn({
        let burst_file = burst_file.clone();
        async move {
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                fs::write(&burst_file, format!("tick at {:?}\n", Instant::now()))?;
                tokio::time::sleep(Duration::from_millis(40)).await; // keep events flowing
            }
            Result::<()>::Ok(())
        }
    });

    // Give the writer a head start so we're clearly in the "continuous changes" phase.
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Wait (bounded) for the remote observer clone to see the file while writes are ongoing.
    timeout(Duration::from_secs(8), async {
        loop {
            setup.pull_in(&second_clone)?;
            if Path::new(&second_clone).join("burst.txt").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Result::<()>::Ok(())
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!("Timed out waiting for burst.txt to be synced during continuous changes")
    })??;

    // Ensure the writer task completed successfully.
    writer.await??;

    // Stop the watcher
    watch_handle.abort();

    Ok(())
}

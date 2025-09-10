mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

#[tokio::test]
async fn sync_stuck_after_error_during_watch() -> Result<()> {
    // Set up repositories
    let setup = TestRepoSetup::new()?;

    // Create initial commit and push
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    setup.push()?;

    // Create a second clone to observe remote state
    let second_clone = setup.create_second_clone("second")?;

    // Put local repo into a state that makes sync() return an error (detached HEAD)
    let status = Command::new("git")
        .current_dir(&setup.local_path)
        .args(["checkout", "--detach", "HEAD"])
        .status()?;
    assert!(status.success(), "Failed to detach HEAD");

    // Start watch mode in a background task
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Auto-sync: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
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
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Create a file to trigger sync
    let fname = "deadlock.txt";
    fs::write(setup.local_path.join(fname), "content\n")?;

    // Wait long enough for the failing sync attempt to occur
    tokio::time::sleep(Duration::from_millis(600)).await;

    // Repair repo state (reattach to master) so future syncs should be allowed
    let status = Command::new("git")
        .current_dir(&setup.local_path)
        .args(["checkout", "-f", "master"]) // force to avoid conflicts
        .status()?;
    assert!(status.success(), "Failed to reattach to master");

    // Nudge the watcher with another change after repair to ensure pending
    std::fs::write(setup.local_path.join(fname), "content2\n")?;

    // Poll for sync completion with a reasonable timeout
    let mut synced = false;
    for _ in 0..30 {
        // ~6s total
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = setup.pull_in(&second_clone); // ignore transient failures
        if Path::new(&second_clone).join(fname).exists() {
            synced = true;
            break;
        }
    }
    if !synced {
        // Print some debugging info to help diagnose
        let status_out = std::process::Command::new("git")
            .current_dir(&setup.local_path)
            .args(["status", "--porcelain"])
            .output()?;
        println!(
            "LOCAL status:\n{}",
            String::from_utf8_lossy(&status_out.stdout)
        );
        let log_out = std::process::Command::new("git")
            .current_dir(&setup.local_path)
            .args(["--no-pager", "log", "--oneline", "-n", "5"])
            .output()?;
        println!("LOCAL log:\n{}", String::from_utf8_lossy(&log_out.stdout));
        let ls_out = std::process::Command::new("ls")
            .current_dir(&setup.local_path)
            .arg("-la")
            .output()?;
        println!("LOCAL ls:\n{}", String::from_utf8_lossy(&ls_out.stdout));
    }
    assert!(
        synced,
        "File was not synced after repairing repo state; likely stuck syncing"
    );

    watch_handle.abort();
    Ok(())
}

mod common;

use anyhow::Result;
use common::{abort_watch_task, TestRepoSetup};
use git_sync_rs::{watch_with_periodic_sync, SyncConfig, WatchConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[tokio::test]
async fn scoped_watch_ignores_events_outside_selected_paths() -> Result<()> {
    let setup = TestRepoSetup::new()?;
    setup.commit_file("README.md", "# Initial\n", "Initial commit")?;
    fs::create_dir_all(setup.local_path.join("sessions"))?;
    setup.commit_file("sessions/.keep", "", "Add sessions directory")?;
    setup.push()?;
    let second_clone = setup.create_second_clone("second")?;

    let watch_config = WatchConfig {
        debounce_ms: 100,
        min_interval_ms: 200,
        sync_on_start: false,
        watch_paths: vec![PathBuf::from("sessions")],
        ..Default::default()
    };
    let local_path = setup.local_path.clone();
    let watch_handle = tokio::spawn(async move {
        watch_with_periodic_sync(
            &local_path,
            SyncConfig {
                sync_new_files: true,
                skip_hooks: false,
                commit_message: Some("Scoped watch sync".to_string()),
                remote_name: "origin".to_string(),
                branch_name: "master".to_string(),
                conflict_branch: false,
                target_branch: None,
            },
            watch_config,
            None,
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(500)).await;
    fs::write(setup.local_path.join("outside.txt"), "outside\n")?;
    tokio::time::sleep(Duration::from_millis(800)).await;
    setup.pull_in(&second_clone)?;
    assert!(
        !second_clone.join("outside.txt").exists(),
        "an event outside the configured watch paths triggered a sync"
    );

    fs::write(setup.local_path.join("sessions/inside.txt"), "inside\n")?;
    wait_for_file(&setup, &second_clone, "sessions/inside.txt").await?;

    abort_watch_task(watch_handle).await;
    Ok(())
}

async fn wait_for_file(setup: &TestRepoSetup, clone_path: &Path, filename: &str) -> Result<()> {
    for _ in 0..50 {
        let _ = setup.pull_in(clone_path);
        if clone_path.join(filename).exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    setup.pull_in(clone_path)?;
    anyhow::ensure!(
        clone_path.join(filename).exists(),
        "{filename} was not synced"
    );
    Ok(())
}

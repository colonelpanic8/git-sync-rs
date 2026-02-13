mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig, SyncError};
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn install_failing_pre_commit_hook(repo_path: &std::path::Path) -> Result<()> {
    let hook_path = repo_path.join(".git/hooks/pre-commit");
    fs::write(
        &hook_path,
        "#!/bin/sh\n\necho 'pre-commit failed from test' >&2\nexit 1\n",
    )?;
    let mut perms = fs::metadata(&hook_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&hook_path, perms)?;
    Ok(())
}

fn sync_config(skip_hooks: bool) -> SyncConfig {
    SyncConfig {
        sync_new_files: true,
        skip_hooks,
        commit_message: Some("Hook behavior test".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    }
}

#[test]
fn failing_pre_commit_hook_blocks_sync_when_skip_hooks_is_false() -> Result<()> {
    let setup = TestRepoSetup::new()?;
    setup.commit_file("tracked.txt", "v1\n", "initial")?;
    setup.push()?;
    install_failing_pre_commit_hook(&setup.local_path)?;

    fs::write(setup.local_path.join("tracked.txt"), "v2\n")?;

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config(false))?;
    let result = synchronizer.sync(false);

    assert!(
        matches!(result, Err(SyncError::HookRejected { .. })),
        "Expected hook rejection when skip_hooks=false, got: {result:?}"
    );

    Ok(())
}

#[test]
fn failing_pre_commit_hook_is_bypassed_when_skip_hooks_is_true() -> Result<()> {
    let setup = TestRepoSetup::new()?;
    setup.commit_file("tracked.txt", "v1\n", "initial")?;
    setup.push()?;
    install_failing_pre_commit_hook(&setup.local_path)?;
    let second_clone = setup.create_second_clone("second")?;

    fs::write(setup.local_path.join("tracked.txt"), "v2\n")?;

    let mut synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config(true))?;
    synchronizer.sync(false)?;

    setup.pull_in(&second_clone)?;
    setup.assert_file_content_in(&second_clone, "tracked.txt", "v2\n")?;

    Ok(())
}

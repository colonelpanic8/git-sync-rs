mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};

#[test]
fn gitignore_respect() -> Result<()> {
    // Setup repositories
    let setup = TestRepoSetup::new()?;

    // Create gitignore
    common::create_gitignore(&setup.local_path, &["*.log", "temp/", "secret.txt"])?;
    setup.commit_file(".gitignore", "*.log\ntemp/\nsecret.txt\n", "Add gitignore")?;
    setup.push()?;

    // Create ignored files
    std::fs::write(setup.local_path.join("test.log"), "Log content\n")?;
    std::fs::write(setup.local_path.join("secret.txt"), "Secret data\n")?;
    std::fs::create_dir_all(setup.local_path.join("temp"))?;
    std::fs::write(
        setup.local_path.join("temp").join("file.txt"),
        "Temp file\n",
    )?;

    // Create a non-ignored file
    std::fs::write(setup.local_path.join("normal.txt"), "Normal file\n")?;

    // Sync with auto-commit
    let sync_config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Auto-commit: {hostname} at {timestamp}".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
    };

    let synchronizer =
        RepositorySynchronizer::new_with_detected_branch(&setup.local_path, sync_config)?;
    synchronizer.sync(false)?;

    // Create a second clone and verify only non-ignored files were synced
    let second_clone = setup.create_second_clone("second")?;

    // Normal file should be synced
    setup.assert_file_content_in(&second_clone, "normal.txt", "Normal file\n")?;

    // Ignored files should not exist in the second clone
    assert!(
        !second_clone.join("test.log").exists(),
        "Log file should not be synced"
    );
    assert!(
        !second_clone.join("secret.txt").exists(),
        "Secret file should not be synced"
    );
    assert!(
        !second_clone.join("temp").exists(),
        "Temp directory should not be synced"
    );

    Ok(())
}

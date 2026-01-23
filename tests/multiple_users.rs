mod common;

use anyhow::Result;
use common::TestRepoSetup;
use git_sync_rs::{RepositorySynchronizer, SyncConfig};

#[test]
fn multiple_users_concurrent_changes() -> Result<()> {
    let setup = TestRepoSetup::new()?;

    // Initial setup
    setup.commit_file("shared.txt", "Initial\n", "Initial")?;
    setup.push()?;

    // Create multiple clones
    let user2 = setup.create_second_clone("user2")?;
    let user3 = setup.create_second_clone("user3")?;

    // Everyone makes changes
    setup.commit_file("file1.txt", "User1 content\n", "User1 adds file")?;
    setup.commit_file_in(&user2, "file2.txt", "User2 content\n", "User2 adds file")?;
    setup.commit_file_in(&user3, "file3.txt", "User3 content\n", "User3 adds file")?;

    // User2 pushes first
    setup.push_from(&user2)?;

    // User1 syncs (should pull user2's changes and push own)
    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: Some("Sync user1".to_string()),
        remote_name: "origin".to_string(),
        branch_name: "master".to_string(),
        conflict_branch: false,
        target_branch: None,
    };

    let mut sync = RepositorySynchronizer::new_with_detected_branch(&setup.local_path, config.clone())?;
    sync.sync(false)?;

    // User3 tries to push (should fail, needs to pull first)
    let push_result = setup.push_from(&user3);
    assert!(
        push_result.is_err(),
        "Direct push should fail due to diverged branches"
    );

    // User3 syncs
    let mut sync3 = RepositorySynchronizer::new_with_detected_branch(&user3, config)?;
    sync3.sync(false)?;

    // Everyone should have all files now
    setup.pull()?;
    setup.pull_in(&user2)?;

    setup.assert_file_content("file1.txt", "User1 content\n")?;
    setup.assert_file_content("file2.txt", "User2 content\n")?;
    setup.assert_file_content("file3.txt", "User3 content\n")?;

    setup.assert_file_content_in(&user2, "file1.txt", "User1 content\n")?;
    setup.assert_file_content_in(&user2, "file3.txt", "User3 content\n")?;

    Ok(())
}

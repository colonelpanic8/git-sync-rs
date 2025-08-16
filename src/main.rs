use anyhow::Result;
use git_sync_rs::{RepositorySynchronizer, SyncConfig, SyncError};
use std::env;
use std::process;
use tracing::{error, info};
use tracing_subscriber;

fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    if let Err(e) = run() {
        error!("Error: {}", e);

        // Exit with appropriate code
        let exit_code = if let Some(sync_error) = e.downcast_ref::<SyncError>() {
            sync_error.exit_code()
        } else {
            2
        };

        process::exit(exit_code);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    // Simple argument parsing for now
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("sync");
    let check_only = mode == "check";

    // Get repository path (current directory for now)
    let repo_path = env::current_dir()?;

    // Create config (hardcoded for testing, will be loaded from files later)
    let config = SyncConfig {
        sync_new_files: true,
        skip_hooks: false,
        commit_message: None,
        remote_name: "origin".to_string(),
        branch_name: String::new(), // Will be auto-detected
    };

    // Create synchronizer with auto-detected branch
    let synchronizer = RepositorySynchronizer::new_with_detected_branch(&repo_path, config)?;

    // Get current branch and update config if needed
    let current_branch = synchronizer.get_current_branch()?;
    info!("Current branch: {}", current_branch);

    // Check repository state
    let repo_state = synchronizer.get_repository_state()?;
    info!("Repository state: {:?}", repo_state);

    // Check sync state
    let sync_state = synchronizer.get_sync_state()?;
    info!("Sync state: {:?}", sync_state);

    // Perform sync
    synchronizer.sync(check_only)?;

    if check_only {
        println!("Check passed - repository is ready to sync");
    } else {
        println!("Sync completed successfully");
    }

    Ok(())
}

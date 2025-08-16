use anyhow::Result;
use clap::{Parser, Subcommand};
use git_sync_rs::{
    watch_with_periodic_sync, ConfigLoader, RepositorySynchronizer, SyncConfig, SyncError,
    WatchConfig,
};
use std::env;
use std::path::Path;
use std::process;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "git-sync-rs")]
#[command(about = "Automatically sync git repositories", long_about = None)]
struct Cli {
    /// Repository path to sync (defaults to current directory)
    #[arg(value_name = "PATH")]
    path: Option<String>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Suppress non-error output
    #[arg(short, long, global = true)]
    quiet: bool,

    /// Sync new/untracked files (compatible with original git-sync -n flag)
    #[arg(short = 'n', long, global = true)]
    new_files: Option<bool>,

    /// Force sync even if not configured (compatible with original git-sync -s flag)
    #[arg(short = 's', long, global = true)]
    sync_anyway: bool,

    /// Dry run mode - detect changes but don't sync (for default watch mode)
    #[arg(long, global = true)]
    dry_run: bool,

    /// Remote name to sync with
    #[arg(short = 'r', long, global = true)]
    remote: Option<String>,

    /// Repository path (overrides positional PATH)
    #[arg(short = 'd', long, global = true)]
    directory: Option<String>,

    /// Use alternate config file
    #[arg(long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Perform one-time synchronization
    Sync {
        /// Only check if sync is possible
        #[arg(long)]
        check_only: bool,
    },

    /// Verify repository is ready to sync
    Check,

    /// Start watching mode with automatic sync (default)
    Watch {
        /// Debounce period in seconds (can use decimals like 0.5)
        #[arg(long, default_value = "0.5")]
        debounce: f64,

        /// Minimum interval between syncs in seconds
        #[arg(long, default_value = "1")]
        min_interval: f64,

        /// Periodic sync interval in seconds (optional)
        #[arg(long)]
        interval: Option<u64>,

        /// Don't sync on startup
        #[arg(long)]
        no_initial_sync: bool,
    },

    /// Initialize config file with example
    Init {
        /// Force overwrite existing config
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize logging (default to INFO unless overridden)
    let filter_level = if cli.verbose {
        tracing::Level::DEBUG
    } else if cli.quiet {
        tracing::Level::ERROR
    } else {
        tracing::Level::INFO
    };

    // Use RUST_LOG env var if set, otherwise use our calculated level
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter_level.to_string()));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    if let Err(e) = run(cli).await {
        error!("Error: {}", e);

        // Exit with appropriate code
        let exit_code = if let Some(sync_error) = e.downcast_ref::<SyncError>() {
            sync_error.exit_code()
        } else {
            1
        };

        process::exit(exit_code);
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Handle init command first (doesn't need repo)
    if let Some(Commands::Init { force }) = &cli.command {
        return init_config(*force);
    }

    // Get repository path (priority: -d flag > positional arg > env var > config)
    let repo_path = if let Some(dir) = cli.directory {
        Some(std::path::PathBuf::from(
            shellexpand::tilde(&dir).to_string(),
        ))
    } else if let Some(path) = cli.path {
        Some(std::path::PathBuf::from(
            shellexpand::tilde(&path).to_string(),
        ))
    } else if let Ok(dir) = env::var("GIT_SYNC_DIRECTORY") {
        Some(std::path::PathBuf::from(
            shellexpand::tilde(&dir).to_string(),
        ))
    } else {
        None
    };

    // Create config loader
    let mut loader = ConfigLoader::new();
    if let Some(config_path) = cli.config {
        loader = loader.with_config_path(config_path);
    }

    // If no repo path specified, check if we should run on multiple repos from config
    let repo_path = match repo_path {
        Some(path) => path,
        None => {
            // For now, error if no directory specified
            // TODO: In the future, could support running on all configured repos
            return Err(anyhow::anyhow!(
                "No repository specified. Use -d, provide a path, or set GIT_SYNC_DIRECTORY"
            ));
        }
    };

    // Ensure the repository exists (clone if needed)
    ensure_repository_exists(&repo_path)?;

    // Log which repository we're working with
    info!("Working with repository: {}", repo_path.display());

    // Load configuration with proper precedence:
    // CLI args > env vars > config file > defaults
    let sync_config = loader.to_sync_config(
        &repo_path,
        cli.new_files, // CLI override for new_files
        cli.remote,    // CLI override for remote
    )?;

    // Handle commands
    match cli.command {
        Some(Commands::Check) => run_check(&repo_path, sync_config).await,
        None => {
            // Default to watch if no command specified
            let config = loader.load()?;

            let watch_config = WatchConfig {
                debounce_ms: 500,      // Default 0.5 seconds
                min_interval_ms: 1000, // Default 1 second
                sync_on_start: true,
                dry_run: cli.dry_run, // Use global dry_run flag
            };

            let interval_ms = Some(config.defaults.sync_interval * 1000);

            if cli.dry_run {
                info!("Starting watch mode in DRY RUN mode (default)");
            } else {
                info!("Starting watch mode (default)");
            }
            watch_with_periodic_sync(&repo_path, sync_config, watch_config, interval_ms)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        }
        Some(Commands::Sync { check_only }) => {
            if check_only {
                run_check(&repo_path, sync_config).await
            } else {
                run_sync(&repo_path, sync_config).await
            }
        }
        Some(Commands::Watch {
            debounce,
            min_interval,
            interval,
            no_initial_sync,
        }) => {
            // Load config once
            let config = loader.load()?;

            let watch_config = WatchConfig {
                debounce_ms: (debounce * 1000.0) as u64,
                min_interval_ms: (min_interval * 1000.0) as u64,
                sync_on_start: !no_initial_sync,
                dry_run: cli.dry_run, // Use global dry_run flag
            };

            // Use interval from CLI or defaults (repo config would need separate loading)
            let interval_ms = interval
                .or(Some(config.defaults.sync_interval))
                .map(|secs| secs * 1000);

            if cli.dry_run {
                info!(
                    "Starting watch mode in DRY RUN mode - changes will be detected but not synced"
                );
            } else {
                info!("Starting watch mode");
            }
            watch_with_periodic_sync(&repo_path, sync_config, watch_config, interval_ms)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        }
        Some(Commands::Init { .. }) => {
            // Already handled above
            unreachable!()
        }
    }
}

async fn run_check(repo_path: &std::path::Path, config: SyncConfig) -> Result<()> {
    // Create synchronizer with auto-detected branch
    let synchronizer = RepositorySynchronizer::new_with_detected_branch(repo_path, config)?;

    // Get current branch
    let current_branch = synchronizer.get_current_branch()?;
    info!("Current branch: {}", current_branch);

    // Check repository state
    let repo_state = synchronizer.get_repository_state()?;
    info!("Repository state: {:?}", repo_state);

    // Check sync state
    let sync_state = synchronizer.get_sync_state()?;
    info!("Sync state: {:?}", sync_state);

    // Run check
    synchronizer.sync(true)?;

    println!("Check passed - repository is ready to sync");
    Ok(())
}

async fn run_sync(repo_path: &std::path::Path, config: SyncConfig) -> Result<()> {
    // Create synchronizer with auto-detected branch
    let synchronizer = RepositorySynchronizer::new_with_detected_branch(repo_path, config)?;

    // Get current branch
    let current_branch = synchronizer.get_current_branch()?;
    info!("Current branch: {}", current_branch);

    // Check repository state
    let repo_state = synchronizer.get_repository_state()?;
    info!("Repository state: {:?}", repo_state);

    // Check sync state
    let sync_state = synchronizer.get_sync_state()?;
    info!("Sync state: {:?}", sync_state);

    // Run sync
    synchronizer.sync(false)?;

    println!("Sync completed successfully");
    Ok(())
}

fn init_config(force: bool) -> Result<()> {
    use directories::ProjectDirs;
    use std::fs;

    let project_dirs = ProjectDirs::from("", "", "git-sync-rs")
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory"))?;

    let config_dir = project_dirs.config_dir();
    let config_path = config_dir.join("config.toml");

    // Check if file exists
    if config_path.exists() && !force {
        return Err(anyhow::anyhow!(
            "Config file already exists at {:?}. Use --force to overwrite.",
            config_path
        ));
    }

    // Create directory if needed
    fs::create_dir_all(config_dir)?;

    // Write example config
    let example = git_sync_rs::config::create_example_config();
    fs::write(&config_path, example)?;

    println!("Created config file at {:?}", config_path);
    println!("Edit this file to configure your repositories.");

    Ok(())
}

/// Clone repository if it doesn't exist and GIT_SYNC_REPOSITORY is set
fn ensure_repository_exists(repo_path: &Path) -> Result<()> {
    // Check if the directory exists
    if repo_path.exists() {
        // Directory exists, check if it's a git repo
        if repo_path.join(".git").exists() {
            return Ok(()); // Already a git repository
        } else {
            return Err(anyhow::anyhow!(
                "Directory {:?} exists but is not a git repository",
                repo_path
            ));
        }
    }

    // Directory doesn't exist, check for GIT_SYNC_REPOSITORY
    let repo_url = match env::var("GIT_SYNC_REPOSITORY") {
        Ok(url) => url,
        Err(_) => {
            return Err(anyhow::anyhow!(
                "Directory {:?} does not exist. Set GIT_SYNC_REPOSITORY to clone a repository.",
                repo_path
            ));
        }
    };

    info!("Cloning repository from {} to {:?}", repo_url, repo_path);

    // Create parent directory if needed
    if let Some(parent) = repo_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Clone the repository using git command
    let output = std::process::Command::new("git")
        .arg("clone")
        .arg(&repo_url)
        .arg(repo_path)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Failed to clone repository: {}", stderr));
    }

    info!("Successfully cloned repository to {:?}", repo_path);

    // Set up push remote for the current branch (like the original script does)
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .arg("symbolic-ref")
        .arg("-q")
        .arg("HEAD")
        .output()?;

    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout);
        let branch = branch.trim().split('/').next_back().unwrap_or("main");

        // Set the pushRemote for the branch
        std::process::Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .arg("config")
            .arg("--add")
            .arg(format!("branch.{}.pushRemote", branch))
            .arg("origin")
            .output()?;

        info!("Configured push remote for branch {}", branch);
    }

    Ok(())
}

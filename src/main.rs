use anyhow::Result;
use clap::{Parser, Subcommand};
#[cfg(feature = "tray")]
use git_sync_rs::runtime_state::SuspensionStore;
#[cfg(feature = "tray")]
use git_sync_rs::watch::run_aggregate_tray;
use git_sync_rs::{
    watch_with_periodic_sync, Config, ConfigLoader, DefaultConfig, RepositoryConfig,
    RepositorySynchronizer, SyncConfig, SyncError, WatchConfig, WatchManager,
};
use std::env;
use std::path::{Path, PathBuf};
use std::process;
use tokio::task::JoinSet;
use tokio::time;
#[cfg(feature = "tray")]
use tracing::warn;
use tracing::{error, info};

const CLI_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (git commit ",
    env!("GIT_COMMIT_HASH"),
    ")"
);

#[derive(Parser)]
#[command(name = "git-sync-rs")]
#[command(version = CLI_VERSION, about = "Automatically sync git repositories", long_about = None)]
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
    #[arg(
        short = 'n',
        long,
        global = true,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    new_files: Option<bool>,

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
        #[arg(long)]
        debounce: Option<f64>,

        /// Minimum interval between syncs in seconds
        #[arg(long)]
        min_interval: Option<f64>,

        /// Periodic sync interval in seconds (optional)
        #[arg(long)]
        interval: Option<u64>,

        /// Don't sync on startup
        #[arg(long)]
        no_initial_sync: bool,

        /// Repository-relative path to watch (repeatable; defaults to the whole repository)
        #[arg(long = "watch-path", value_name = "PATH")]
        watch_paths: Vec<PathBuf>,

        /// Show system tray indicator (requires tray feature)
        #[cfg(feature = "tray")]
        #[arg(long)]
        tray: bool,

        /// Custom tray icon: a freedesktop icon name (e.g. "git") or path to an image file
        #[cfg(feature = "tray")]
        #[arg(long)]
        tray_icon: Option<String>,
    },

    /// Initialize config file with example
    Init {
        /// Force overwrite existing config
        #[arg(long)]
        force: bool,
    },

    /// Show version information
    Version,
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
    if let Some(Commands::Version) = &cli.command {
        return print_version();
    }

    // Get repository path (priority: -d flag > positional arg > env var > config)
    let repo_path = resolve_repo_path(&cli);

    // Create config loader
    let mut loader = ConfigLoader::new();
    if let Some(config_path) = &cli.config {
        loader = loader.with_config_path(config_path);
    }

    if let Some(repo_path) = repo_path {
        run_for_single_repo(&cli, &loader, repo_path).await
    } else {
        run_for_configured_repositories(&cli, &loader).await
    }
}

fn resolve_repo_path(cli: &Cli) -> Option<PathBuf> {
    if let Some(dir) = &cli.directory {
        Some(PathBuf::from(shellexpand::tilde(dir).to_string()))
    } else if let Some(path) = &cli.path {
        Some(PathBuf::from(shellexpand::tilde(path).to_string()))
    } else if let Ok(dir) = env::var("GIT_SYNC_DIRECTORY") {
        Some(PathBuf::from(shellexpand::tilde(&dir).to_string()))
    } else {
        None
    }
}

fn configured_repositories_for_command(
    config: &Config,
    command: &Option<Commands>,
) -> Vec<PathBuf> {
    match command {
        None | Some(Commands::Watch { .. }) => {
            let watched: Vec<PathBuf> = config
                .repositories
                .iter()
                .filter(|repo| repo.watch)
                .map(|repo| repo.path.clone())
                .collect();
            if watched.is_empty() {
                config
                    .repositories
                    .iter()
                    .map(|repo| repo.path.clone())
                    .collect()
            } else {
                watched
            }
        }
        Some(Commands::Check) | Some(Commands::Sync { .. }) => config
            .repositories
            .iter()
            .map(|repo| repo.path.clone())
            .collect(),
        Some(Commands::Init { .. }) | Some(Commands::Version) => vec![],
    }
}

fn resolve_watch_interval_ms(
    cli_interval: Option<u64>,
    repo_interval: Option<u64>,
    default_interval: u64,
) -> Option<u64> {
    cli_interval
        .or(repo_interval)
        .or(Some(default_interval))
        .map(|secs| secs * 1000)
}

fn resolve_repo_watch_config(
    mut watch_config: WatchConfig,
    defaults: &DefaultConfig,
    repo: &RepositoryConfig,
    cli_debounce: Option<f64>,
    cli_min_interval: Option<f64>,
    cli_no_initial_sync: bool,
    cli_watch_paths: &[PathBuf],
) -> WatchConfig {
    watch_config.debounce_ms =
        (cli_debounce.or(repo.debounce).unwrap_or(defaults.debounce) * 1000.0) as u64;
    watch_config.min_interval_ms = (cli_min_interval
        .or(repo.min_interval)
        .unwrap_or(defaults.min_interval)
        * 1000.0) as u64;
    watch_config.sync_on_start = if cli_no_initial_sync {
        false
    } else {
        repo.initial_sync.unwrap_or(defaults.initial_sync)
    };
    watch_config.watch_paths = if cli_watch_paths.is_empty() {
        repo.watch_paths.clone()
    } else {
        cli_watch_paths.to_vec()
    };
    watch_config
}

struct MultiRepoWatchOptions {
    cli_new_files: Option<bool>,
    cli_remote: Option<String>,
    watch_config: WatchConfig,
    cli_interval_secs: Option<u64>,
    cli_debounce: Option<f64>,
    cli_min_interval: Option<f64>,
    cli_no_initial_sync: bool,
    cli_watch_paths: Vec<PathBuf>,
}

async fn run_for_single_repo(cli: &Cli, loader: &ConfigLoader, repo_path: PathBuf) -> Result<()> {
    // Ensure the repository exists (clone if needed)
    let repo_config = loader.load_for_repo(&repo_path)?;
    ensure_repository_exists(&repo_path, repo_config.uri.as_deref())?;

    // Log which repository we're working with
    info!("Working with repository: {}", repo_path.display());

    // Load configuration with proper precedence:
    // CLI args > env vars > config file > defaults
    let sync_config = loader.to_sync_config(
        &repo_path,
        cli.new_files,      // CLI override for new_files
        cli.remote.clone(), // CLI override for remote
    )?;

    match &cli.command {
        Some(Commands::Check) => run_check(&repo_path, sync_config).await,
        None => {
            // Default to watch if no command specified
            let config = loader.load()?;
            let repo_config = loader.load_for_repo(&repo_path)?;

            #[cfg(feature = "tray")]
            let enable_tray = env::var("GIT_SYNC_TRAY").is_ok();
            #[cfg(not(feature = "tray"))]
            let enable_tray = false;

            let tray_icon = env::var("GIT_SYNC_TRAY_ICON").ok();

            let watch_config = resolve_repo_watch_config(
                WatchConfig {
                    dry_run: cli.dry_run,
                    enable_tray,
                    tray_icon,
                    ..WatchConfig::default()
                },
                &config.defaults,
                &repo_config,
                None,
                None,
                false,
                &[],
            );

            let interval_ms = resolve_watch_interval_ms(
                None,
                repo_config.interval,
                config.defaults.sync_interval,
            );

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
            if *check_only {
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
            watch_paths,
            #[cfg(feature = "tray")]
            tray,
            #[cfg(feature = "tray")]
            tray_icon,
        }) => {
            let config = loader.load()?;
            let repo_config = loader.load_for_repo(&repo_path)?;

            #[cfg(feature = "tray")]
            let enable_tray = *tray || env::var("GIT_SYNC_TRAY").is_ok();
            #[cfg(not(feature = "tray"))]
            let enable_tray = false;

            #[cfg(feature = "tray")]
            let tray_icon = tray_icon
                .clone()
                .or_else(|| env::var("GIT_SYNC_TRAY_ICON").ok());
            #[cfg(not(feature = "tray"))]
            let tray_icon: Option<String> = None;

            let watch_config = resolve_repo_watch_config(
                WatchConfig {
                    dry_run: cli.dry_run,
                    enable_tray,
                    tray_icon,
                    ..WatchConfig::default()
                },
                &config.defaults,
                &repo_config,
                *debounce,
                *min_interval,
                *no_initial_sync,
                watch_paths,
            );

            let interval_ms = resolve_watch_interval_ms(
                *interval,
                repo_config.interval,
                config.defaults.sync_interval,
            );

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
        Some(Commands::Init { .. }) | Some(Commands::Version) => unreachable!(),
    }
}

async fn run_for_configured_repositories(cli: &Cli, loader: &ConfigLoader) -> Result<()> {
    let config = loader.load()?;
    let repo_paths = configured_repositories_for_command(&config, &cli.command);

    if repo_paths.is_empty() {
        return Err(anyhow::anyhow!(
            "No repository specified and no repositories configured. Use -d, provide a path, set GIT_SYNC_DIRECTORY, or add [[repositories]] to config."
        ));
    }

    info!(
        "No explicit repository path provided; using {} configured repositories",
        repo_paths.len()
    );

    match &cli.command {
        Some(Commands::Check) => {
            for repo_path in &repo_paths {
                info!("Working with repository: {}", repo_path.display());
                let sync_config =
                    loader.to_sync_config(repo_path, cli.new_files, cli.remote.clone())?;
                let repo_config = loader.load_for_repo(repo_path)?;
                ensure_repository_exists(repo_path, repo_config.uri.as_deref())?;
                run_check(repo_path, sync_config).await?;
            }
            Ok(())
        }
        Some(Commands::Sync { check_only }) => {
            for repo_path in &repo_paths {
                info!("Working with repository: {}", repo_path.display());
                let sync_config =
                    loader.to_sync_config(repo_path, cli.new_files, cli.remote.clone())?;
                let repo_config = loader.load_for_repo(repo_path)?;
                ensure_repository_exists(repo_path, repo_config.uri.as_deref())?;
                if *check_only {
                    run_check(repo_path, sync_config).await?;
                } else {
                    run_sync(repo_path, sync_config).await?;
                }
            }
            Ok(())
        }
        None => {
            #[cfg(feature = "tray")]
            let enable_tray = env::var("GIT_SYNC_TRAY").is_ok();
            #[cfg(not(feature = "tray"))]
            let enable_tray = false;

            let tray_icon = env::var("GIT_SYNC_TRAY_ICON").ok();

            let watch_config = WatchConfig {
                dry_run: cli.dry_run,
                enable_tray,
                tray_icon,
                ..WatchConfig::default()
            };

            if cli.dry_run {
                info!(
                    "Starting watch mode in DRY RUN mode (default) across {} repositories",
                    repo_paths.len()
                );
            } else {
                info!(
                    "Starting watch mode (default) across {} repositories",
                    repo_paths.len()
                );
            }

            run_multi_repo_watch(
                repo_paths,
                loader,
                MultiRepoWatchOptions {
                    cli_new_files: cli.new_files,
                    cli_remote: cli.remote.clone(),
                    watch_config,
                    cli_interval_secs: None,
                    cli_debounce: None,
                    cli_min_interval: None,
                    cli_no_initial_sync: false,
                    cli_watch_paths: Vec::new(),
                },
            )
            .await
        }
        Some(Commands::Watch {
            debounce,
            min_interval,
            interval,
            no_initial_sync,
            watch_paths,
            #[cfg(feature = "tray")]
            tray,
            #[cfg(feature = "tray")]
            tray_icon,
        }) => {
            #[cfg(feature = "tray")]
            let enable_tray = *tray || env::var("GIT_SYNC_TRAY").is_ok();
            #[cfg(not(feature = "tray"))]
            let enable_tray = false;

            #[cfg(feature = "tray")]
            let tray_icon = tray_icon
                .clone()
                .or_else(|| env::var("GIT_SYNC_TRAY_ICON").ok());
            #[cfg(not(feature = "tray"))]
            let tray_icon: Option<String> = None;

            let watch_config = WatchConfig {
                dry_run: cli.dry_run,
                enable_tray,
                tray_icon,
                ..WatchConfig::default()
            };

            if cli.dry_run {
                info!(
                    "Starting watch mode in DRY RUN mode across {} repositories",
                    repo_paths.len()
                );
            } else {
                info!(
                    "Starting watch mode across {} repositories",
                    repo_paths.len()
                );
            }

            run_multi_repo_watch(
                repo_paths,
                loader,
                MultiRepoWatchOptions {
                    cli_new_files: cli.new_files,
                    cli_remote: cli.remote.clone(),
                    watch_config,
                    cli_interval_secs: *interval,
                    cli_debounce: *debounce,
                    cli_min_interval: *min_interval,
                    cli_no_initial_sync: *no_initial_sync,
                    cli_watch_paths: watch_paths.clone(),
                },
            )
            .await
        }
        Some(Commands::Init { .. }) | Some(Commands::Version) => unreachable!(),
    }
}

async fn run_multi_repo_watch(
    repo_paths: Vec<PathBuf>,
    loader: &ConfigLoader,
    options: MultiRepoWatchOptions,
) -> Result<()> {
    let mut join_set = JoinSet::new();
    let defaults = loader.load()?.defaults;
    #[cfg(feature = "tray")]
    let aggregate_tray_enabled = options.watch_config.enable_tray;
    #[cfg(feature = "tray")]
    let mut controls = Vec::new();
    #[cfg(feature = "tray")]
    let suspension_store = if aggregate_tray_enabled {
        match SuspensionStore::load_default() {
            Ok(store) => {
                info!(path = %store.path().display(), "Loaded persistent suspension state");
                Some(store)
            }
            Err(error) => {
                warn!(%error, "Unable to load persistent suspension state; continuing with in-memory state");
                None
            }
        }
    } else {
        None
    };

    for repo_path in repo_paths {
        info!("Watching repository: {}", repo_path.display());
        let sync_config = loader.to_sync_config(
            &repo_path,
            options.cli_new_files,
            options.cli_remote.clone(),
        )?;
        let repo_config = loader.load_for_repo(&repo_path)?;
        let interval_ms = resolve_watch_interval_ms(
            options.cli_interval_secs,
            repo_config.interval,
            defaults.sync_interval,
        );
        let repo_watch_config = resolve_repo_watch_config(
            options.watch_config.clone(),
            &defaults,
            &repo_config,
            options.cli_debounce,
            options.cli_min_interval,
            options.cli_no_initial_sync,
            &options.cli_watch_paths,
        );

        #[cfg(feature = "tray")]
        if aggregate_tray_enabled {
            let mut controlled_watch_config = repo_watch_config;
            controlled_watch_config.enable_tray = false;
            controlled_watch_config.periodic_sync_interval_ms = interval_ms;
            let manager = WatchManager::new(&repo_path, sync_config, controlled_watch_config);
            let manager = if let Some(store) = suspension_store.clone() {
                manager.with_suspension_store(store)
            } else {
                manager
            };
            let control = manager.control_named(repo_config.name.clone());
            controls.push(control.clone());
            let repo_uri = repo_config.uri.clone();
            join_set.spawn(async move {
                loop {
                    if control.is_shutdown_requested() {
                        return Ok(());
                    }
                    if let Err(error) = ensure_repository_exists(&repo_path, repo_uri.as_deref()) {
                        control.set_watch_error(error.to_string()).await;
                        time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                    match manager.watch().await {
                        Ok(()) => return Ok(()),
                        Err(error) => {
                            control.set_watch_error(error.to_string()).await;
                            error!(repo = %repo_path.display(), %error, "Repository watcher failed; retrying");
                            time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            });
            continue;
        }

        let repo_uri = repo_config.uri.clone();
        let mut supervised_watch_config = repo_watch_config;
        supervised_watch_config.periodic_sync_interval_ms = interval_ms;
        let manager = WatchManager::new(&repo_path, sync_config, supervised_watch_config);
        join_set.spawn(async move {
            loop {
                if let Err(error) = ensure_repository_exists(&repo_path, repo_uri.as_deref()) {
                    error!(repo = %repo_path.display(), %error, "Repository unavailable; retrying");
                    time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                match manager.watch().await {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        error!(repo = %repo_path.display(), %error, "Repository watcher failed; retrying");
                        time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
        });
    }

    #[cfg(feature = "tray")]
    if aggregate_tray_enabled {
        let tray_icon = options.watch_config.tray_icon.clone();
        join_set.spawn(async move {
            run_aggregate_tray(controls, tray_icon)
                .await
                .map_err(anyhow::Error::from)
        });
    }

    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                join_set.abort_all();
                return Err(e);
            }
            Err(e) => {
                join_set.abort_all();
                return Err(anyhow::anyhow!("Watch task failed: {}", e));
            }
        }
    }

    Ok(())
}

fn print_version() -> Result<()> {
    println!("git-sync-rs {CLI_VERSION}");
    Ok(())
}

async fn run_check(repo_path: &std::path::Path, config: SyncConfig) -> Result<()> {
    // Create synchronizer with auto-detected branch
    let mut synchronizer = RepositorySynchronizer::new_with_detected_branch(repo_path, config)?;

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
    let mut synchronizer = RepositorySynchronizer::new_with_detected_branch(repo_path, config)?;

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
fn ensure_repository_exists(repo_path: &Path, configured_uri: Option<&str>) -> Result<()> {
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
    let repo_url = match configured_uri
        .map(str::to_owned)
        .or_else(|| env::var("GIT_SYNC_REPOSITORY").ok())
    {
        Some(url) => url,
        None => {
            return Err(anyhow::anyhow!(
                "Directory {:?} does not exist. Configure its uri or set GIT_SYNC_REPOSITORY to clone it.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use git_sync_rs::{DefaultConfig, RepositoryConfig};

    #[cfg(feature = "tray")]
    fn watch_command() -> Option<Commands> {
        Some(Commands::Watch {
            debounce: Some(0.5),
            min_interval: Some(1.0),
            interval: None,
            no_initial_sync: false,
            watch_paths: Vec::new(),
            tray: false,
            tray_icon: None,
        })
    }

    #[cfg(not(feature = "tray"))]
    fn watch_command() -> Option<Commands> {
        Some(Commands::Watch {
            debounce: Some(0.5),
            min_interval: Some(1.0),
            interval: None,
            no_initial_sync: false,
            watch_paths: Vec::new(),
        })
    }

    fn config_with_repos(repos: Vec<RepositoryConfig>) -> Config {
        Config {
            defaults: DefaultConfig::default(),
            repositories: repos,
        }
    }

    fn repo(path: &str, watch: bool) -> RepositoryConfig {
        RepositoryConfig {
            path: PathBuf::from(path),
            watch,
            ..RepositoryConfig::default()
        }
    }

    #[test]
    fn watch_selects_only_watch_enabled_repositories_when_any_are_marked() {
        let config = config_with_repos(vec![repo("/tmp/repo-a", true), repo("/tmp/repo-b", false)]);

        let selected = configured_repositories_for_command(&config, &watch_command());
        assert_eq!(selected, vec![PathBuf::from("/tmp/repo-a")]);
    }

    #[test]
    fn default_command_uses_same_watch_repository_selection() {
        let config = config_with_repos(vec![repo("/tmp/repo-a", true), repo("/tmp/repo-b", false)]);

        let selected = configured_repositories_for_command(&config, &None);
        assert_eq!(selected, vec![PathBuf::from("/tmp/repo-a")]);
    }

    #[test]
    fn watch_selects_all_repositories_when_none_are_marked() {
        let config =
            config_with_repos(vec![repo("/tmp/repo-a", false), repo("/tmp/repo-b", false)]);

        let selected = configured_repositories_for_command(&config, &watch_command());
        assert_eq!(
            selected,
            vec![PathBuf::from("/tmp/repo-a"), PathBuf::from("/tmp/repo-b")]
        );
    }

    #[test]
    fn check_selects_all_configured_repositories() {
        let config = config_with_repos(vec![repo("/tmp/repo-a", true), repo("/tmp/repo-b", false)]);

        let selected = configured_repositories_for_command(&config, &Some(Commands::Check));
        assert_eq!(
            selected,
            vec![PathBuf::from("/tmp/repo-a"), PathBuf::from("/tmp/repo-b")]
        );
    }

    #[test]
    fn watch_interval_precedence_is_cli_then_repo_then_default() {
        assert_eq!(resolve_watch_interval_ms(Some(5), Some(10), 20), Some(5000));
        assert_eq!(resolve_watch_interval_ms(None, Some(10), 20), Some(10000));
        assert_eq!(resolve_watch_interval_ms(None, None, 20), Some(20000));
    }

    #[test]
    fn repository_watch_settings_override_defaults_and_cli_wins() {
        let defaults = DefaultConfig::default();
        let repository = RepositoryConfig {
            path: PathBuf::from("/tmp/repo"),
            debounce: Some(2.0),
            min_interval: Some(30.0),
            initial_sync: Some(false),
            watch_paths: vec![PathBuf::from("history")],
            ..RepositoryConfig::default()
        };

        let resolved = resolve_repo_watch_config(
            WatchConfig::default(),
            &defaults,
            &repository,
            None,
            Some(5.0),
            false,
            &[],
        );
        assert_eq!(resolved.debounce_ms, 2000);
        assert_eq!(resolved.min_interval_ms, 5000);
        assert!(!resolved.sync_on_start);
        assert_eq!(resolved.watch_paths, vec![PathBuf::from("history")]);
    }
}

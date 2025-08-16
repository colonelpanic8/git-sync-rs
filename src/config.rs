use crate::error::{Result, SyncError};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Complete configuration for git-sync-rs
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub defaults: DefaultConfig,

    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,
}

/// Default configuration values
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DefaultConfig {
    #[serde(default = "default_sync_interval")]
    pub sync_interval: u64, // seconds

    #[serde(default = "default_sync_new_files")]
    pub sync_new_files: bool,

    #[serde(default)]
    pub skip_hooks: bool,

    #[serde(default = "default_commit_message")]
    pub commit_message: String,

    #[serde(default = "default_remote")]
    pub remote: String,
}

impl Default for DefaultConfig {
    fn default() -> Self {
        Self {
            sync_interval: default_sync_interval(),
            sync_new_files: default_sync_new_files(),
            skip_hooks: false,
            commit_message: default_commit_message(),
            remote: default_remote(),
        }
    }
}

/// Repository-specific configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RepositoryConfig {
    pub path: PathBuf,

    #[serde(default)]
    pub sync_new_files: Option<bool>,

    #[serde(default)]
    pub skip_hooks: Option<bool>,

    #[serde(default)]
    pub commit_message: Option<String>,

    #[serde(default)]
    pub remote: Option<String>,

    #[serde(default)]
    pub branch: Option<String>,

    #[serde(default)]
    pub watch: bool,

    #[serde(default)]
    pub interval: Option<u64>, // seconds
}

// Default value functions for serde
fn default_sync_interval() -> u64 {
    60
}

fn default_sync_new_files() -> bool {
    true
}

fn default_commit_message() -> String {
    "changes from {hostname} on {timestamp}".to_string()
}

fn default_remote() -> String {
    "origin".to_string()
}

/// Configuration loader that merges multiple sources with correct precedence
pub struct ConfigLoader {
    config_path: Option<PathBuf>,
    cached_config: std::cell::RefCell<Option<Config>>,
}

impl ConfigLoader {
    /// Create a new config loader
    pub fn new() -> Self {
        Self {
            config_path: None,
            cached_config: std::cell::RefCell::new(None),
        }
    }

    /// Set explicit config file path
    pub fn with_config_path(mut self, path: impl AsRef<Path>) -> Self {
        self.config_path = Some(path.as_ref().to_path_buf());
        self
    }
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigLoader {
    /// Load configuration from all sources and merge with correct precedence
    pub fn load(&self) -> Result<Config> {
        // Check cache first
        if let Some(cached) = self.cached_config.borrow().as_ref() {
            return Ok(cached.clone());
        }

        // Start with defaults
        let mut config = Config::default();

        // Layer 1: Load from TOML config file (lowest priority)
        if let Some(toml_config) = self.load_toml_config()? {
            debug!("Loaded TOML configuration");
            config = toml_config;
        }

        // Layer 2: Apply environment variables (medium priority)
        self.apply_env_vars(&mut config);

        // Note: Command-line args are applied in main.rs (highest priority)

        // Cache the result
        *self.cached_config.borrow_mut() = Some(config.clone());

        Ok(config)
    }

    /// Load repository-specific config for a given path
    pub fn load_for_repo(&self, repo_path: &Path) -> Result<RepositoryConfig> {
        let config = self.load()?;

        // Find matching repository config
        let repo_config = config
            .repositories
            .into_iter()
            .find(|r| r.path == repo_path)
            .unwrap_or_else(|| {
                // Create default config for this repo
                RepositoryConfig {
                    path: repo_path.to_path_buf(),
                    sync_new_files: None,
                    skip_hooks: None,
                    commit_message: None,
                    remote: None,
                    branch: None,
                    watch: false,
                    interval: None,
                }
            });

        Ok(repo_config)
    }

    /// Convert to SyncConfig for the synchronizer
    pub fn to_sync_config(
        &self,
        repo_path: &Path,
        cli_new_files: Option<bool>,
        cli_remote: Option<String>,
    ) -> Result<crate::sync::SyncConfig> {
        let config = self.load()?;
        let repo_config = self.load_for_repo(repo_path)?;

        // Merge with precedence: CLI > env > repo config > defaults
        Ok(crate::sync::SyncConfig {
            sync_new_files: cli_new_files
                .or(env::var("GIT_SYNC_NEW_FILES")
                    .ok()
                    .and_then(|v| v.parse().ok()))
                .or(repo_config.sync_new_files)
                .unwrap_or(config.defaults.sync_new_files),

            skip_hooks: repo_config.skip_hooks.unwrap_or(config.defaults.skip_hooks),

            commit_message: repo_config
                .commit_message
                .or(Some(config.defaults.commit_message)),

            remote_name: cli_remote
                .or(env::var("GIT_SYNC_REMOTE").ok())
                .or(repo_config.remote)
                .unwrap_or(config.defaults.remote),

            branch_name: repo_config.branch.unwrap_or_default(), // Will be auto-detected
        })
    }

    /// Load TOML configuration file
    fn load_toml_config(&self) -> Result<Option<Config>> {
        let config_path = if let Some(path) = &self.config_path {
            // Use explicit path
            path.clone()
        } else {
            // Use default XDG path
            let project_dirs = ProjectDirs::from("", "", "git-sync-rs").ok_or_else(|| {
                SyncError::Other("Could not determine config directory".to_string())
            })?;

            project_dirs.config_dir().join("config.toml")
        };

        if !config_path.exists() {
            debug!("Config file not found at {:?}", config_path);
            return Ok(None);
        }

        info!("Loading config from {:?}", config_path);
        let contents = fs::read_to_string(&config_path)?;
        let mut config: Config = toml::from_str(&contents)
            .map_err(|e| SyncError::Other(format!("Failed to parse config: {}", e)))?;

        // Expand tildes in repository paths
        for repo in &mut config.repositories {
            let expanded = shellexpand::tilde(&repo.path.to_string_lossy()).to_string();
            repo.path = PathBuf::from(expanded);
        }

        Ok(Some(config))
    }

    /// Apply environment variables to config
    fn apply_env_vars(&self, config: &mut Config) {
        // GIT_SYNC_INTERVAL
        if let Ok(interval) = env::var("GIT_SYNC_INTERVAL") {
            if let Ok(secs) = interval.parse::<u64>() {
                debug!("Setting sync interval from env: {}s", secs);
                config.defaults.sync_interval = secs;
            }
        }

        // GIT_SYNC_NEW_FILES
        if let Ok(new_files) = env::var("GIT_SYNC_NEW_FILES") {
            if let Ok(enabled) = new_files.parse::<bool>() {
                debug!("Setting sync_new_files from env: {}", enabled);
                config.defaults.sync_new_files = enabled;
            }
        }

        // GIT_SYNC_REMOTE
        if let Ok(remote) = env::var("GIT_SYNC_REMOTE") {
            debug!("Setting remote from env: {}", remote);
            config.defaults.remote = remote;
        }

        // GIT_SYNC_COMMIT_MESSAGE
        if let Ok(msg) = env::var("GIT_SYNC_COMMIT_MESSAGE") {
            debug!("Setting commit message from env");
            config.defaults.commit_message = msg;
        }

        // GIT_SYNC_DIRECTORY - add as a repository if not already configured
        if let Ok(dir) = env::var("GIT_SYNC_DIRECTORY") {
            let expanded = shellexpand::tilde(&dir).to_string();
            let path = PathBuf::from(expanded);
            if !config.repositories.iter().any(|r| r.path == path) {
                debug!("Adding repository from GIT_SYNC_DIRECTORY env: {:?}", path);
                config.repositories.push(RepositoryConfig {
                    path,
                    sync_new_files: None,
                    skip_hooks: None,
                    commit_message: None,
                    remote: None,
                    branch: None,
                    watch: true, // Assume watch mode when using env var
                    interval: None,
                });
            }
        }
    }
}

/// Create an example config file
pub fn create_example_config() -> String {
    r#"# git-sync-rs configuration file

[defaults]
# Default sync interval in seconds (for watch mode)
sync_interval = 60

# Whether to sync untracked files by default
sync_new_files = true

# Skip git hooks when committing
skip_hooks = false

# Commit message template
# Available placeholders: {hostname}, {timestamp}
# {timestamp} format: YYYY-MM-DD HH:MM:SS AM/PM TZ (e.g., 2024-03-15 02:30:45 PM PST)
commit_message = "changes from {hostname} on {timestamp}"

# Default remote name
remote = "origin"

# Example repository configurations
[[repositories]]
path = "/home/user/notes"
sync_new_files = true
remote = "origin"
branch = "main"
watch = true
interval = 30  # Override sync interval for this repo

[[repositories]]
path = "/home/user/dotfiles"
sync_new_files = false
watch = true
# Uses defaults for other settings
"#
    .to_string()
}

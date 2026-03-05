# git-sync-rs

[![CI](https://github.com/colonelpanic8/git-sync-rs/workflows/CI/badge.svg)](https://github.com/colonelpanic8/git-sync-rs/actions)
[![Crates.io](https://img.shields.io/crates/v/git-sync-rs.svg)](https://crates.io/crates/git-sync-rs)
[![Documentation](https://docs.rs/git-sync-rs/badge.svg)](https://docs.rs/git-sync-rs)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

A Rust implementation of automatic git repository synchronization with file watching capabilities. This tool automatically commits, pushes, and pulls changes to keep one or many repositories in sync.

## Features

- 🔄 **Automatic synchronization** - Commits, fetches, pushes, and merges/rebases automatically
- 👀 **File watching** - Monitors file changes and triggers sync with debouncing
- 🚫 **Gitignore support** - Respects `.gitignore` patterns
- 📦 **Repository cloning** - Automatically clones repositories if they don't exist
- 🔐 **SSH authentication** - Works with SSH keys (with fallback to git command)
- 🗂️ **Multi-repository config** - Watch/sync multiple repositories from one config file
- 🌿 **Conflict fallback branch** - Optionally continue syncing on `git-sync/*` branches after conflicts
- ⚡ **Efficient** - Only syncs when changes are detected
- 🧪 **Dry-run mode** - Test your configuration without making changes
- 🔧 **Flexible configuration** - Configure via TOML, environment variables, or CLI

## Installation

```bash
cargo install git-sync-rs
```

Optional system tray support:

```bash
cargo install git-sync-rs --features tray
```

## Usage

### Basic Commands

```bash
# Check if repository is ready to sync
git-sync-rs /path/to/repo check

# Perform one-time sync
git-sync-rs /path/to/repo sync

# Validate whether sync can run without changing anything
git-sync-rs /path/to/repo sync --check-only

# Watch for changes and auto-sync
git-sync-rs /path/to/repo watch

# Watch with custom intervals
git-sync-rs /path/to/repo watch --debounce 2 --min-interval 1 --interval 300

# Create a starter config file at ~/.config/git-sync-rs/config.toml
git-sync-rs init

# Print version and commit hash
git-sync-rs version
```

### Environment Variables

Compatible with the original `git-sync` environment variables (plus `git-sync-rs` tray variables):

- `GIT_SYNC_DIRECTORY` - Repository path
- `GIT_SYNC_REPOSITORY` - Repository URL for initial clone
- `GIT_SYNC_INTERVAL` - Sync interval in seconds
- `GIT_SYNC_NEW_FILES` - Whether to add new files (true/false)
- `GIT_SYNC_REMOTE` - Remote name (default: origin)
- `GIT_SYNC_COMMIT_MESSAGE` - Custom commit message template
- `GIT_SYNC_TRAY` - Enable tray indicator in watch mode (when built with `tray` feature)
- `GIT_SYNC_TRAY_ICON` - Tray icon name/path (when built with `tray` feature)

### Watch Mode

Watch mode monitors your repository for changes and automatically syncs them:

```bash
# Basic watch mode
git-sync-rs /path/to/repo watch

# With custom debounce (wait 2 seconds after changes)
git-sync-rs /path/to/repo watch --debounce 2

# Require at least 3 seconds between sync runs
git-sync-rs /path/to/repo watch --min-interval 3

# With periodic sync every 5 minutes
git-sync-rs /path/to/repo watch --interval 300

# Skip the initial sync at startup
git-sync-rs /path/to/repo watch --no-initial-sync

# Dry run mode (detect changes but don't sync)
git-sync-rs /path/to/repo watch --dry-run

# Run watch mode for multiple repos from config
git-sync-rs watch
```

### Configuration File

Generate a config file with:

```bash
git-sync-rs init
```

Then edit `~/.config/git-sync-rs/config.toml`:

```toml
[defaults]
sync_interval = 60
sync_new_files = true
skip_hooks = false
commit_message = "changes from {hostname} on {timestamp}"
remote = "origin"
conflict_branch = false

[[repositories]]
path = "~/my-notes"
branch = "main"
watch = true
interval = 30

[[repositories]]
path = "~/my-docs"
remote = "backup"
conflict_branch = true
watch = true
```

When no repository path is passed:
- `git-sync-rs watch` (and default watch mode) runs all repositories with `watch = true` in `[[repositories]]`.
- If no repo has `watch = true`, watch mode runs all configured repositories.
- `git-sync-rs check` and `git-sync-rs sync` run across all configured repositories.

## Command Line Options

- `-n, --new-files` - Sync new/untracked files
- `-r, --remote <name>` - Specify remote name
- `-d, --directory <path>` - Repository path
- `--dry-run` - Detect changes but do not sync (watch mode)
- `-v, --verbose` - Enable verbose output
- `-q, --quiet` - Suppress non-error output
- `--config <path>` - Use alternate config file
- `check` - Verify repository is ready to sync
- `watch --debounce <seconds>` - Debounce window before syncing (default `0.5`)
- `watch --min-interval <seconds>` - Minimum time between sync attempts (default `1`)
- `watch --interval <seconds>` - Periodic sync interval
- `watch --no-initial-sync` - Do not sync immediately on startup
- `sync --check-only` - Run repository checks without mutating state
- `init [--force]` - Create example config file
- `version` - Print semantic version and git commit hash

## Compatibility

This tool is designed to be a drop-in replacement for `git-sync-on-inotify` with additional features and better performance.

## License

Licensed under either of:
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

at your option.

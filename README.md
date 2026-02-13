# git-sync-rs

[![CI](https://github.com/colonelpanic8/git-sync-rs/workflows/CI/badge.svg)](https://github.com/colonelpanic8/git-sync-rs/actions)
[![Crates.io](https://img.shields.io/crates/v/git-sync-rs.svg)](https://crates.io/crates/git-sync-rs)
[![Documentation](https://docs.rs/git-sync-rs/badge.svg)](https://docs.rs/git-sync-rs)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

A Rust implementation of automatic git repository synchronization with file watching capabilities. This tool automatically commits, pushes, and pulls changes to keep your repositories in sync.

## Features

- 🔄 **Automatic synchronization** - Commits, fetches, pushes, and merges/rebases automatically
- 👀 **File watching** - Monitors file changes and triggers sync with debouncing
- 🚫 **Gitignore support** - Respects `.gitignore` patterns
- 📦 **Repository cloning** - Automatically clones repositories if they don't exist
- 🔐 **SSH authentication** - Works with SSH keys (with fallback to git command)
- ⚡ **Efficient** - Only syncs when changes are detected
- 🧪 **Dry-run mode** - Test your configuration without making changes
- 🔧 **Flexible configuration** - Configure via TOML, environment variables, or CLI

## Installation

```bash
cargo install git-sync-rs
```

## Usage

### Basic Commands

```bash
# Check if repository is ready to sync
git-sync-rs /path/to/repo check

# Perform one-time sync
git-sync-rs /path/to/repo sync

# Watch for changes and auto-sync
git-sync-rs /path/to/repo watch

# Watch with custom intervals
git-sync-rs /path/to/repo watch --debounce 2 --interval 300

# Print version and commit hash
git-sync-rs version
```

### Environment Variables

Fully compatible with the original `git-sync` environment variables:

- `GIT_SYNC_DIRECTORY` - Repository path
- `GIT_SYNC_REPOSITORY` - Repository URL for initial clone
- `GIT_SYNC_INTERVAL` - Sync interval in seconds
- `GIT_SYNC_NEW_FILES` - Whether to add new files (true/false)
- `GIT_SYNC_REMOTE` - Remote name (default: origin)
- `GIT_SYNC_COMMIT_MESSAGE` - Custom commit message template

### Watch Mode

Watch mode monitors your repository for changes and automatically syncs them:

```bash
# Basic watch mode
git-sync-rs watch /path/to/repo

# With custom debounce (wait 2 seconds after changes)
git-sync-rs watch /path/to/repo --debounce 2

# With periodic sync every 5 minutes
git-sync-rs watch /path/to/repo --interval 300

# Dry run mode (detect changes but don't sync)
git-sync-rs watch /path/to/repo --dry-run
```

### Configuration File

Create a configuration file at `~/.config/git-sync-rs/config.toml`:

```toml
[defaults]
sync_interval = 300
sync_new_files = true
commit_message = "Auto-sync: {hostname} at {timestamp}"
remote_name = "origin"

[[repositories]]
path = "~/my-notes"
sync_new_files = true

[[repositories]]
path = "~/my-docs"
remote_name = "backup"
```

## Command Line Options

- `-n, --new-files` - Sync new/untracked files
- `-r, --remote <name>` - Specify remote name
- `-d, --directory <path>` - Repository path
- `-v, --verbose` - Enable verbose output
- `-q, --quiet` - Suppress non-error output
- `--config <path>` - Use alternate config file
- `version` - Print semantic version and git commit hash

## Compatibility

This tool is designed to be a drop-in replacement for `git-sync-on-inotify` with additional features and better performance.

## License

Licensed under either of:
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)

at your option.

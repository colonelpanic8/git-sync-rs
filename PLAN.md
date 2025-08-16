# git-sync-rs: Project Plan

## Vision
A Rust implementation of git-sync that automatically keeps git repositories synchronized with their remotes. This tool combines the core synchronization logic of git-sync with built-in file watching capabilities, providing a single, efficient binary for all synchronization needs.

## Core Philosophy
- **Safety First**: Never lose data, always fail gracefully with clear error messages
- **Simplicity**: Do one thing well - keep repos in sync
- **Transparency**: Clear error messages, predictable behavior, meaningful exit codes
- **Flexibility**: Multiple configuration sources, sensible defaults
- **Compatibility**: Smooth migration path from bash git-sync

## Key Learnings from Original git-sync

### Exit Code Strategy
The original uses meaningful exit codes that we should preserve:
- `0`: Success - repository is in sync
- `1`: Manual intervention required (conflicts, uncommitted changes)
- `2`: Repository in unsafe state (mid-rebase, detached HEAD, etc.)
- `3`: Network/transient failure (can retry)
- `100`: Invalid mode/arguments
- `128`: Not a git repository (matching git's error code)

### Safety Checks
Before any sync operation:
1. Verify we're in a git repository
2. Check repository state (no ongoing rebase/merge/cherry-pick/bisect)
3. Verify we're on a branch (not detached HEAD)
4. Check for unhandled file states
5. Ensure branch is configured for sync

### Configuration Precedence
1. Command-line flags (`-n`, `-s`)
2. Branch-specific git config (`branch.<name>.*`)
3. Global git-sync config (`git-sync.*`)
4. Environment variables (for watch mode)
5. Hardcoded defaults

### Unfinished Features from Original
- Stash push & pop mode for handling uncommitted changes
- Optional fetch/push operations
- Branch creation on rebase failure to preserve history

## Implementation Phases

### Phase 1: Core Synchronization Engine
**Goal**: Implement the fundamental git synchronization logic

**Key Components**:
- Repository state detection
  - Clean vs dirty working tree
  - Current branch detection
  - Ongoing operations (rebase, merge, etc.)
- Sync state analysis relative to remote
  - Equal: local and remote are the same
  - Ahead: local has commits remote doesn't
  - Behind: remote has commits local doesn't
  - Diverged: both have unique commits
- Auto-commit functionality
  - Configurable commit messages with hostname/timestamp
  - Option to include new files or only modified
  - Option to skip git hooks
- Git operations
  - Fetch from remote
  - Fast-forward merge when behind
  - Push when ahead
  - Rebase when diverged
- Error handling with appropriate exit codes

**Critical Decisions**:
- **git2 vs shell commands**: Start with git2 for better error handling, fall back to shell for complex operations?
- **Rebase strategy**: Default to rebase for diverged state (matching LFixoriginal)
- **Conflict handling**: Exit cleanly, require manual resolution

**Testing Strategy**:
- Unit tests for state detection
- Integration tests with real git repos
- Test all four sync states (equal, ahead, behind, diverged)

### Phase 2: Triggering Mechanisms
**Goal**: Implement ways to trigger synchronization

**Components**:
- File system watching
  - Use notify crate for cross-platform support
  - Respect .gitignore patterns
  - Debounce rapid changes (configurable interval)
- Periodic polling
  - Configurable interval (default from GIT_SYNC_INTERVAL)
  - Sync on timeout even without file changes
- Manual trigger
  - One-shot sync mode
  - Check mode (verify readiness without syncing)

**Key Behaviors from Original**:
- Initial sync on startup
- Sync on file change OR timeout (whichever comes first)
- Skip sync if changed file is git-ignored
- Clone repository if it doesn't exist (with GIT_SYNC_REPOSITORY)

**Technical Considerations**:
- Event coalescing for multiple rapid changes
- Handling watch events during active sync
- Resource usage for large repositories

### Phase 3: Configuration System
**Goal**: Flexible, hierarchical configuration

**Configuration Sources** (in order of precedence):
1. Command-line arguments
2. Environment variables
   - `GIT_SYNC_DIRECTORY`: Repository path
   - `GIT_SYNC_COMMAND`: Command to run (for compatibility)
   - `GIT_SYNC_INTERVAL`: Sync interval in seconds
   - `GIT_SYNC_REPOSITORY`: Remote URL for initial clone
3. TOML config file (XDG directories: `~/.config/git-sync-rs/config.toml`)
4. Git config values (for compatibility with original)
   - `branch.<name>.sync`: Enable sync for branch
   - `branch.<name>.syncNewFiles`: Include untracked files
   - `branch.<name>.syncSkipHooks`: Skip pre-commit hooks
   - `branch.<name>.syncCommitMsg`: Custom commit message
   - `branch.<name>.autocommitscript`: Custom commit command
   - `branch.<name>.pushRemote`: Remote to push to
   - `git-sync.syncEnabled`: Global sync enable
   - `git-sync.syncNewFiles`: Global new files setting
5. Sensible defaults

**TOML Config Schema (Draft)**:
```toml
[defaults]
sync_interval = 500  # milliseconds for watch mode
sync_new_files = false
skip_hooks = false
commit_message = "changes from {hostname} on {timestamp}"

[[repositories]]
path = "/home/user/notes"
sync_enabled = true
sync_new_files = true
remote = "origin"
branch = "main"

[[repositories]]
path = "/home/user/dotfiles"
sync_enabled = true
watch = true
interval = 1000
```

**Migration Strategy**:
- Read existing git config settings
- Provide migration command to generate TOML from git config
- Support both configuration methods simultaneously

### Phase 4: CLI Interface
**Goal**: User-friendly command-line interface using clap

**Commands**:
```
git-sync-rs [OPTIONS] [COMMAND]

COMMANDS:
    sync    Perform one-time synchronization (default)
    check   Verify repository is ready to sync
    watch   Start watching mode with automatic sync
    init    Initialize repository for syncing
    migrate Convert git-sync config to TOML

OPTIONS:
    -n, --new-files     Sync new/untracked files
    -s, --sync          Force sync even if not configured
    -v, --verbose       Increase verbosity
    -q, --quiet         Suppress non-error output
    --dry-run           Show what would be done
    --config PATH       Use alternate config file
```

**Output Design**:
- Prefix messages with "git-sync-rs:" (like original)
- Use stderr for errors, stdout for info
- Structured logging with levels
- Optional JSON output for scripting

## Technical Stack

### Core Dependencies
- `clap`: CLI argument parsing (industry standard)
- `git2`: Git operations (libgit2 bindings)
- `notify`: Cross-platform file watching
- `tokio`: Async runtime for watch mode
- `serde`/`toml`: Configuration management
- `directories`: XDG directory support
- `anyhow`/`thiserror`: Error handling

### Architecture Decisions
- **Modules vs Crates**: Start with modules, refactor to workspace if needed
- **Async Design**: Use async only for watch mode, sync operations remain synchronous
- **Error Handling**: Use `thiserror` for custom errors, `anyhow` for application errors
- **Logging**: Use `tracing` for structured logging

## Testing Strategy

### Unit Tests
- State detection functions
- Configuration loading and precedence
- Sync state analysis

### Integration Tests
- Full sync flow with test repositories
- All four sync states (equal, ahead, behind, diverged)
- Configuration file handling
- Watch mode with simulated file changes

### End-to-End Tests
- Migration from bash git-sync
- Compatibility with existing git config
- Network failure handling
- Concurrent sync prevention

## Success Criteria

### Functional Requirements
- ✅ Feature parity with bash git-sync
- ✅ Built-in watch mode (no separate script needed)
- ✅ Support all original configuration options
- ✅ Maintain exit code compatibility

### Non-Functional Requirements
- ✅ Single static binary
- ✅ Cross-platform (Linux, macOS, Windows)
- ✅ Better error messages than bash version
- ✅ Performance equal or better than original
- ✅ Memory usage under 50MB for watch mode

### Quality Metrics
- Test coverage > 80%
- No panics in normal operation
- Graceful handling of all error conditions
- Clear documentation with examples

## Future Enhancements (Post-v1.0)

### Nice to Have
- Multiple repository support in single process
- Webhook/notification support
- Dry-run mode with detailed preview
- Interactive conflict resolution
- Stash-based conflict avoidance
- Parallel sync for multiple repos

### Explicitly Not in Scope (v1.0)
- GUI interface
- Complex merge strategies
- CI/CD integration
- Repository statistics/reporting
- Sync history tracking
- Cloud storage backends

## Development Approach

### Phase 1 Milestones
1. Basic project structure with error types
2. Repository state detection
3. Sync state analysis (ahead/behind/diverged)
4. Auto-commit implementation
5. Core sync flow (fetch, merge/rebase, push)
6. Exit code compliance

### Incremental Development
Each phase builds on the previous:
- Phase 1 provides a working sync command
- Phase 2 adds automatic triggering
- Phase 3 adds configuration flexibility
- Phase 4 polishes the user experience

### Definition of Done
For each component:
- Unit tests pass
- Integration tests pass
- Documentation updated
- Error cases handled
- Logging implemented
- Performance acceptable

## Open Questions

### Technical
1. Should we support Windows file watching quirks?
2. How to handle submodules?
3. Should we implement credential caching?
4. How to handle SSH key passphrases?

### Product
1. Should sync commands be customizable per-repo?
2. How verbose should default output be?
3. Should we support multiple branches per repo?
4. How to handle force-push scenarios?

### Compatibility
1. Should we provide a git-sync wrapper script?
2. How closely to match original's output format?
3. Should we support the autocommitscript option?
4. How to handle platform-specific paths in config?

## Next Steps

1. Set up basic Rust project with dependencies
2. Implement Phase 1 core synchronization
3. Create test harness with sample repositories
4. Get feedback on core functionality
5. Iterate on design based on learnings

---

This plan is a living document and will be updated as we learn more during implementation.

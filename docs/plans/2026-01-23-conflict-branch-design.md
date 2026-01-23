# Conflict Branch Feature Design

## Overview

When a merge conflict occurs during sync, instead of failing, the tool can optionally:
1. Create a fallback branch with the local state
2. Push to that branch and continue syncing there
3. Actively attempt to return to the original target branch when possible

This is useful for devices that can't easily resolve conflicts (headless servers, embedded devices) - by pushing to a side branch, other machines can resolve the conflict and merge to the target branch.

## Configuration

### New Config Options

```toml
[defaults]
conflict_branch = false  # opt-in, default preserves current behavior

[[repositories]]
path = "/home/user/notes"
conflict_branch = true   # enable for this repo
branch = "main"          # optional: target branch (defaults to repo's default branch)
```

- `conflict_branch` (bool): Enable fallback branch behavior on conflicts. Default: `false`
- `branch` (string): The target branch to track. Defaults to the repository's default branch (detected from `refs/remotes/origin/HEAD`).

### Config Precedence

Following existing pattern: CLI > env vars > repo config > defaults

## Behavior

### On Conflict (when `conflict_branch = true`)

1. Rebase fails with conflict
2. Abort the rebase (restore clean state)
3. Generate branch name: `git-sync/{hostname}-{timestamp}`
   - Example: `git-sync/laptop-2026-01-23-143052`
4. Create and checkout new branch from current HEAD
5. Commit any uncommitted changes (if present)
6. Push to remote
7. Log that we've switched to fallback mode
8. Continue syncing normally on fallback branch

### Detecting Fallback Mode

A repository is in "fallback mode" when:
- Current branch matches pattern `git-sync/*`
- Configured `branch` differs from current branch

### Return-to-Target Logic

On each sync cycle while in fallback mode:

1. Fetch remote (already happens in normal sync)
2. Check: has target branch moved past `last_checked` (in-memory cache)?
   - If NO: skip expensive merge check, continue normal sync on fallback
   - If YES: target branch advanced, proceed to step 3
3. Perform in-memory merge check using libgit2:
   - `repo.merge_commits()` to merge target into current HEAD
   - Check `index.has_conflicts()` - does NOT touch working tree
4. If no conflicts:
   - Checkout target branch
   - Rebase any new fallback commits onto it (may be 0 commits)
   - Push
   - Log successful return to target branch
5. If conflicts:
   - Update `last_checked` to current target branch HEAD
   - Continue syncing on fallback branch
   - Log that return not yet possible

### State Management

**Persistent (config file):**
- `conflict_branch`: boolean to enable feature
- `branch`: target branch name

**In-memory only (optimization):**
- `last_checked`: OID of target branch when we last attempted merge check
- Cleared on process restart (causes one redundant check, acceptable)

## Implementation Details

### Files to Modify

1. **src/config.rs**
   - Add `conflict_branch: Option<bool>` to `DefaultConfig` and `RepositoryConfig`
   - Add default branch detection logic

2. **src/sync.rs**
   - Add `conflict_branch` to `SyncConfig`
   - Add `target_branch` to track intended branch
   - Add `FallbackState` struct for in-memory tracking
   - Modify `rebase()` to handle conflicts when `conflict_branch` is enabled
   - Add `create_fallback_branch()` method
   - Add `try_return_to_target()` method
   - Add `can_merge_cleanly()` in-memory check
   - Modify `sync()` to check for return-to-target when in fallback mode

3. **src/error.rs**
   - Add `SwitchedToFallbackBranch { branch: String }` - informational, not really an error

### Key Implementation Notes

- Use `repo.merge_commits()` for in-memory merge check (doesn't touch working tree)
- Branch name format: `git-sync/{hostname}-{YYYY-MM-DD-HHMMSS}`
- Default branch detection: try `refs/remotes/origin/HEAD`, fall back to "main" then "master"
- Fallback branches are left around after returning (for debugging/history)

## Testing

1. **test_conflict_branch_creation**: Verify fallback branch is created on conflict
2. **test_conflict_branch_naming**: Verify branch name format
3. **test_return_to_target_after_resolution**: Verify automatic return when target is updated
4. **test_no_return_when_still_conflicting**: Verify we stay on fallback when merge would still conflict
5. **test_continued_work_on_fallback**: Verify new commits on fallback are rebased when returning
6. **test_conflict_branch_disabled_by_default**: Verify current behavior when feature is off

## Edge Cases

1. **Multiple devices create fallback branches**: Each gets unique timestamp, no collision
2. **Target branch deleted**: Stay on fallback, log warning
3. **Fallback branch already exists**: Append counter or use new timestamp
4. **Return rebase conflicts**: Stay on fallback, try again next cycle

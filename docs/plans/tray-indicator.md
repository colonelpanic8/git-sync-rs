# System Tray Indicator for git-sync-rs

## Overview

Add an optional system tray indicator using the Linux StatusNotifierItem (SNI) protocol via the `ksni` crate, gated behind a `tray` Cargo feature flag.

## Architecture

### Feature Flag

```toml
[features]
tray = ["ksni"]

[dependencies]
ksni = { version = "0.2", optional = true }
```

### Module Structure

```
src/tray/
  mod.rs          # Re-exports
  indicator.rs    # TrayIndicator struct, ksni::Tray impl
  menu.rs         # Menu building logic
  icons.rs        # Embedded fallback icons
  state.rs        # TrayState, TrayStatus, TrayCommand
```

### State Model

```rust
pub struct TrayState {
    pub repo_path: PathBuf,
    pub status: TrayStatus,
    pub last_sync: Option<Instant>,
    pub last_error: Option<String>,
    pub paused: bool,
}

pub enum TrayStatus {
    Idle,         // Green - waiting for changes
    Syncing,      // Blue/spinning - sync in progress
    Error(String),// Red - last sync failed
    Paused,       // Grey - user paused
}
```

### Communication

- `watch` loop -> tray: `tokio::sync::watch<TrayState>` for status updates
- tray -> `watch` loop: `tokio::sync::mpsc<TrayCommand>` for user actions

```rust
pub enum TrayCommand {
    SyncNow,
    Pause,
    Resume,
    Quit,
}
```

### Menu Structure

```
git-sync-rs - repo_name
---
Status: Idle | Syncing | Error
Last sync: 2m ago
---
Sync Now
Pause / Resume
---
Quit
```

### Icon Strategy

1. Try XDG theme icon: `git-sync-idle`, `git-sync-syncing`, etc.
2. Fall back to embedded PNG icons compiled into the binary

### Integration with Watch Loop

The tray indicator is created and run inside the existing `watch` function when `--tray` is passed. The `ksni` service runs on a separate thread. The watch loop sends state updates via a `watch` channel; menu callbacks send commands back via `mpsc`.

```rust
// In watch.rs (pseudocode)
#[cfg(feature = "tray")]
if enable_tray {
    let (state_tx, state_rx) = tokio::sync::watch::channel(initial_state);
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn tray on blocking thread
    std::thread::spawn(move || {
        let service = ksni::TrayService::new(GitSyncTray::new(state_rx, cmd_tx));
        service.run();
    });

    // In main loop, select! on cmd_rx alongside existing events
}
```

## CLI

```
git-sync-rs watch --tray /path/to/repo
```

The `--tray` flag is only available when compiled with the `tray` feature. If not compiled with the feature, the flag is hidden.

## Nix Packaging

```nix
packages = {
  default = git-sync-rs;           # No tray, no extra deps
  git-sync-rs = ...;               # Standard build
  git-sync-rs-tray = ...;          # With tray feature + dbus deps
};
```

The tray variant adds `dbus` and `glib` to `buildInputs`, and wraps the binary with `XDG_DATA_DIRS` for icon theme lookup.

## Files to Create/Modify

| File | Change |
|------|--------|
| `Cargo.toml` | Add `tray` feature, `ksni` optional dep |
| `src/lib.rs` | Add `#[cfg(feature = "tray")] pub mod tray;` |
| `src/tray/mod.rs` | Module re-exports |
| `src/tray/indicator.rs` | `TrayIndicator` struct, SNI impl |
| `src/tray/menu.rs` | Menu building logic |
| `src/tray/icons.rs` | Embedded fallback icons |
| `src/tray/state.rs` | `TrayState`, `TrayStatus`, `TrayCommand` |
| `src/watch.rs` | Add tray integration, channels, `enable_tray` config |
| `src/bin/git-sync-rs.rs` | Add `--tray` flag to watch subcommand |
| `assets/icons/` | Fallback SVG/PNG icons |
| `flake.nix` | Add `git-sync-rs-tray` package variant |

## Out of Scope

- Multi-repo monitoring in a single indicator
- Config file option for tray (CLI flag only)
- Custom icon themes
- Home-manager module (future)

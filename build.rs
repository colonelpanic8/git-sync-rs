use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let commit = std::env::var("GIT_COMMIT_HASH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| get_git_commit().ok())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=GIT_COMMIT_HASH={}", commit);

    // Ensure we rerun the build script when HEAD advances (normal commits do not change `.git/HEAD`).
    // If GIT_COMMIT_HASH is injected by the builder (e.g. Nix), we don't need
    // git-dir based rerun tracking.
    if std::env::var_os("GIT_COMMIT_HASH").is_none() {
        if let Ok(git_dir) = get_git_dir() {
            emit_rerun_if_git_head_changes(&git_dir);
        }
    }
}

fn get_git_commit() -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if commit.is_empty() {
        return Err("empty git commit hash".into());
    }

    Ok(commit)
}

fn get_git_dir() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .map_err(|err| err.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return Err("empty git dir".into());
    }

    let p = PathBuf::from(raw);
    if p.is_absolute() {
        Ok(p)
    } else {
        let manifest_dir =
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
        Ok(manifest_dir.join(p))
    }
}

fn emit_rerun_if_git_head_changes(git_dir: &Path) {
    let head_path = git_dir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());

    let common_dir = read_common_dir(git_dir).unwrap_or_else(|| git_dir.to_path_buf());
    let packed_refs = common_dir.join("packed-refs");
    println!("cargo:rerun-if-changed={}", packed_refs.display());

    if let Ok(head_contents) = fs::read_to_string(&head_path) {
        if let Some(ref_path) = head_contents.trim().strip_prefix("ref: ") {
            let ref_file = common_dir.join(ref_path);
            println!("cargo:rerun-if-changed={}", ref_file.display());
        }
    }
}

fn read_common_dir(git_dir: &Path) -> Option<PathBuf> {
    // Worktrees store shared refs/packed-refs in a "commondir".
    let commondir_path = git_dir.join("commondir");
    let commondir = fs::read_to_string(commondir_path).ok()?;
    let commondir = commondir.trim();
    if commondir.is_empty() {
        return None;
    }
    Some(git_dir.join(commondir))
}

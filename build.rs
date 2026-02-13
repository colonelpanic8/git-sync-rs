use std::process::Command;

fn main() {
    let commit = get_git_commit().unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=GIT_COMMIT_HASH={}", commit);

    println!("cargo:rerun-if-changed=.git/HEAD");
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

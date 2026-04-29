use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow};

pub fn ensure_available() -> Result<()> {
    let output = Command::new("git")
        .arg("--version")
        .output()
        .context("running `git --version` (is git installed and on PATH?)")?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git --version` exited with status {}",
            output.status
        ));
    }
    Ok(())
}

pub fn clone(url: &str, dest: &Path) -> Result<()> {
    let status = Command::new("git")
        .arg("clone")
        .arg(url)
        .arg(dest)
        .status()
        .with_context(|| format!("invoking `git clone {url}`"))?;
    if !status.success() {
        return Err(anyhow!(
            "`git clone {url}` failed with status {status} (check the URL and your credentials)"
        ));
    }
    Ok(())
}

/// Best-effort sync of a previously cloned repo to the latest of its tracked
/// upstream branch. Errors propagate so the caller can decide how to surface
/// them (for read flows, surfacing a warning is usually enough).
pub fn fetch_and_fast_forward(repo: &Path) -> Result<()> {
    run_git(repo, &["fetch", "--quiet", "--prune"])?;
    run_git(repo, &["reset", "--quiet", "--hard", "@{upstream}"])?;
    Ok(())
}

fn run_git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .with_context(|| format!("invoking `git {}` in {}", args.join(" "), repo.display()))?;
    if !status.success() {
        return Err(anyhow!(
            "`git {}` failed in {} with status {status}",
            args.join(" "),
            repo.display()
        ));
    }
    Ok(())
}

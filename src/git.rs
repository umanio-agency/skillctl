use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

pub fn head_sha(repo: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("invoking `git rev-parse HEAD` in {}", repo.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git rev-parse HEAD` failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let sha = String::from_utf8(output.stdout)
        .context("`git rev-parse HEAD` returned non-UTF8 output")?;
    Ok(sha.trim().to_string())
}

/// List the blob SHAs of every file under `path` at `refspec`, keyed by their
/// repo-relative path. Returns an empty map if the path does not exist at that
/// ref (a missing-from-library signal).
pub fn ls_tree_blobs(repo: &Path, refspec: &str, path: &Path) -> Result<HashMap<PathBuf, String>> {
    let output = Command::new("git")
        .current_dir(repo)
        .args(["ls-tree", "-r", "-z", refspec, "--"])
        .arg(path)
        .output()
        .with_context(|| {
            format!(
                "invoking `git ls-tree -r {refspec} -- {}` in {}",
                path.display(),
                repo.display()
            )
        })?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git ls-tree {refspec}` failed in {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let raw = String::from_utf8(output.stdout).context("`git ls-tree` returned non-UTF8 output")?;
    let mut map = HashMap::new();
    for entry in raw.split('\0') {
        if entry.is_empty() {
            continue;
        }
        // Format: "<mode> <type> <sha>\t<path>"
        let (meta, file) = entry
            .split_once('\t')
            .ok_or_else(|| anyhow!("malformed ls-tree entry: {entry:?}"))?;
        let parts: Vec<&str> = meta.split_whitespace().collect();
        if parts.len() != 3 || parts[1] != "blob" {
            // Skip submodules, symlinks etc. for now.
            continue;
        }
        let sha = parts[2].to_string();
        map.insert(PathBuf::from(file), sha);
    }
    Ok(map)
}

/// Compute the git blob SHA of a local file as if it were `git add`-ed.
pub fn hash_object(file: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["hash-object"])
        .arg(file)
        .output()
        .with_context(|| format!("invoking `git hash-object {}`", file.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git hash-object {}` failed: {}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let sha =
        String::from_utf8(output.stdout).context("`git hash-object` returned non-UTF8 output")?;
    Ok(sha.trim().to_string())
}

/// Stage all changes under `path` (including deletions).
pub fn add_all(repo: &Path, path: &Path) -> Result<()> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["add", "-A", "--"])
        .arg(path)
        .status()
        .with_context(|| {
            format!(
                "invoking `git add -A -- {}` in {}",
                path.display(),
                repo.display()
            )
        })?;
    if !status.success() {
        return Err(anyhow!(
            "`git add` failed in {} with status {status}",
            repo.display()
        ));
    }
    Ok(())
}

/// True if there are staged changes in the index relative to HEAD.
pub fn has_staged_changes(repo: &Path) -> Result<bool> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .with_context(|| format!("invoking `git diff --cached --quiet` in {}", repo.display()))?;
    match status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(anyhow!(
            "`git diff --cached --quiet` exited unexpectedly with {status} in {}",
            repo.display()
        )),
    }
}

pub fn commit(repo: &Path, message: &str) -> Result<String> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["commit", "--quiet", "-m", message])
        .status()
        .with_context(|| format!("invoking `git commit` in {}", repo.display()))?;
    if !status.success() {
        return Err(anyhow!(
            "`git commit` failed in {} with status {status} (is git user.name/user.email configured?)",
            repo.display()
        ));
    }
    head_sha(repo)
}

pub fn push(repo: &Path) -> Result<()> {
    let status = Command::new("git")
        .current_dir(repo)
        .args(["push"])
        .status()
        .with_context(|| format!("invoking `git push` in {}", repo.display()))?;
    if !status.success() {
        return Err(anyhow!(
            "`git push` failed in {} with status {status} (check your credentials and write access)",
            repo.display()
        ));
    }
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

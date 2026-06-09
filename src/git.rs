use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};

/// Build a fresh `git` invocation with `core.hooksPath` neutralised. The
/// library cache repo is shared between potentially-malicious skill content
/// and the operator's machine; if a malicious library dropped a script at
/// the operator's globally-configured `core.hooksPath`, an otherwise
/// innocent `git commit` here would execute it. Forcing `core.hooksPath` to
/// a non-existent path per invocation makes hook execution impossible
/// regardless of the global git config or any in-cache `.git/config` mutation.
fn git_cmd() -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.hooksPath=/dev/null"]);
    // Pin the transport allowlist to https + ssh (the only forms
    // `host::parse_remote_url` accepts). `protocol.allow=never` denies every
    // unlisted transport — `ext::` (command execution), `file://`, `fd::`,
    // `git://` — so even a library URL that smuggled an alternate transport
    // past URL validation can never make git speak it, and we never depend on
    // the operator's ambient `protocol.*.allow` config. https/ssh clone, fetch
    // and push all keep working.
    cmd.args([
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.ssh.allow=always",
    ]);
    cmd
}

/// Render `git`'s stderr safely for inclusion in error chains. (1) Take the
/// first non-empty line — git stack traces past the first line rarely add
/// signal but multiply leak surface. (2) Strip control bytes (C0/C1, DEL,
/// ESC, NUL) — git stderr can echo back attacker-controlled refnames, hook
/// output, proxy banners. (3) Scrub known credential token prefixes
/// (`ghp_*`, `gho_*`, `ghs_*`, `ghu_*`, `github_pat_*`, `x-access-token:*`)
/// so a credential helper / `git remote -v` style leak in an error doesn't
/// make it into the JSON output, `anyhow` error chain, or CI logs.
pub(crate) fn scrub_stderr(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let first_line = lossy
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let stripped: String = first_line
        .chars()
        .map(|c| {
            let cp = c as u32;
            if cp < 0x20 || cp == 0x7F || (0x80..=0x9F).contains(&cp) {
                '?'
            } else {
                c
            }
        })
        .collect();
    scrub_token_patterns(&stripped)
}

fn scrub_token_patterns(s: &str) -> String {
    let mut result = s.to_string();
    // Order matters: `x-access-token:<token>` envelops e.g. `ghp_…`, so
    // process the longest/outermost pattern first. Otherwise the inner
    // `ghp_***` gets replaced and the outer `x-access-token:` walks past
    // its own redacted-but-no-longer-token-shaped suffix.
    for prefix in &[
        "x-access-token:",
        "github_pat_",
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
    ] {
        result = redact_after_prefix(&result, prefix);
    }
    result
}

fn redact_after_prefix(s: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut remaining = s;
    while let Some(idx) = remaining.find(prefix) {
        out.push_str(&remaining[..idx]);
        out.push_str(prefix);
        out.push_str("***");
        let after = &remaining[idx + prefix.len()..];
        let token_end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(after.len());
        remaining = &after[token_end..];
    }
    out.push_str(remaining);
    out
}

pub fn ensure_available() -> Result<()> {
    let output = git_cmd()
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
    // `--no-recurse-submodules` defends against a malicious library that
    // adds a `.gitmodules` pointing at an attacker-controlled repo: without
    // this flag, `git clone` would recursively pull and check out those
    // submodules during `skillctl init`, landing arbitrary content in the
    // library cache. Skills do not use submodules; if a legitimate use
    // case appears, it can be opt-in via an explicit flag later.
    let status = git_cmd()
        .arg("clone")
        .arg("--no-recurse-submodules")
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
/// them (for read flows, surfacing a warning is usually enough). Refuses to
/// touch the cache if `git status --porcelain` reports uncommitted/staged
/// changes — those almost always come from a previous skillctl run that
/// crashed mid-commit, and a silent `reset --hard @{upstream}` would
/// destroy them.
pub fn fetch_and_fast_forward(repo: &Path) -> Result<()> {
    if !is_clean(repo)? {
        return Err(anyhow!(
            "library cache at {} has uncommitted changes — refusing to `reset --hard @{{upstream}}` and silently discard them (likely the residue of a previously-interrupted `skillctl push`/`detect`; inspect with `git -C {} status`, commit/discard as appropriate, then re-run)",
            repo.display(),
            repo.display()
        ));
    }
    run_git(repo, &["fetch", "--quiet", "--prune"])?;
    run_git(repo, &["reset", "--quiet", "--hard", "@{upstream}"])?;
    Ok(())
}

/// True iff `git status --porcelain` reports an empty working tree + index,
/// ignoring skillctl's own lock file (which sits in the cache working tree
/// while the command holds the cache lock).
fn is_clean(repo: &Path) -> Result<bool> {
    let output = git_cmd()
        .current_dir(repo)
        .args(["status", "--porcelain"])
        .output()
        .with_context(|| format!("invoking `git status --porcelain` in {}", repo.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git status --porcelain` failed in {}: {}",
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(porcelain_is_clean(&stdout))
}

/// Decide cleanliness from `git status --porcelain` output, ignoring
/// skillctl's lock file. Each porcelain line is `XY <path>`; we only treat
/// the tree as dirty if some entry other than the lock file is present.
/// Without this, holding the cache lock makes every `fetch_and_fast_forward`
/// see the cache as dirty and skip its refresh.
fn porcelain_is_clean(stdout: &str) -> bool {
    !stdout.lines().any(|line| {
        let path = line.get(3..).unwrap_or("").trim();
        !path.is_empty() && path != crate::lock::LOCK_FILE_NAME
    })
}

pub fn head_sha(repo: &Path) -> Result<String> {
    let output = git_cmd()
        .current_dir(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("invoking `git rev-parse HEAD` in {}", repo.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git rev-parse HEAD` failed in {}: {}",
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    let sha = String::from_utf8(output.stdout)
        .context("`git rev-parse HEAD` returned non-UTF8 output")?;
    Ok(sha.trim().to_string())
}

/// List the blob SHAs of every file under `path` at `refspec`, keyed by
/// their repo-relative path.
///
/// Returns:
/// - `Ok(Some(map))` with an empty map if the path doesn't exist at that
///   ref (a missing-from-library signal).
/// - `Ok(None)` if the `refspec` itself doesn't resolve in this repo —
///   e.g. an orphaned `source_sha` from `.skills.toml` after a library
///   force-push or GC. Callers should treat this as a per-skill problem
///   (skip with warning) rather than failing the whole batch.
/// - `Err(...)` for any other git failure.
pub fn ls_tree_blobs(
    repo: &Path,
    refspec: &str,
    path: &Path,
) -> Result<Option<HashMap<PathBuf, String>>> {
    let output = git_cmd()
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
        let stderr_lossy = String::from_utf8_lossy(&output.stderr);
        if stderr_lossy.contains("Not a valid object name")
            || stderr_lossy.contains("unknown revision")
            || stderr_lossy.contains("bad revision")
        {
            return Ok(None);
        }
        return Err(anyhow!(
            "`git ls-tree {refspec}` failed in {}: {}",
            repo.display(),
            scrub_stderr(&output.stderr)
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
    Ok(Some(map))
}

/// Compute the git blob SHA of a local file as if it were `git add`-ed.
pub fn hash_object(file: &Path) -> Result<String> {
    let output = git_cmd()
        .args(["hash-object"])
        .arg(file)
        .output()
        .with_context(|| format!("invoking `git hash-object {}`", file.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git hash-object {}` failed: {}",
            file.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    let sha =
        String::from_utf8(output.stdout).context("`git hash-object` returned non-UTF8 output")?;
    Ok(sha.trim().to_string())
}

/// Stage all changes under `path` (including deletions).
pub fn add_all(repo: &Path, path: &Path) -> Result<()> {
    let output = git_cmd()
        .current_dir(repo)
        .args(["add", "-A", "--"])
        .arg(path)
        .output()
        .with_context(|| {
            format!(
                "invoking `git add -A -- {}` in {}",
                path.display(),
                repo.display()
            )
        })?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git add` failed in {}: {}",
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    Ok(())
}

/// Restore `path` in `repo` to its HEAD state, discarding any working-tree
/// and index changes for that path. Used by `push` after a per-skill
/// failure to undo the partial work before continuing with the next skill.
/// "Path did not match" is tolerated — the next `fetch_and_fast_forward`
/// also acts as a safety net by `reset --hard @{upstream}`.
pub fn checkout_paths(repo: &Path, path: &Path) -> Result<()> {
    let output = git_cmd()
        .current_dir(repo)
        .args(["checkout", "--quiet", "HEAD", "--"])
        .arg(path)
        .output()
        .with_context(|| {
            format!(
                "invoking `git checkout HEAD -- {}` in {}",
                path.display(),
                repo.display()
            )
        })?;
    if !output.status.success() {
        let stderr_lossy = String::from_utf8_lossy(&output.stderr);
        if stderr_lossy.contains("did not match any file") {
            return Ok(());
        }
        return Err(anyhow!(
            "`git checkout HEAD -- {}` failed in {}: {}",
            path.display(),
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    Ok(())
}

/// True if there are staged changes in the index relative to HEAD.
pub fn has_staged_changes(repo: &Path) -> Result<bool> {
    let status = git_cmd()
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
    let output = git_cmd()
        .current_dir(repo)
        .args(["commit", "--quiet", "-m", message])
        .output()
        .with_context(|| format!("invoking `git commit` in {}", repo.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git commit` failed in {} (is git user.name/user.email configured?): {}",
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    head_sha(repo)
}

/// Reset `repo` to its parent commit (`HEAD~1`), discarding the most
/// recent commit AND its working-tree / index changes. Used by `push` /
/// `detect` to undo the just-created commit when `git push` fails — the
/// commit otherwise sits orphaned in the local cache, ahead of upstream,
/// and the next `fetch_and_fast_forward` would silently `reset --hard
/// @{upstream}` it away (or refuse, post-M10, if the working tree happened
/// to get dirty in between). Better to make the rollback explicit.
pub fn reset_hard_to_parent(repo: &Path) -> Result<()> {
    run_git(repo, &["reset", "--quiet", "--hard", "HEAD~1"])
}

pub fn push(repo: &Path) -> Result<()> {
    let output = git_cmd()
        .current_dir(repo)
        .args(["push"])
        .output()
        .with_context(|| format!("invoking `git push` in {}", repo.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git push` failed in {} (check your credentials and write access): {}",
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    Ok(())
}

fn run_git(repo: &Path, args: &[&str]) -> Result<()> {
    let output = git_cmd()
        .current_dir(repo)
        .args(args)
        .output()
        .with_context(|| format!("invoking `git {}` in {}", args.join(" "), repo.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "`git {}` failed in {}: {}",
            args.join(" "),
            repo.display(),
            scrub_stderr(&output.stderr)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{porcelain_is_clean, scrub_stderr};

    #[test]
    fn porcelain_empty_is_clean() {
        assert!(porcelain_is_clean(""));
        assert!(porcelain_is_clean("\n"));
    }

    #[test]
    fn porcelain_only_lock_file_is_clean() {
        assert!(porcelain_is_clean("?? .skillctl.lock"));
        assert!(porcelain_is_clean("?? .skillctl.lock\n"));
    }

    #[test]
    fn porcelain_other_untracked_is_dirty() {
        assert!(!porcelain_is_clean("?? other.txt"));
        assert!(!porcelain_is_clean(" M src/foo.rs"));
    }

    #[test]
    fn porcelain_lock_plus_real_change_is_dirty() {
        assert!(!porcelain_is_clean("?? .skillctl.lock\n M src/foo.rs"));
    }

    #[test]
    fn scrub_keeps_clean_first_line() {
        assert_eq!(
            scrub_stderr(b"fatal: ambiguous argument 'HEAD'"),
            "fatal: ambiguous argument 'HEAD'"
        );
    }

    #[test]
    fn scrub_drops_trailing_lines() {
        let raw = b"first line\nsecond line\nthird";
        assert_eq!(scrub_stderr(raw), "first line");
    }

    #[test]
    fn scrub_strips_ansi_escape() {
        let raw = b"\x1b[31mERROR\x1b[0m: boom";
        let out = scrub_stderr(raw);
        assert!(!out.contains('\x1b'), "ESC should be stripped: {out}");
        assert!(out.contains("ERROR"));
    }

    #[test]
    fn scrub_redacts_ghp_token() {
        let raw = b"fatal: could not read Password for 'https://ghp_abc123def456ghi789jkl012mno345pqr678@github.com'";
        let out = scrub_stderr(raw);
        assert!(
            out.contains("ghp_***"),
            "ghp_ token should be redacted: {out}"
        );
        assert!(
            !out.contains("ghp_abc123"),
            "raw ghp_ value must not survive: {out}"
        );
    }

    #[test]
    fn scrub_redacts_x_access_token() {
        // x-access-token: envelops the inner ghp_ token, so the outer
        // prefix wins (and the inner secret value is consumed as part of
        // the outer token). Both the outer marker and the inner raw
        // secret must not appear in the output.
        let raw =
            b"fatal: remote https://x-access-token:ghp_abc12345@github.com/foo/bar.git not found";
        let out = scrub_stderr(raw);
        assert!(
            out.contains("x-access-token:***"),
            "x-access-token marker missing: {out}"
        );
        assert!(
            !out.contains("ghp_abc"),
            "raw ghp_ value must not survive: {out}"
        );
        assert!(
            !out.contains("abc12345"),
            "secret value must not survive: {out}"
        );
    }

    #[test]
    fn scrub_handles_empty_stderr() {
        assert_eq!(scrub_stderr(b""), "");
        assert_eq!(scrub_stderr(b"\n\n"), "");
    }

    #[test]
    fn scrub_strips_nul_byte() {
        let raw = b"fatal: \x00cor\x00rupt index";
        let out = scrub_stderr(raw);
        assert!(!out.contains('\0'));
        assert!(out.contains("corrupt") || out.contains("cor"));
    }
}

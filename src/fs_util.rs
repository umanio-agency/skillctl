use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::error::AppError;

/// Iteratively copy `src` into `dst`. Hard-rejects, with `AppError::Config`,
/// any of:
/// - symlinks at the top-level `src` or any descendant entry (would let a
///   crafted link exfiltrate or overwrite files outside the skill, then ship
///   the content via `skillctl push`);
/// - non-regular-non-directory file types — FIFO/named-pipe, socket,
///   block/character device (would block `fs::copy` indefinitely on a FIFO
///   or read until OOM on `/dev/zero`-style devices);
/// - regular files with `nlink > 1` (hardlinks share inodes; on Unix
///   `fs::copy` would read the target content as-is, enabling silent
///   exfiltration through the round-trip).
///
/// Implementation: explicit work-stack (`Vec<(PathBuf, PathBuf)>`) of
/// `(from, to)` directory pairs instead of recursion, so a maliciously
/// deep skill tree (e.g. 10k-level nesting) cannot blow Rust's default
/// 8 MiB thread stack.
///
/// On Unix: regular file modes are masked to `0o644 | (src_mode & 0o100)`
/// (i.e. only the user-execute bit is propagated; setuid/setgid/sticky,
/// group/world writability, and group/world execute bits are stripped).
/// Skills do not need group/world writability or setuid; a library
/// drop-in of a setuid binary would otherwise become an attack vector when
/// installed into a project that gets shared with others.
pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    let src_meta = fs::symlink_metadata(src)
        .with_context(|| format!("reading metadata for {}", src.display()))?;
    let src_ft = src_meta.file_type();
    if src_ft.is_symlink() {
        return Err(AppError::Config(format!(
            "refusing to copy `{}`: source is a symlink (skill folders may not be or contain symlinks)",
            src.display()
        ))
        .into());
    }
    if !src_ft.is_dir() {
        return Err(AppError::Config(format!(
            "refusing to copy `{}`: source must be a regular directory",
            src.display()
        ))
        .into());
    }
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;

    let mut stack: Vec<(PathBuf, PathBuf)> = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from_dir, to_dir)) = stack.pop() {
        for entry in
            fs::read_dir(&from_dir).with_context(|| format!("reading {}", from_dir.display()))?
        {
            let entry = entry?;
            let from = entry.path();
            let to = to_dir.join(entry.file_name());
            let file_type = entry
                .file_type()
                .with_context(|| format!("stat {}", from.display()))?;
            if file_type.is_symlink() {
                return Err(AppError::Config(format!(
                    "refusing to copy symlink `{}`: skill folders may not contain symlinks (a crafted link could exfiltrate or overwrite files outside the skill, e.g. `~/.aws/credentials`). Replace the symlink with actual content if it is intentional.",
                    from.display()
                ))
                .into());
            }
            if file_type.is_dir() {
                fs::create_dir_all(&to).with_context(|| format!("creating {}", to.display()))?;
                stack.push((from, to));
            } else if file_type.is_file() {
                reject_hardlink(&from)?;
                fs::copy(&from, &to)
                    .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
                mask_file_mode(&to)?;
            } else {
                return Err(AppError::Config(format!(
                    "refusing to copy `{}`: not a regular file or directory (FIFO, socket, device, or other special file). Skill folders may contain only regular files and directories.",
                    from.display()
                ))
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn reject_hardlink(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta =
        fs::metadata(path).with_context(|| format!("reading metadata for {}", path.display()))?;
    if meta.nlink() > 1 {
        return Err(AppError::Config(format!(
            "refusing to copy `{}`: file has {} hardlinks (shared inodes can silently exfiltrate content from outside the skill folder via `skillctl push`)",
            path.display(),
            meta.nlink()
        ))
        .into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_hardlink(_path: &Path) -> Result<()> {
    // Hardlink detection on Windows requires `GetFileInformationByHandle` and
    // counting via `nNumberOfLinks` — not in std. Skip until a real Windows
    // use case forces the implementation.
    Ok(())
}

#[cfg(unix)]
fn mask_file_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta =
        fs::metadata(path).with_context(|| format!("reading mode for {}", path.display()))?;
    let src_mode = meta.permissions().mode();
    let masked = 0o644 | (src_mode & 0o100);
    if (src_mode & 0o7777) != masked {
        let perms = std::fs::Permissions::from_mode(masked);
        fs::set_permissions(path, perms)
            .with_context(|| format!("masking mode of {}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn mask_file_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Atomically replace the contents of `dst` with the contents of `src`.
///
/// Crash safety: at any process-interruption point (SIGINT, panic, power
/// loss between two `fs::rename` syscalls), either the old `dst` or the new
/// content is in place — never a half-written tree. The implementation
/// stages the new content into a uniquely-named sibling of `dst`, moves the
/// old `dst` aside into a backup sibling, then atomically renames the
/// staging dir over `dst`. A failure during the staging copy never touches
/// the live `dst`. A failure during the final rename rolls the backup back
/// into place.
///
/// Refuses to operate when `dst` is itself a symlink: a malicious symlink at
/// the destination (e.g. `.claude/skills` linked to `~/.ssh`) would otherwise
/// be followed and replace the target's content.
pub fn replace_folder_contents(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        let dst_meta = fs::symlink_metadata(dst)
            .with_context(|| format!("reading metadata for {}", dst.display()))?;
        if dst_meta.file_type().is_symlink() {
            return Err(AppError::Config(format!(
                "refusing to replace `{}`: target is a symlink (would follow outside the project/library root)",
                dst.display()
            ))
            .into());
        }
    }

    let tmp = unique_sibling(dst, "tmp");
    let bak = unique_sibling(dst, "bak");

    // Stage new content into a fresh sibling. copy_dir_all creates the dir
    // itself; on failure here, dst is untouched.
    if let Err(e) = copy_dir_all(src, &tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }

    let dst_existed = dst.exists();
    if dst_existed {
        if let Err(e) = fs::rename(dst, &bak) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e).with_context(|| format!("moving {} aside before swap", dst.display()));
        }
    }

    if let Err(e) = fs::rename(&tmp, dst) {
        // Roll back: try to restore the old dst from the backup. If that
        // fails too, leave both bak and tmp on disk so the operator can
        // recover manually — surface a clear error mentioning both paths.
        if dst_existed {
            if let Err(restore_err) = fs::rename(&bak, dst) {
                let _ = fs::remove_dir_all(&tmp);
                return Err(anyhow::anyhow!(
                    "swap failed and rollback failed: original content is at {}, new content at {} (original error: {e}, rollback error: {restore_err})",
                    bak.display(),
                    tmp.display()
                ));
            }
        }
        let _ = fs::remove_dir_all(&tmp);
        return Err(e).with_context(|| format!("swapping staging copy into {}", dst.display()));
    }

    if dst_existed {
        let _ = fs::remove_dir_all(&bak);
    }
    Ok(())
}

/// Atomically swap `new_content` into `dst`, moving any existing `dst` to
/// `bak_path` first. On failure during the final rename, the old content at
/// `bak_path` is renamed back. The `new_content` directory is consumed
/// (renamed into place on success, removed on failure).
///
/// Used by `pull --on-divergence fork-locally`: the caller stages the
/// library version into a temp sibling of the local destination, then calls
/// this with `bak_path` set to the operator-chosen fork target. On crash
/// between the two renames, either the original or the new content is at
/// `dst` (never neither, never partial).
pub fn swap_with_bak(new_content: &Path, dst: &Path, bak_path: &Path) -> Result<()> {
    let dst_existed = dst.exists();
    if dst_existed {
        fs::rename(dst, bak_path).with_context(|| {
            format!(
                "moving {} aside to {} before swap",
                dst.display(),
                bak_path.display()
            )
        })?;
    }
    if let Err(e) = fs::rename(new_content, dst) {
        if dst_existed {
            let _ = fs::rename(bak_path, dst);
        }
        let _ = fs::remove_dir_all(new_content);
        return Err(e).with_context(|| format!("swapping into {}", dst.display()));
    }
    Ok(())
}

/// Build a uniquely-named sibling path of `dst` for use as a staging or
/// backup slot. The name is hidden (`.skillctl-…`), tagged with the role
/// (`tmp` or `bak`), the original basename, the process id, and a
/// nanosecond timestamp. Unique under the standard `LibraryCacheLock` /
/// `ProjectConfigLock` protection in `src/lock.rs`, and unique-enough even
/// without locks given the nanosecond resolution + pid.
pub fn unique_sibling(dst: &Path, role: &str) -> PathBuf {
    let parent = dst.parent().unwrap_or(Path::new("."));
    let basename = dst.file_name().and_then(|n| n.to_str()).unwrap_or("anon");
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!(".skillctl-{role}.{basename}.{pid}.{nanos}"))
}

/// Express `path` relative to `base` when possible; otherwise strip a leading
/// `./` for a cleaner display.
pub fn relative_to_or_self(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| strip_dot_prefix(path.to_path_buf()))
}

pub fn strip_dot_prefix(p: PathBuf) -> PathBuf {
    p.strip_prefix(".").map(Path::to_path_buf).unwrap_or(p)
}

/// Render a path for display, swapping a leading `$HOME` (resolved at
/// runtime from the environment) with `~/`. Used in error messages,
/// JSON output, and operator-facing logs to avoid leaking the
/// operator's Unix username (`/Users/<name>/...`) into CI logs and
/// agent-mode JSON consumed by third parties.
///
/// Falls back to the raw path when `HOME` is unset (CI tasks running
/// as a service user, Windows without an HOME translation, etc.) or
/// when the path doesn't share the `$HOME` prefix.
pub fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME");
    display_path_with_home(path, home.as_deref().map(Path::new))
}

// Inner implementation. Split out so tests can inject a known HOME
// without touching `std::env` (which is unsafe to set under cargo's
// parallel-test execution model in edition 2024).
fn display_path_with_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home_path) = home
        && !home_path.as_os_str().is_empty()
        && let Ok(rest) = path.strip_prefix(home_path)
    {
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn copy_dir_all_rejects_symlink_inside_source() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();

        fs::write(src.path().join("normal.txt"), "ok").unwrap();
        symlink("/etc/hostname", src.path().join("evil")).unwrap();

        let result = copy_dir_all(src.path(), &dst.path().join("out"));
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("symlink"),
            "error must mention symlink"
        );
    }

    #[test]
    fn copy_dir_all_rejects_top_level_symlink_source() {
        let work = TempDir::new().unwrap();
        let real = work.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = work.path().join("link");
        symlink(&real, &link).unwrap();

        let result = copy_dir_all(&link, &work.path().join("out"));
        assert!(result.is_err());
    }

    #[test]
    fn copy_dir_all_rejects_symlink_subdir() {
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("ok.txt"), "ok").unwrap();
        let outside = work.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, src.join("linked_dir")).unwrap();

        let result = copy_dir_all(&src, &work.path().join("out"));
        assert!(result.is_err());
    }

    #[test]
    fn copy_dir_all_accepts_normal_tree() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::write(src.path().join("a.txt"), "a").unwrap();
        fs::create_dir(src.path().join("sub")).unwrap();
        fs::write(src.path().join("sub/b.txt"), "b").unwrap();

        copy_dir_all(src.path(), dst.path()).unwrap();
        assert_eq!(fs::read_to_string(dst.path().join("a.txt")).unwrap(), "a");
        assert_eq!(
            fs::read_to_string(dst.path().join("sub/b.txt")).unwrap(),
            "b"
        );
    }

    #[test]
    fn replace_folder_contents_rejects_symlink_destination() {
        let work = TempDir::new().unwrap();
        let real_target = work.path().join("sensitive");
        fs::create_dir(&real_target).unwrap();
        fs::write(real_target.join("data"), "secret").unwrap();

        let link_dst = work.path().join("destination");
        symlink(&real_target, &link_dst).unwrap();

        let src = work.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("ok"), "ok").unwrap();

        let result = replace_folder_contents(&src, &link_dst);
        assert!(result.is_err());
        assert!(
            real_target.exists() && real_target.join("data").exists(),
            "remove_dir_all must NOT have followed the symlink"
        );
    }

    #[test]
    fn copy_dir_all_rejects_fifo_inside_source() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        fs::write(src.path().join("normal.txt"), "ok").unwrap();
        let fifo_path = src.path().join("evil_pipe");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo must be available on this host");
        assert!(status.success(), "mkfifo failed");

        let result = copy_dir_all(src.path(), &dst.path().join("out"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("FIFO") || err.contains("not a regular file"),
            "error should mention non-regular file: {err}"
        );
    }

    #[test]
    fn copy_dir_all_rejects_hardlink_inside_source() {
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        fs::create_dir(&src).unwrap();
        let real = work.path().join("secret");
        fs::write(&real, "sensitive").unwrap();
        // Hardlink the secret into the skill folder. fs::hard_link gives both
        // paths the same inode and nlink count of 2.
        let link_inside = src.join("data");
        fs::hard_link(&real, &link_inside).unwrap();

        let result = copy_dir_all(&src, &work.path().join("out"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("hardlink"),
            "error should mention hardlink: {err}"
        );
    }

    #[test]
    fn copy_dir_all_rejects_non_directory_source() {
        let work = TempDir::new().unwrap();
        let file = work.path().join("not_a_dir.txt");
        fs::write(&file, "x").unwrap();
        let result = copy_dir_all(&file, &work.path().join("out"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("regular directory")
        );
    }

    #[test]
    fn replace_folder_contents_normal_case_works() {
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        let dst = work.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a"), "new").unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("b"), "old").unwrap();

        replace_folder_contents(&src, &dst).unwrap();
        assert!(dst.join("a").exists());
        assert!(!dst.join("b").exists(), "old content should be gone");
        assert_eq!(fs::read_to_string(dst.join("a")).unwrap(), "new");
    }

    #[test]
    fn replace_folder_contents_failure_preserves_dst() {
        // If the source is unreadable (we use a non-existent path), the
        // staging copy fails and `dst` is left untouched. This validates
        // the atomicity contract: failure means dst's old content stays.
        let work = TempDir::new().unwrap();
        let dst = work.path().join("dst");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("important"), "do not lose").unwrap();

        let bad_src = work.path().join("does-not-exist");
        let result = replace_folder_contents(&bad_src, &dst);
        assert!(result.is_err());
        assert!(
            dst.join("important").exists(),
            "dst must be untouched on staging failure"
        );
        assert_eq!(
            fs::read_to_string(dst.join("important")).unwrap(),
            "do not lose"
        );
    }

    #[test]
    fn replace_folder_contents_failure_cleans_staging() {
        let work = TempDir::new().unwrap();
        let dst = work.path().join("dst");
        fs::create_dir(&dst).unwrap();

        let bad_src = work.path().join("does-not-exist");
        let _ = replace_folder_contents(&bad_src, &dst);

        // No `.skillctl-tmp.*` or `.skillctl-bak.*` siblings should remain.
        let parent = dst.parent().unwrap();
        let leftovers: Vec<_> = fs::read_dir(parent)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".skillctl-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no skillctl-* siblings should remain, found: {:?}",
            leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn copy_dir_all_handles_deep_nesting() {
        // 200 levels of single-character dir names. Recursive copy_dir_all
        // could blow the stack on debug builds (each frame holds a DirEntry
        // iterator); the iterative version handles arbitrary depth bounded
        // only by the filesystem's PATH_MAX (≈1024 bytes on macOS) — kept
        // short here to stay well under that limit.
        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        let mut cur = src.clone();
        for _ in 0..200 {
            cur = cur.join("a");
        }
        fs::create_dir_all(&cur).unwrap();
        fs::write(cur.join("leaf.txt"), "deep").unwrap();

        let dst = work.path().join("dst");
        copy_dir_all(&src, &dst).unwrap();

        let mut cur = dst.clone();
        for _ in 0..200 {
            cur = cur.join("a");
        }
        assert_eq!(fs::read_to_string(cur.join("leaf.txt")).unwrap(), "deep");
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_all_masks_setuid_and_world_write() {
        use std::os::unix::fs::PermissionsExt;

        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        fs::create_dir(&src).unwrap();
        let exec_file = src.join("exec");
        fs::write(&exec_file, "#!/bin/sh\n").unwrap();
        // setuid (04000) + setgid (02000) + sticky (01000) + 0777 = 07777
        fs::set_permissions(&exec_file, std::fs::Permissions::from_mode(0o7777)).unwrap();

        let dst = work.path().join("dst");
        copy_dir_all(&src, &dst).unwrap();

        let dst_mode = fs::metadata(dst.join("exec")).unwrap().permissions().mode() & 0o7777;
        // setuid/setgid/sticky stripped, group/world write stripped, exec
        // bit preserved → expect 0744.
        assert_eq!(dst_mode, 0o744, "expected 0o744, got 0o{:o}", dst_mode);
    }

    #[cfg(unix)]
    #[test]
    fn copy_dir_all_preserves_644_for_non_exec_files() {
        use std::os::unix::fs::PermissionsExt;

        let work = TempDir::new().unwrap();
        let src = work.path().join("src");
        fs::create_dir(&src).unwrap();
        let plain = src.join("data.txt");
        fs::write(&plain, "hello").unwrap();
        fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o600)).unwrap();

        let dst = work.path().join("dst");
        copy_dir_all(&src, &dst).unwrap();

        let dst_mode = fs::metadata(dst.join("data.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(dst_mode, 0o644, "expected 0o644, got 0o{:o}", dst_mode);
    }

    #[test]
    fn display_path_renders_home_as_tilde() {
        let home = Path::new("/Users/test");
        assert_eq!(
            display_path_with_home(Path::new("/Users/test/Library/Caches/foo"), Some(home)),
            "~/Library/Caches/foo"
        );
        assert_eq!(
            display_path_with_home(Path::new("/Users/test"), Some(home)),
            "~"
        );
    }

    #[test]
    fn display_path_leaves_paths_outside_home_alone() {
        let home = Path::new("/Users/test");
        assert_eq!(
            display_path_with_home(Path::new("/etc/passwd"), Some(home)),
            "/etc/passwd"
        );
        assert_eq!(
            display_path_with_home(Path::new("/Users/someone-else/file"), Some(home)),
            "/Users/someone-else/file"
        );
    }

    #[test]
    fn display_path_falls_back_when_home_unset() {
        assert_eq!(
            display_path_with_home(Path::new("/Users/test/foo"), None),
            "/Users/test/foo"
        );
    }

    #[test]
    fn swap_with_bak_moves_dst_to_bak_and_new_into_dst() {
        let work = TempDir::new().unwrap();
        let dst = work.path().join("dst");
        let bak = work.path().join("bak");
        let new_src = work.path().join("new-content");

        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("old"), "old-value").unwrap();
        fs::create_dir(&new_src).unwrap();
        fs::write(new_src.join("new"), "new-value").unwrap();

        swap_with_bak(&new_src, &dst, &bak).unwrap();
        assert!(dst.join("new").exists(), "new content should be at dst");
        assert!(!dst.join("old").exists());
        assert!(bak.join("old").exists(), "old content should be at bak");
        assert!(!new_src.exists(), "new_src is consumed by the rename");
    }
}

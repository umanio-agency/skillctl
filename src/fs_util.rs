use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::error::AppError;

/// Recursively copy `src` into `dst`. Refuses to follow symlinks: any symlink
/// (top-level `src`, or any descendant entry) is rejected with
/// `AppError::Config`. This prevents a malicious skill folder from
/// exfiltrating arbitrary files on the operator's machine via a crafted
/// symlink that would otherwise be dereferenced by `fs::copy` and later
/// pushed back to a public library by `skillctl push`.
pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    let src_meta = fs::symlink_metadata(src)
        .with_context(|| format!("reading metadata for {}", src.display()))?;
    if src_meta.file_type().is_symlink() {
        return Err(AppError::Config(format!(
            "refusing to copy `{}`: source is a symlink (skill folders may not be or contain symlinks)",
            src.display()
        ))
        .into());
    }
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
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
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// Replace the contents of `dst` with the contents of `src`.
/// Removes `dst` first if it exists, so any files only in `dst` are dropped.
///
/// Refuses to operate when `dst` is itself a symlink: a malicious symlink at
/// the destination (e.g. `.claude/skills` linked to `~/.ssh`) would otherwise
/// be followed by `remove_dir_all` and wipe arbitrary directories.
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
        fs::remove_dir_all(dst).with_context(|| format!("removing {}", dst.display()))?;
    }
    copy_dir_all(src, dst)
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
}

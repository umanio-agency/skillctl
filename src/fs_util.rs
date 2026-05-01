use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
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
pub fn replace_folder_contents(src: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
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

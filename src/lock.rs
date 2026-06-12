//! Process-level advisory file locks for shared mutable state.
//!
//! Two critical sections need single-writer serialisation:
//! - **Library cache** (`~/.cache/skills-cli/<owner>-<repo>/`). Every command
//!   that touches it performs `git fetch && reset --hard @{upstream}` then
//!   possibly `add` / `commit` / `push`. Two concurrent processes racing on
//!   `.git/index.lock` can leave the index in `fatal: index file corrupt`
//!   state.
//! - **`.skills.toml`** in a project. Read-modify-write across two
//!   concurrent `skillctl add` / `detect` runs would lose entries with the
//!   last writer winning.
//!
//! Both locks are exclusive (writer-only). A lock file is created next to
//! the resource it guards; on Unix `flock(LOCK_EX | LOCK_NB)` via `fs4`
//! gives us a process-level advisory lock that is auto-released on file
//! drop or process exit. If another process holds the lock, we fail fast
//! with a clear "another skillctl is running" message rather than blocking
//! indefinitely.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::Result;
use fs4::fs_std::FileExt;

use crate::error::AppError;

/// Name of the advisory lock file created next to a guarded resource. It lives
/// in the library-cache working tree while a command holds the lock, so
/// `git::is_clean` must ignore it (otherwise every refresh would see the cache
/// as dirty and skip the fetch).
pub const LOCK_FILE_NAME: &str = ".skillctl.lock";

/// Holds an exclusive flock on a lock file. The lock is released when this
/// value is dropped (or the process exits, since the OS owns the lock).
#[derive(Debug)]
pub struct LockGuard {
    _file: File,
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Best-effort cleanup of the lock file. The OS already released the
        // flock when `_file` dropped; the file itself is just a sentinel and
        // its removal is non-critical.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Acquire an exclusive advisory lock at `<dir>/.skillctl.lock`. Fails
/// immediately (no blocking) if another process already holds the lock.
pub fn acquire_exclusive(dir: &Path, what: &str) -> Result<LockGuard> {
    let lock_path = dir.join(LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            AppError::Config(format!(
                "could not open lock file at {}: {e}",
                lock_path.display()
            ))
        })?;
    file.try_lock_exclusive().map_err(|e| {
        AppError::Conflict(format!(
            "another `skillctl` process is operating on the {what} ({}); try again in a moment (error: {e})",
            lock_path.display()
        ))
    })?;
    Ok(LockGuard {
        _file: file,
        path: lock_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn acquire_creates_lock_file_and_drop_cleans_up() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".skillctl.lock");
        assert!(!lock_path.exists());
        {
            let _g = acquire_exclusive(dir.path(), "test").unwrap();
            assert!(lock_path.exists(), "lock file should exist while held");
        }
        // After drop, the lock file is removed (best-effort cleanup in Drop).
        assert!(!lock_path.exists(), "lock file should be cleaned up");
    }

    #[test]
    fn reacquire_after_drop_succeeds() {
        let dir = TempDir::new().unwrap();
        {
            let _g1 = acquire_exclusive(dir.path(), "test").unwrap();
        }
        let _g2 = acquire_exclusive(dir.path(), "test").unwrap();
    }

    // Note on cross-process behaviour: the in-process double-acquire test is
    // intentionally absent because `flock(2)` semantics differ across
    // platforms — on macOS (BSD `flock`) the same process can re-acquire
    // its own lock on a different fd; on Linux the second acquire is
    // typically denied. The contract we rely on is the *cross-process*
    // mutual exclusion guaranteed by `fs4::FileExt::try_lock_exclusive`,
    // which can only be exercised by spawning a subprocess. That test
    // lives in `tests/integration_lock.rs` (added separately).
}

//! Lexical path-safety helpers for joining untrusted relative subpaths.
//!
//! Threat model: an attacker controls the right-hand side of a `Path::join`
//! (e.g., a field deserialized from `.skills.toml`, a CLI flag, a path read
//! from a third-party library repo). The operator's local filesystem layout
//! is trusted. Filesystem-level concerns (symlinks inside copied trees) are
//! handled separately in `fs_util`.
//!
//! The validation is purely lexical: it inspects `std::path::Component` and
//! rejects anything that could escape the base — absolute markers
//! (`RootDir`, `Prefix`) and parent traversal (`..`). No filesystem calls,
//! so no TOCTOU window between check and use.

use std::path::{Component, Path, PathBuf};

use crate::error::AppError;

/// Validate that `p` is a relative subpath safe to join under any base
/// directory: no absolute markers, no `..`. `.` is allowed.
pub fn validate_relative_subpath(p: &Path) -> Result<(), AppError> {
    for component in p.components() {
        let reason = match component {
            Component::Normal(_) | Component::CurDir => continue,
            Component::ParentDir => "parent traversal `..` is not allowed",
            Component::RootDir => "absolute paths are not allowed",
            Component::Prefix(_) => "Windows path prefix is not allowed",
        };
        return Err(AppError::Config(format!(
            "rejected path `{}`: {reason}",
            p.display()
        )));
    }
    Ok(())
}

/// Lexically safe `Path::join`. Returns `base.join(untrusted)` only when
/// `untrusted` cannot escape `base` via absolute or `..` components.
///
/// Note: this does NOT canonicalize. If a parent of `base` is itself a
/// symlink, the returned path may resolve outside `base` at use time. That
/// is acceptable for the threat model — only the right-hand side is
/// adversarial; the operator's local fs layout is trusted. Symlinks inside
/// copied trees are rejected by `fs_util::copy_dir_all`.
pub fn safe_join(base: &Path, untrusted: impl AsRef<Path>) -> Result<PathBuf, AppError> {
    let untrusted = untrusted.as_ref();
    validate_relative_subpath(untrusted)?;
    Ok(base.join(untrusted))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(p: &str) {
        assert!(
            validate_relative_subpath(Path::new(p)).is_err(),
            "expected `{p}` to be rejected"
        );
    }

    fn accepted(p: &str) {
        assert!(
            validate_relative_subpath(Path::new(p)).is_ok(),
            "expected `{p}` to be accepted"
        );
    }

    #[test]
    fn rejects_absolute_unix() {
        rejected("/etc/passwd");
        rejected("/");
        rejected("/home/user/.ssh");
    }

    #[test]
    fn rejects_parent_traversal_at_start() {
        rejected("..");
        rejected("../escape");
        rejected("../../etc/passwd");
    }

    #[test]
    fn rejects_parent_traversal_in_middle() {
        rejected("foo/../bar");
        rejected("a/b/../../etc");
    }

    #[test]
    fn rejects_parent_traversal_at_end() {
        rejected("foo/..");
    }

    #[test]
    fn accepts_empty_path() {
        accepted("");
    }

    #[test]
    fn accepts_curdir_only() {
        accepted(".");
    }

    #[test]
    fn accepts_simple_relative() {
        accepted("foo");
        accepted("foo/bar");
        accepted("foo/bar/baz");
    }

    #[test]
    fn accepts_dotfile() {
        accepted(".claude/skills/foo");
        accepted(".skills.toml");
    }

    #[test]
    fn accepts_curdir_interleaved() {
        accepted("./foo");
        accepted("foo/./bar");
    }

    #[test]
    fn safe_join_combines_when_safe() {
        let joined = safe_join(Path::new("/tmp/base"), "subdir/file").unwrap();
        assert_eq!(joined, Path::new("/tmp/base/subdir/file"));
    }

    #[test]
    fn safe_join_rejects_absolute_rhs() {
        assert!(safe_join(Path::new("/tmp/base"), "/etc/passwd").is_err());
    }

    #[test]
    fn safe_join_rejects_parent_traversal() {
        assert!(safe_join(Path::new("/tmp/base"), "../escape").is_err());
        assert!(safe_join(Path::new("/tmp/base"), "ok/../escape").is_err());
    }

    #[test]
    fn safe_join_with_empty_returns_base() {
        let joined = safe_join(Path::new("/tmp/base"), "").unwrap();
        assert_eq!(joined, Path::new("/tmp/base"));
    }
}

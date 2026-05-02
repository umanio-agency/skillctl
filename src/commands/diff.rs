use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::git;
use crate::project_config::InstalledSkill;

/// Classification of an installed skill's local copy versus its library
/// counterpart. The same enum is used by `push` (which cares about pushing
/// up local edits) and `pull` (which cares about pulling down library
/// updates) — each command interprets the variants differently.
#[derive(Clone, Debug)]
pub enum SkillStatus {
    /// Local content matches `source_sha`, and the library at HEAD also
    /// matches `source_sha`. Nothing to do in either direction.
    Unchanged,
    /// Local has edits the library doesn't yet have. (`local != source`,
    /// `head == source`.) `push` candidate.
    LocalChangesOnly,
    /// The library has moved past `source_sha` while the local copy still
    /// matches it. (`local == source`, `head != source`.) `pull` candidate.
    LibraryAhead { library_changed: usize },
    /// Both sides moved past `source_sha`. Conflict. (`local != source`,
    /// `head != source`.)
    BothDiverged {
        local_changed: usize,
        library_changed: usize,
    },
    /// The local destination folder no longer exists.
    LocalMissing,
    /// The skill's `source_path` no longer exists at the library's HEAD.
    LibraryMissing,
}

pub fn classify(
    installed: &InstalledSkill,
    project_root: &Path,
    library_root: &Path,
) -> Result<SkillStatus> {
    let local_dir = project_root.join(&installed.destination);
    if !local_dir.exists() {
        return Ok(SkillStatus::LocalMissing);
    }

    let head_manifest = git::ls_tree_blobs(library_root, "HEAD", &installed.source_path)?;
    if head_manifest.is_empty() {
        return Ok(SkillStatus::LibraryMissing);
    }
    let source_manifest =
        git::ls_tree_blobs(library_root, &installed.source_sha, &installed.source_path)?;
    let local_manifest = local_blob_manifest(&local_dir, &installed.source_path)?;

    let local_eq_source = local_manifest == source_manifest;
    let head_eq_source = head_manifest == source_manifest;

    Ok(match (local_eq_source, head_eq_source) {
        (true, true) => SkillStatus::Unchanged,
        (true, false) => SkillStatus::LibraryAhead {
            library_changed: count_diff(&head_manifest, &source_manifest),
        },
        (false, true) => SkillStatus::LocalChangesOnly,
        (false, false) => SkillStatus::BothDiverged {
            local_changed: count_diff(&local_manifest, &source_manifest),
            library_changed: count_diff(&head_manifest, &source_manifest),
        },
    })
}

pub fn local_blob_manifest(
    local_dir: &Path,
    repo_relative_root: &Path,
) -> Result<HashMap<PathBuf, String>> {
    let walker = WalkBuilder::new(local_dir).hidden(false).build();
    let mut map = HashMap::new();
    for entry in walker {
        let entry = entry.context("walking the local skill folder")?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let abs = entry.path();
        let rel_to_skill = abs.strip_prefix(local_dir).with_context(|| {
            format!(
                "computing path of {} relative to {}",
                abs.display(),
                local_dir.display()
            )
        })?;
        let key = repo_relative_root.join(rel_to_skill);
        let sha = git::hash_object(abs)?;
        map.insert(key, sha);
    }
    Ok(map)
}

pub fn count_diff(a: &HashMap<PathBuf, String>, b: &HashMap<PathBuf, String>) -> usize {
    let mut count = 0usize;
    for (k, v) in a {
        if b.get(k) != Some(v) {
            count += 1;
        }
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, &str)]) -> HashMap<PathBuf, String> {
        entries
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect()
    }

    #[test]
    fn count_diff_zero_when_equal() {
        let a = map(&[("a", "1"), ("b", "2")]);
        let b = map(&[("a", "1"), ("b", "2")]);
        assert_eq!(count_diff(&a, &b), 0);
    }

    #[test]
    fn count_diff_added_in_a() {
        let a = map(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let b = map(&[("a", "1"), ("b", "2")]);
        assert_eq!(count_diff(&a, &b), 1);
    }

    #[test]
    fn count_diff_removed_from_a() {
        let a = map(&[("a", "1")]);
        let b = map(&[("a", "1"), ("b", "2")]);
        assert_eq!(count_diff(&a, &b), 1);
    }

    #[test]
    fn count_diff_changed_value() {
        let a = map(&[("a", "1")]);
        let b = map(&[("a", "9")]);
        assert_eq!(count_diff(&a, &b), 1);
    }

    #[test]
    fn count_diff_combined() {
        let a = map(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let b = map(&[("a", "1"), ("b", "9"), ("d", "4")]);
        assert_eq!(count_diff(&a, &b), 3);
    }
}

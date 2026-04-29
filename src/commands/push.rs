use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use cliclack::{intro, log, multiselect, outro, select};
use ignore::WalkBuilder;

use crate::cli::PushArgs;
use crate::config;
use crate::fs_util;
use crate::git;
use crate::project_config::{self, InstalledSkill};

#[derive(Clone, Debug)]
enum SkillStatus {
    Unchanged,
    LocalChangesOnly,
    BothDiverged {
        local_changed: usize,
        library_changed: usize,
    },
    LocalMissing,
    LibraryMissing,
}

struct Candidate {
    index: usize,
    name: String,
    destination: PathBuf,
    status: SkillStatus,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DivergenceAction {
    Overwrite,
    Skip,
}

pub fn run(_args: PushArgs) -> Result<()> {
    intro("skills push")?;

    let cfg = config::load()?;
    let library = cfg
        .library
        .ok_or_else(|| anyhow!("no library configured — run `skills init <github-url>` first"))?;

    let library_root = config::library_cache_path(&library.url)?;
    if !library_root.exists() {
        return Err(anyhow!(
            "library cache not found at {} — run `skills init {}` again",
            library_root.display(),
            library.url
        ));
    }

    if let Err(e) = git::fetch_and_fast_forward(&library_root) {
        log::warning(format!(
            "could not refresh library cache ({e}); diff is computed against the cached HEAD"
        ))?;
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let mut project_cfg = project_config::load(&cwd)?;
    if project_cfg.installed.is_empty() {
        outro("no skills installed in this project (.skills.toml is empty)")?;
        return Ok(());
    }

    let mut candidates = Vec::new();
    for (index, installed) in project_cfg.installed.iter().enumerate() {
        let status = classify(installed, &cwd, &library_root)?;
        candidates.push(Candidate {
            index,
            name: installed.name.clone(),
            destination: installed.destination.clone(),
            status,
        });
    }

    let pushable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                SkillStatus::LocalChangesOnly | SkillStatus::BothDiverged { .. }
            )
        })
        .collect();

    for c in &candidates {
        match &c.status {
            SkillStatus::Unchanged => log::info(format!("{} — no local changes", c.name))?,
            SkillStatus::LocalMissing => log::warning(format!(
                "{} — destination {} no longer exists; skipping",
                c.name,
                c.destination.display()
            ))?,
            SkillStatus::LibraryMissing => log::warning(format!(
                "{} — source path no longer exists in library; fork support arrives in Phase 5",
                c.name
            ))?,
            _ => {}
        }
    }

    if pushable.is_empty() {
        outro("nothing to push")?;
        return Ok(());
    }

    let mut prompt = multiselect("Skills to push").required(true);
    for c in &pushable {
        let hint = describe(&c.status);
        prompt = prompt.item(c.index, &c.name, hint);
    }
    let selected_indices: Vec<usize> = prompt.interact()?;

    let mut to_apply: Vec<&Candidate> = Vec::new();
    for idx in &selected_indices {
        let candidate = pushable
            .iter()
            .find(|c| c.index == *idx)
            .copied()
            .ok_or_else(|| anyhow!("selected index {idx} not in pushable set"))?;

        if let SkillStatus::BothDiverged {
            local_changed,
            library_changed,
        } = &candidate.status
        {
            let action = select(format!(
                "`{}` diverged ({} file(s) changed locally, {} in library) — what do you want to do?",
                candidate.name, local_changed, library_changed
            ))
            .item(
                DivergenceAction::Overwrite,
                "Overwrite library",
                "force the local version onto the library, discarding library-side changes",
            )
            .item(
                DivergenceAction::Skip,
                "Skip",
                "leave this skill untouched on both sides",
            )
            .interact()?;
            match action {
                DivergenceAction::Overwrite => to_apply.push(candidate),
                DivergenceAction::Skip => {
                    log::info(format!("skipped {}", candidate.name))?;
                }
            }
        } else {
            to_apply.push(candidate);
        }
    }

    if to_apply.is_empty() {
        outro("nothing to push after conflict resolution")?;
        return Ok(());
    }

    for candidate in &to_apply {
        let installed = &project_cfg.installed[candidate.index];
        let local_dir = cwd.join(&installed.destination);
        let library_dir = library_root.join(&installed.source_path);
        fs_util::replace_folder_contents(&local_dir, &library_dir)?;
        git::add_all(&library_root, &installed.source_path)?;
    }

    if !git::has_staged_changes(&library_root)? {
        outro("no effective changes after applying selections")?;
        return Ok(());
    }

    let names: Vec<&str> = to_apply.iter().map(|c| c.name.as_str()).collect();
    let message = if names.len() == 1 {
        format!("update skill: {}", names[0])
    } else {
        format!("update skills: {}", names.join(", "))
    };

    let new_sha = git::commit(&library_root, &message)?;
    git::push(&library_root)?;

    for candidate in &to_apply {
        project_cfg.installed[candidate.index].source_sha = new_sha.clone();
        log::success(format!("{} → {}", candidate.name, &new_sha[..7.min(new_sha.len())]))?;
    }
    project_config::save(&cwd, &project_cfg)?;

    let summary = match (to_apply.len(), selected_indices.len() - to_apply.len()) {
        (n, 0) => format!("pushed {n} skill(s)"),
        (n, s) => format!("pushed {n}, skipped {s}"),
    };
    outro(summary)?;
    Ok(())
}

fn classify(installed: &InstalledSkill, project_root: &Path, library_root: &Path) -> Result<SkillStatus> {
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
        (true, _) => SkillStatus::Unchanged,
        (false, true) => SkillStatus::LocalChangesOnly,
        (false, false) => SkillStatus::BothDiverged {
            local_changed: count_diff(&local_manifest, &source_manifest),
            library_changed: count_diff(&head_manifest, &source_manifest),
        },
    })
}

fn local_blob_manifest(
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

fn count_diff(a: &HashMap<PathBuf, String>, b: &HashMap<PathBuf, String>) -> usize {
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

fn describe(status: &SkillStatus) -> String {
    match status {
        SkillStatus::LocalChangesOnly => "local edits, library unchanged".to_string(),
        SkillStatus::BothDiverged {
            local_changed,
            library_changed,
        } => format!("diverged: {local_changed} local, {library_changed} in library"),
        SkillStatus::Unchanged => "no local changes".to_string(),
        SkillStatus::LocalMissing => "destination missing locally".to_string(),
        SkillStatus::LibraryMissing => "removed from library".to_string(),
    }
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
        // changed: b. added in a: c. removed from a: d. = 3
        assert_eq!(count_diff(&a, &b), 3);
    }
}

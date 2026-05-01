use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use cliclack::{input, intro, log, multiselect, outro, select};
use ignore::WalkBuilder;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::{OnDivergence, PushArgs};
use crate::config;
use crate::context::Context;
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

#[derive(Clone, Debug)]
struct Candidate {
    index: usize,
    name: String,
    destination: PathBuf,
    status: SkillStatus,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DivergenceChoice {
    Overwrite,
    Fork,
    Skip,
}

impl From<OnDivergence> for DivergenceChoice {
    fn from(v: OnDivergence) -> Self {
        match v {
            OnDivergence::Overwrite => Self::Overwrite,
            OnDivergence::Skip => Self::Skip,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum LibMissingChoice {
    Fork,
    Skip,
}

struct Apply {
    candidate_index: usize,
    op: ApplyOp,
}

enum ApplyOp {
    Update,
    Fork {
        new_name: String,
        new_library_path: PathBuf,
        new_local_destination: PathBuf,
    },
}

pub fn run(args: PushArgs, ctx: &Context) -> Result<()> {
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

    for c in &candidates {
        match &c.status {
            SkillStatus::Unchanged => log::info(format!("{} — no local changes", c.name))?,
            SkillStatus::LocalMissing => log::warning(format!(
                "{} — destination {} no longer exists; skipping",
                c.name,
                c.destination.display()
            ))?,
            _ => {}
        }
    }

    let pushable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                SkillStatus::LocalChangesOnly
                    | SkillStatus::BothDiverged { .. }
                    | SkillStatus::LibraryMissing
            )
        })
        .collect();

    if pushable.is_empty() {
        outro("nothing to push")?;
        return Ok(());
    }

    let selected_indices = select_pushable(&args, ctx, &pushable)?;
    if selected_indices.is_empty() {
        outro("no skills selected")?;
        return Ok(());
    }

    let mut applies: Vec<Apply> = Vec::new();
    for idx in &selected_indices {
        let candidate = pushable
            .iter()
            .find(|c| c.index == *idx)
            .copied()
            .ok_or_else(|| anyhow!("selected index {idx} not in pushable set"))?;
        let installed = &project_cfg.installed[candidate.index];

        let op = match &candidate.status {
            SkillStatus::LocalChangesOnly => Some(ApplyOp::Update),
            SkillStatus::BothDiverged {
                local_changed,
                library_changed,
            } => {
                let choice = if let Some(policy) = args.on_divergence {
                    DivergenceChoice::from(policy)
                } else if !ctx.interactive {
                    log::warning(format!(
                        "{} diverged but no --on-divergence policy provided; skipping",
                        candidate.name
                    ))?;
                    DivergenceChoice::Skip
                } else {
                    select(format!(
                        "`{}` diverged ({} file(s) changed locally, {} in library) — what do you want to do?",
                        candidate.name, local_changed, library_changed
                    ))
                    .item(
                        DivergenceChoice::Overwrite,
                        "Overwrite library",
                        "force the local version onto the library, discarding library-side changes",
                    )
                    .item(
                        DivergenceChoice::Fork,
                        "Fork as new skill",
                        "create a new skill in the library from the local content; the original stays untouched",
                    )
                    .item(
                        DivergenceChoice::Skip,
                        "Skip",
                        "leave this skill untouched on both sides",
                    )
                    .interact()?
                };
                match choice {
                    DivergenceChoice::Overwrite => Some(ApplyOp::Update),
                    DivergenceChoice::Fork => Some(prompt_fork_op(installed, &library_root)?),
                    DivergenceChoice::Skip => {
                        log::info(format!("skipped {}", candidate.name))?;
                        None
                    }
                }
            }
            SkillStatus::LibraryMissing => {
                let choice = if let Some(policy) = args.on_divergence {
                    // For LibraryMissing: only fork makes sense (no library to overwrite).
                    // Skip policy → Skip; Overwrite policy → also treated as Skip (no source path
                    // to overwrite at), with a warning.
                    match policy {
                        OnDivergence::Skip => LibMissingChoice::Skip,
                        OnDivergence::Overwrite => {
                            log::warning(format!(
                                "{} is removed from the library; --on-divergence overwrite cannot apply (use the interactive flow for fork)",
                                candidate.name
                            ))?;
                            LibMissingChoice::Skip
                        }
                    }
                } else if !ctx.interactive {
                    log::warning(format!(
                        "{} is removed from the library and fork is interactive-only; skipping",
                        candidate.name
                    ))?;
                    LibMissingChoice::Skip
                } else {
                    select(format!(
                        "`{}` no longer exists in the library — what do you want to do?",
                        candidate.name
                    ))
                    .item(
                        LibMissingChoice::Fork,
                        "Fork as new skill",
                        "push the local content back as a new skill",
                    )
                    .item(
                        LibMissingChoice::Skip,
                        "Skip",
                        "leave this skill untracked",
                    )
                    .interact()?
                };
                match choice {
                    LibMissingChoice::Fork => Some(prompt_fork_op(installed, &library_root)?),
                    LibMissingChoice::Skip => {
                        log::info(format!("skipped {}", candidate.name))?;
                        None
                    }
                }
            }
            _ => None,
        };

        if let Some(op) = op {
            applies.push(Apply {
                candidate_index: candidate.index,
                op,
            });
        }
    }

    if applies.is_empty() {
        outro("nothing to push after conflict resolution")?;
        return Ok(());
    }

    for apply in &applies {
        let installed = &project_cfg.installed[apply.candidate_index];
        let local_dir = cwd.join(&installed.destination);
        let (library_dir, library_relative) = match &apply.op {
            ApplyOp::Update => (
                library_root.join(&installed.source_path),
                installed.source_path.clone(),
            ),
            ApplyOp::Fork {
                new_library_path, ..
            } => (
                library_root.join(new_library_path),
                new_library_path.clone(),
            ),
        };
        fs_util::replace_folder_contents(&local_dir, &library_dir)?;
        git::add_all(&library_root, &library_relative)?;
    }

    if !git::has_staged_changes(&library_root)? {
        outro("no effective changes after applying selections")?;
        return Ok(());
    }

    let updates: Vec<&str> = applies
        .iter()
        .filter(|a| matches!(a.op, ApplyOp::Update))
        .map(|a| project_cfg.installed[a.candidate_index].name.as_str())
        .collect();
    let adds: Vec<&str> = applies
        .iter()
        .filter_map(|a| match &a.op {
            ApplyOp::Fork { new_name, .. } => Some(new_name.as_str()),
            _ => None,
        })
        .collect();
    let message = args
        .message
        .clone()
        .unwrap_or_else(|| build_commit_message(&updates, &adds));

    let new_sha = git::commit(&library_root, &message)?;
    git::push(&library_root)?;

    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    let mut updated_count = 0usize;
    let mut forked_count = 0usize;

    for apply in &applies {
        match &apply.op {
            ApplyOp::Update => {
                let entry = &mut project_cfg.installed[apply.candidate_index];
                entry.source_sha = new_sha.clone();
                log::success(format!(
                    "{} → {}",
                    entry.name,
                    short_sha(&new_sha)
                ))?;
                updated_count += 1;
            }
            ApplyOp::Fork {
                new_name,
                new_library_path,
                new_local_destination,
            } => {
                let abs_old = cwd.join(&project_cfg.installed[apply.candidate_index].destination);
                let abs_new = cwd.join(new_local_destination);
                if abs_old != abs_new {
                    if let Some(parent) = abs_new.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("creating parent of {}", abs_new.display())
                        })?;
                    }
                    fs::rename(&abs_old, &abs_new).with_context(|| {
                        format!("renaming {} -> {}", abs_old.display(), abs_new.display())
                    })?;
                }
                project_cfg.installed[apply.candidate_index] = InstalledSkill {
                    name: new_name.clone(),
                    source_path: new_library_path.clone(),
                    source_sha: new_sha.clone(),
                    destination: new_local_destination.clone(),
                    installed_at: installed_at.clone(),
                };
                log::success(format!(
                    "forked → {} ({})",
                    new_name,
                    short_sha(&new_sha)
                ))?;
                forked_count += 1;
            }
        }
    }
    project_config::save(&cwd, &project_cfg)?;

    let total = updated_count + forked_count;
    let skipped = selected_indices.len() - total;
    let summary = match (updated_count, forked_count, skipped) {
        (u, 0, 0) => format!("pushed {u} skill(s)"),
        (0, f, 0) => format!("forked {f} skill(s)"),
        (u, f, 0) => format!("pushed {u}, forked {f}"),
        (u, 0, s) => format!("pushed {u}, skipped {s}"),
        (0, f, s) => format!("forked {f}, skipped {s}"),
        (u, f, s) => format!("pushed {u}, forked {f}, skipped {s}"),
    };
    outro(summary)?;
    Ok(())
}

fn select_pushable(
    args: &PushArgs,
    ctx: &Context,
    pushable: &[&Candidate],
) -> Result<Vec<usize>> {
    if args.all {
        return Ok(pushable.iter().map(|c| c.index).collect());
    }
    if !args.skills.is_empty() {
        let mut chosen = Vec::with_capacity(args.skills.len());
        for name in &args.skills {
            let candidate = pushable
                .iter()
                .find(|c| c.name == *name)
                .ok_or_else(|| {
                    anyhow!("no pushable skill named `{name}` (skill is unchanged, missing locally, or unknown)")
                })?;
            chosen.push(candidate.index);
        }
        return Ok(chosen);
    }
    if !ctx.interactive {
        return Err(anyhow!(
            "no skills selected — pass --skill <name> (repeatable) or --all"
        ));
    }
    let mut prompt = multiselect("Skills to push").required(true);
    for c in pushable {
        let hint = describe(&c.status);
        prompt = prompt.item(c.index, &c.name, hint);
    }
    Ok(prompt.interact()?)
}

fn prompt_fork_op(installed: &InstalledSkill, library_root: &Path) -> Result<ApplyOp> {
    let raw_name: String = input("New skill name")
        .placeholder("foo-custom")
        .validate(|s: &String| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err("name cannot be empty");
            }
            if trimmed.contains('/') || trimmed.contains('\\') {
                return Err("name cannot contain `/` or `\\`");
            }
            Ok(())
        })
        .interact()?;
    let new_name = raw_name.trim().to_string();

    let library_parent = installed
        .source_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    let new_library_path = if library_parent.as_os_str().is_empty() {
        PathBuf::from(&new_name)
    } else {
        library_parent.join(&new_name)
    };

    if library_root.join(&new_library_path).exists() {
        return Err(anyhow!(
            "a folder already exists at {} in the library — pick a different name",
            new_library_path.display()
        ));
    }

    let local_parent = installed
        .destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    let new_local_destination = if local_parent.as_os_str().is_empty() {
        PathBuf::from(&new_name)
    } else {
        local_parent.join(&new_name)
    };

    Ok(ApplyOp::Fork {
        new_name,
        new_library_path,
        new_local_destination,
    })
}

fn build_commit_message(updates: &[&str], adds: &[&str]) -> String {
    match (updates.is_empty(), adds.is_empty()) {
        (false, true) => {
            if updates.len() == 1 {
                format!("update skill: {}", updates[0])
            } else {
                format!("update skills: {}", updates.join(", "))
            }
        }
        (true, false) => {
            if adds.len() == 1 {
                format!("add skill: {}", adds[0])
            } else {
                format!("add skills: {}", adds.join(", "))
            }
        }
        (false, false) => format!(
            "sync skills\n\nUpdate: {}\nAdd: {}",
            updates.join(", "),
            adds.join(", ")
        ),
        _ => "sync skills".to_string(),
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..7.min(sha.len())]
}

fn classify(
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
        assert_eq!(count_diff(&a, &b), 3);
    }

    #[test]
    fn commit_message_single_update() {
        assert_eq!(build_commit_message(&["foo"], &[]), "update skill: foo");
    }

    #[test]
    fn commit_message_multi_update() {
        assert_eq!(
            build_commit_message(&["foo", "bar"], &[]),
            "update skills: foo, bar"
        );
    }

    #[test]
    fn commit_message_single_add() {
        assert_eq!(build_commit_message(&[], &["fork"]), "add skill: fork");
    }

    #[test]
    fn commit_message_multi_add() {
        assert_eq!(
            build_commit_message(&[], &["a", "b"]),
            "add skills: a, b"
        );
    }

    #[test]
    fn commit_message_mixed_uses_body() {
        let msg = build_commit_message(&["foo"], &["bar"]);
        assert!(msg.starts_with("sync skills\n"));
        assert!(msg.contains("Update: foo"));
        assert!(msg.contains("Add: bar"));
    }
}

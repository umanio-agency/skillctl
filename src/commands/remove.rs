use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use cliclack::confirm;
use serde_json::{Value, json};

use crate::cli::RemoveArgs;
use crate::context::Context;
use crate::error::AppError;
use crate::fs_util;
use crate::lock;
use crate::path_safety::{normalize_lexical, safe_join};
use crate::project_config::{self, InstalledSkill};
use crate::prompt::multiselect;
use crate::skill;
use crate::ui;

/// One removable unit in the current project. A candidate is either a skill
/// folder present on disk (tracked by skillctl or not) or an orphaned
/// `.skills.toml` entry whose destination folder no longer exists.
#[derive(Clone, Debug)]
struct Candidate {
    /// Display name (frontmatter name for on-disk skills, manifest name for orphans).
    name: String,
    /// Path relative to the project root, used for display and matching.
    rel_path: PathBuf,
    /// `Some` when a real directory exists to delete. `None` for orphans and
    /// for symlinked destinations (we never follow a symlink to delete).
    abs_path: Option<PathBuf>,
    /// Index into `ProjectConfig::installed` when this skill is skillctl-tracked.
    installed_index: Option<usize>,
}

impl Candidate {
    fn is_tracked(&self) -> bool {
        self.installed_index.is_some()
    }

    /// Orphan = recorded in `.skills.toml` but no folder on disk.
    fn is_orphan(&self) -> bool {
        self.abs_path.is_none() && self.installed_index.is_some()
    }

    fn hint(&self) -> String {
        let p = self.rel_path.display();
        if self.is_orphan() {
            format!("{p} — orphan: .skills.toml entry, folder already gone")
        } else if self.is_tracked() {
            format!("{p} — installed via skillctl")
        } else {
            format!("{p} — created locally, not tracked")
        }
    }
}

pub fn run(args: RemoveArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl remove")?;

    let cwd = std::env::current_dir().context("reading current directory")?;
    // Serialise concurrent skillctl runs on this project's .skills.toml.
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    let mut project_cfg = project_config::load(&cwd)?;

    let discovered = skill::discover(&cwd, false)?;
    for w in &discovered.warnings {
        ui::log_warning(ctx, w)?;
    }

    let candidates = build_candidates(&cwd, discovered.skills, &project_cfg.installed)?;
    // Only short-circuit when the operator hasn't named specific skills. With
    // `--skill X` on an empty project we fall through so `select_by_names` can
    // report `no skill named X` rather than a silent success (matches `add`).
    if candidates.is_empty() && args.skills.is_empty() {
        ui::outro(ctx, "no removable skills found in this project")?;
        emit_json(ctx, &[]);
        return Ok(());
    }

    let selected = select_candidates(&args, ctx, &candidates)?;
    if selected.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, &[]);
        return Ok(());
    }

    // Destructive: in an interactive session, confirm before deleting. In
    // non-interactive / --json mode the explicit --skill/--all flags are the
    // authorisation; there is no prompt to fall back to.
    if ctx.interactive && !confirm_removal(&candidates, &selected)? {
        ui::outro_cancel(ctx, "aborted")?;
        emit_json(ctx, &[]);
        return Ok(());
    }

    let mut entries_to_drop: HashSet<usize> = HashSet::new();
    let mut results: Vec<Value> = Vec::new();

    for &ci in &selected {
        let c = &candidates[ci];
        let rel_display = c.rel_path.display().to_string();

        if let Some(abs) = &c.abs_path {
            if let Err(reason) = delete_skill_dir(abs) {
                ui::log_warning(ctx, format!("remove failed for `{}`: {reason}", c.name))?;
                results.push(json!({
                    "name": c.name,
                    "status": "failed",
                    "reason": reason,
                }));
                continue;
            }
        }

        let removed_folder = c.abs_path.is_some();
        let mut removed_entry = false;
        if let Some(i) = c.installed_index {
            entries_to_drop.insert(i);
            removed_entry = true;
        }

        ui::log_success(ctx, format!("removed {}", c.name))?;
        results.push(json!({
            "name": c.name,
            "status": "removed",
            "path": rel_display,
            "removed_folder": removed_folder,
            "removed_entry": removed_entry,
        }));
    }

    // Only rewrite .skills.toml when an entry actually changed — removing an
    // untracked local folder leaves the manifest untouched.
    if !entries_to_drop.is_empty() {
        let kept: Vec<InstalledSkill> = std::mem::take(&mut project_cfg.installed)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !entries_to_drop.contains(i))
            .map(|(_, e)| e)
            .collect();
        project_cfg.installed = kept;
        project_config::save(&cwd, &project_cfg)?;
    }

    ui::outro(ctx, summary_text(&results))?;
    emit_json(ctx, &results);
    Ok(())
}

/// Correlate skills discovered on disk with `.skills.toml` entries into a
/// single, de-duplicated candidate list.
fn build_candidates(
    cwd: &std::path::Path,
    discovered: Vec<skill::Skill>,
    installed: &[InstalledSkill],
) -> Result<Vec<Candidate>> {
    // Normalised destination → installed index, for tracked-state lookup.
    let mut dest_to_installed: HashMap<PathBuf, usize> = HashMap::new();
    for (i, e) in installed.iter().enumerate() {
        dest_to_installed.insert(normalize_lexical(&e.destination), i);
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut covered: HashSet<PathBuf> = HashSet::new();

    for s in discovered {
        let rel = fs_util::relative_to_or_self(&s.path, cwd);
        let key = normalize_lexical(&rel);
        // A SKILL.md at the project root would resolve to an empty key; never
        // offer to delete the project root itself.
        if key.as_os_str().is_empty() {
            continue;
        }
        let installed_index = dest_to_installed.get(&key).copied();
        covered.insert(key);
        candidates.push(Candidate {
            name: s.name,
            rel_path: rel,
            abs_path: Some(s.path),
            installed_index,
        });
    }

    // Installed entries with no matching on-disk folder: either orphaned
    // (folder gone) or living under a .gitignore'd path that `discover`
    // skipped. Resolve the destination safely and only mark it deletable when
    // it is a real directory — `symlink_metadata` does not follow the final
    // component, so a symlinked destination is treated as "no folder" and we
    // never delete through it.
    for (i, e) in installed.iter().enumerate() {
        let key = normalize_lexical(&e.destination);
        if key.as_os_str().is_empty() || covered.contains(&key) {
            continue;
        }
        covered.insert(key);
        let abs = safe_join(cwd, &e.destination)?;
        let abs_path = match fs::symlink_metadata(&abs) {
            Ok(md) if md.is_dir() => Some(abs),
            _ => None,
        };
        candidates.push(Candidate {
            name: e.name.clone(),
            rel_path: e.destination.clone(),
            abs_path,
            installed_index: Some(i),
        });
    }

    candidates.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(candidates)
}

/// Delete a skill folder, refusing to follow a symlink. The symlink/dir check
/// is performed here, immediately before the unlink, rather than relying on the
/// `symlink_metadata` probe done at candidate-building time: a folder swapped
/// for a symlink in between (the project tree is a semi-trusted surface) must
/// not be able to redirect the recursive delete outside the project. This keeps
/// the safety property in our code instead of depending on `remove_dir_all`'s
/// internal handling of a symlinked top-level path.
fn delete_skill_dir(abs: &std::path::Path) -> Result<(), String> {
    let md = fs::symlink_metadata(abs).map_err(|e| e.to_string())?;
    if md.file_type().is_symlink() {
        return Err("destination is a symlink; refusing to delete through it".into());
    }
    if !md.is_dir() {
        return Err("destination is no longer a directory".into());
    }
    fs::remove_dir_all(abs).map_err(|e| e.to_string())
}

fn select_candidates(
    args: &RemoveArgs,
    ctx: &Context,
    candidates: &[Candidate],
) -> Result<Vec<usize>> {
    if args.all {
        return Ok((0..candidates.len()).collect());
    }
    if !args.skills.is_empty() {
        return select_by_names(candidates, &args.skills);
    }
    if !ctx.interactive {
        return Err(AppError::Config(
            "no skills selected — pass --skill <name> (repeatable) or --all".into(),
        )
        .into());
    }
    let mut prompt = multiselect::<usize>("Skills to remove").required(true);
    for (i, c) in candidates.iter().enumerate() {
        prompt = prompt.item(i, &c.name, c.hint(), Vec::new());
    }
    prompt.interact()
}

/// Resolve `--skill NAME` values to candidate indices. Errors on an unknown
/// name, and on an ambiguous one (two skills share the name) so the operator
/// disambiguates by hand rather than silently removing the wrong folder.
fn select_by_names(candidates: &[Candidate], names: &[String]) -> Result<Vec<usize>> {
    let mut chosen = Vec::with_capacity(names.len());
    for name in names {
        let matches: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name == *name)
            .map(|(i, _)| i)
            .collect();
        match matches.as_slice() {
            [] => {
                return Err(AppError::Config(format!(
                    "no skill named `{name}` found in this project"
                ))
                .into());
            }
            [i] => chosen.push(*i),
            many => {
                let paths: Vec<String> = many
                    .iter()
                    .map(|&i| candidates[i].rel_path.display().to_string())
                    .collect();
                return Err(AppError::Config(format!(
                    "multiple skills named `{name}` in this project ({}); remove them interactively to disambiguate",
                    paths.join(", ")
                ))
                .into());
            }
        }
    }
    Ok(chosen)
}

fn confirm_removal(candidates: &[Candidate], selected: &[usize]) -> Result<bool> {
    let folders = selected
        .iter()
        .filter(|&&i| candidates[i].abs_path.is_some())
        .count();
    let entries = selected
        .iter()
        .filter(|&&i| candidates[i].is_tracked())
        .count();

    let mut what = Vec::new();
    if folders > 0 {
        what.push(format!("delete {folders} folder(s)"));
    }
    if entries > 0 {
        what.push(format!("drop {entries} .skills.toml entr(y/ies)"));
    }
    let action = what.join(" and ");

    Ok(confirm(format!("This will {action}. Continue?"))
        .initial_value(false)
        .interact()?)
}

fn summary_text(results: &[Value]) -> String {
    let removed = results.iter().filter(|r| r["status"] == "removed").count();
    let failed = results.iter().filter(|r| r["status"] == "failed").count();
    if failed > 0 {
        format!("{removed} removed, {failed} failed")
    } else {
        format!("{removed} skill(s) removed")
    }
}

fn emit_json(ctx: &Context, results: &[Value]) {
    if !ctx.json {
        return;
    }
    let removed = results.iter().filter(|r| r["status"] == "removed").count();
    let failed = results.iter().filter(|r| r["status"] == "failed").count();
    let out = json!({
        "command": "remove",
        "results": results,
        "summary": {
            "removed": removed,
            "failed": failed,
        },
    });
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, rel: &str, has_folder: bool, tracked: Option<usize>) -> Candidate {
        Candidate {
            name: name.to_string(),
            rel_path: PathBuf::from(rel),
            abs_path: has_folder.then(|| PathBuf::from("/abs").join(rel)),
            installed_index: tracked,
        }
    }

    #[test]
    fn hint_distinguishes_kinds() {
        assert!(
            cand("a", ".claude/skills/a", true, Some(0))
                .hint()
                .contains("installed via skillctl")
        );
        assert!(
            cand("b", ".claude/skills/b", true, None)
                .hint()
                .contains("created locally")
        );
        assert!(
            cand("c", ".claude/skills/c", false, Some(1))
                .hint()
                .contains("orphan")
        );
    }

    #[test]
    fn orphan_requires_missing_folder_and_tracking() {
        assert!(cand("c", "x", false, Some(0)).is_orphan());
        assert!(!cand("c", "x", true, Some(0)).is_orphan());
        assert!(!cand("c", "x", false, None).is_orphan());
    }

    #[test]
    fn select_by_names_resolves_unique() {
        let cands = vec![
            cand("alpha", "skills/alpha", true, None),
            cand("beta", "skills/beta", true, Some(0)),
        ];
        let idx = select_by_names(&cands, &["beta".to_string()]).unwrap();
        assert_eq!(idx, vec![1]);
    }

    #[test]
    fn select_by_names_rejects_unknown() {
        let cands = vec![cand("alpha", "skills/alpha", true, None)];
        let err = select_by_names(&cands, &["ghost".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("no skill named `ghost`"), "got: {err}");
    }

    #[test]
    fn select_by_names_rejects_ambiguous() {
        let cands = vec![
            cand("dup", "a/dup", true, None),
            cand("dup", "b/dup", true, Some(0)),
        ];
        let err = select_by_names(&cands, &["dup".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("multiple skills named `dup`"), "got: {err}");
    }
}

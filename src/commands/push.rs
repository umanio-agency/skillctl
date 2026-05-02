use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use cliclack::{input, multiselect, select};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::{OnDivergence, PushArgs};
use crate::commands::diff::{SkillStatus, classify};
use crate::config;
use crate::context::Context;
use crate::error::AppError;
use crate::fs_util;
use crate::git;
use crate::project_config::{self, InstalledSkill};
use crate::ui;

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
    ui::intro(ctx, "skills push")?;

    let cfg = config::load()?;
    let library = cfg.library.ok_or_else(|| {
        AppError::Config("no library configured — run `skills init <github-url>` first".into())
    })?;

    let library_root = config::library_cache_path(&library.url)
        .map_err(|e| AppError::Config(e.to_string()))?;
    if !library_root.exists() {
        return Err(AppError::Config(format!(
            "library cache not found at {} — run `skills init {}` again",
            library_root.display(),
            library.url
        ))
        .into());
    }

    if let Err(e) = git::fetch_and_fast_forward(&library_root) {
        ui::log_warning(
            ctx,
            format!("could not refresh library cache ({e}); diff is computed against the cached HEAD"),
        )?;
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let mut project_cfg = project_config::load(&cwd)?;
    if project_cfg.installed.is_empty() {
        ui::outro(ctx, "no skills installed in this project (.skills.toml is empty)")?;
        emit_json(ctx, &[], None);
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
            SkillStatus::Unchanged => {
                ui::log_info(ctx, format!("{} — no local changes", c.name))?
            }
            SkillStatus::LibraryAhead { .. } => ui::log_info(
                ctx,
                format!("{} — library has updates (run `skills pull`)", c.name),
            )?,
            SkillStatus::LocalMissing => ui::log_warning(
                ctx,
                format!(
                    "{} — destination {} no longer exists; skipping",
                    c.name,
                    c.destination.display()
                ),
            )?,
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
        ui::outro(ctx, "nothing to push")?;
        emit_json(ctx, &[], None);
        return Ok(());
    }

    let selected_indices = select_pushable(&args, ctx, &pushable)?;
    if selected_indices.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, &[], None);
        return Ok(());
    }

    let mut results: Vec<Value> = Vec::new();
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
                    ui::log_warning(
                        ctx,
                        format!(
                            "{} diverged but no --on-divergence policy provided; skipping",
                            candidate.name
                        ),
                    )?;
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
                        ui::log_info(ctx, format!("skipped {}", candidate.name))?;
                        results.push(json!({
                            "name": candidate.name,
                            "status": "skipped",
                            "reason": "diverged; --on-divergence skip",
                        }));
                        None
                    }
                }
            }
            SkillStatus::LibraryMissing => {
                let choice = if let Some(policy) = args.on_divergence {
                    match policy {
                        OnDivergence::Skip => LibMissingChoice::Skip,
                        OnDivergence::Overwrite => {
                            ui::log_warning(ctx, format!(
                                "{} is removed from the library; --on-divergence overwrite cannot apply (use the interactive flow for fork)",
                                candidate.name
                            ))?;
                            LibMissingChoice::Skip
                        }
                    }
                } else if !ctx.interactive {
                    ui::log_warning(
                        ctx,
                        format!(
                            "{} is removed from the library and fork is interactive-only; skipping",
                            candidate.name
                        ),
                    )?;
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
                        ui::log_info(ctx, format!("skipped {}", candidate.name))?;
                        results.push(json!({
                            "name": candidate.name,
                            "status": "skipped",
                            "reason": "removed from library; fork is interactive-only",
                        }));
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
        ui::outro(ctx, "nothing to push after conflict resolution")?;
        emit_json(ctx, &results, None);
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
        git::add_all(&library_root, &library_relative)
            .map_err(|e| AppError::Git(e.to_string()))?;
    }

    if !git::has_staged_changes(&library_root)
        .map_err(|e| AppError::Git(e.to_string()))?
    {
        ui::outro(ctx, "no effective changes after applying selections")?;
        emit_json(ctx, &results, None);
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

    let new_sha =
        git::commit(&library_root, &message).map_err(|e| AppError::Git(e.to_string()))?;
    git::push(&library_root).map_err(|e| AppError::Git(e.to_string()))?;

    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    for apply in &applies {
        match &apply.op {
            ApplyOp::Update => {
                let entry = &mut project_cfg.installed[apply.candidate_index];
                entry.source_sha = new_sha.clone();
                ui::log_success(
                    ctx,
                    format!("{} → {}", entry.name, short_sha(&new_sha)),
                )?;
                results.push(json!({
                    "name": entry.name,
                    "status": "pushed",
                    "operation": "update",
                    "source_sha": new_sha,
                }));
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
                let original_name =
                    project_cfg.installed[apply.candidate_index].name.clone();
                project_cfg.installed[apply.candidate_index] = InstalledSkill {
                    name: new_name.clone(),
                    source_path: new_library_path.clone(),
                    source_sha: new_sha.clone(),
                    destination: new_local_destination.clone(),
                    installed_at: installed_at.clone(),
                };
                ui::log_success(
                    ctx,
                    format!("forked → {} ({})", new_name, short_sha(&new_sha)),
                )?;
                results.push(json!({
                    "name": original_name,
                    "status": "forked",
                    "operation": "fork",
                    "new_name": new_name,
                    "new_source_path": new_library_path.display().to_string(),
                    "source_sha": new_sha,
                }));
            }
        }
    }
    project_config::save(&cwd, &project_cfg)?;

    let pushed = results.iter().filter(|r| r["status"] == "pushed").count();
    let forked = results.iter().filter(|r| r["status"] == "forked").count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let summary = match (pushed, forked, skipped) {
        (u, 0, 0) => format!("pushed {u} skill(s)"),
        (0, f, 0) => format!("forked {f} skill(s)"),
        (u, f, 0) => format!("pushed {u}, forked {f}"),
        (u, 0, s) => format!("pushed {u}, skipped {s}"),
        (0, f, s) => format!("forked {f}, skipped {s}"),
        (u, f, s) => format!("pushed {u}, forked {f}, skipped {s}"),
    };
    ui::outro(ctx, summary)?;
    emit_json(
        ctx,
        &results,
        Some((new_sha.as_str(), message.as_str())),
    );
    Ok(())
}

fn emit_json(ctx: &Context, results: &[Value], commit: Option<(&str, &str)>) {
    if !ctx.json {
        return;
    }
    let pushed = results.iter().filter(|r| r["status"] == "pushed").count();
    let forked = results.iter().filter(|r| r["status"] == "forked").count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let commit_value = commit.map(|(sha, message)| {
        json!({"sha": sha, "message": message})
    });
    let out = json!({
        "command": "push",
        "results": results,
        "commit": commit_value,
        "summary": {
            "pushed": pushed,
            "forked": forked,
            "skipped": skipped,
        },
    });
    println!("{out}");
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
                    AppError::Config(format!(
                        "no pushable skill named `{name}` (skill is unchanged, missing locally, or unknown)"
                    ))
                })?;
            chosen.push(candidate.index);
        }
        return Ok(chosen);
    }
    if !ctx.interactive {
        return Err(AppError::Config(
            "no skills selected — pass --skill <name> (repeatable) or --all".into(),
        )
        .into());
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
        return Err(AppError::Conflict(format!(
            "a folder already exists at {} in the library — pick a different name",
            new_library_path.display()
        ))
        .into());
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

fn describe(status: &SkillStatus) -> String {
    match status {
        SkillStatus::LocalChangesOnly => "local edits, library unchanged".to_string(),
        SkillStatus::BothDiverged {
            local_changed,
            library_changed,
        } => format!("diverged: {local_changed} local, {library_changed} in library"),
        SkillStatus::LibraryAhead { library_changed } => {
            format!("library has {library_changed} update(s); use `skills pull`")
        }
        SkillStatus::Unchanged => "no local changes".to_string(),
        SkillStatus::LocalMissing => "destination missing locally".to_string(),
        SkillStatus::LibraryMissing => "removed from library".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(build_commit_message(&[], &["a", "b"]), "add skills: a, b");
    }

    #[test]
    fn commit_message_mixed_uses_body() {
        let msg = build_commit_message(&["foo"], &["bar"]);
        assert!(msg.starts_with("sync skills\n"));
        assert!(msg.contains("Update: foo"));
        assert!(msg.contains("Add: bar"));
    }
}

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use cliclack::{input, select};

use crate::prompt::multiselect;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::{AddArgs, OnConflict};
use crate::commands::shared::{matches_tags, short_hint};
use crate::config;
use crate::context::Context;
use crate::error::AppError;
use crate::fs_util;
use crate::git;
use crate::project_config::{self, InstalledSkill};
use crate::skill::{self, Skill};
use crate::ui;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DestChoice {
    Existing(PathBuf),
    Preset { label: &'static str, path: PathBuf },
    Custom,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ConflictAction {
    Overwrite,
    Skip,
    Abort,
}

impl From<OnConflict> for ConflictAction {
    fn from(v: OnConflict) -> Self {
        match v {
            OnConflict::Overwrite => Self::Overwrite,
            OnConflict::Skip => Self::Skip,
            OnConflict::Abort => Self::Abort,
        }
    }
}

pub fn run(args: AddArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skills add")?;

    let cfg = config::load()?;
    let library = cfg.library.ok_or_else(|| {
        AppError::Config("no library configured — run `skills init <github-url>` first".into())
    })?;

    let library_root =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
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
            format!("could not refresh library cache ({e}); using cached version"),
        )?;
    }

    let skills = skill::discover(&library_root)?;
    if skills.is_empty() {
        ui::outro(ctx, format!("no skills found in {}", library.url))?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    let selected = select_skills(&args, ctx, &skills)?;
    if selected.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let dest_root = resolve_destination(&args, ctx, &cwd)?;
    let conflict_policy: Option<ConflictAction> = args.on_conflict.map(Into::into);

    let source_sha = git::head_sha(&library_root).map_err(|e| AppError::Git(e.to_string()))?;
    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    let mut project_cfg = project_config::load(&cwd)?;
    let mut results: Vec<Value> = Vec::new();
    let mut aborted = false;

    for skill in selected {
        let folder_name = skill.path.file_name().ok_or_else(|| {
            AppError::Config(format!(
                "skill has no folder name: {}",
                skill.path.display()
            ))
        })?;
        let dest = dest_root.join(folder_name);

        if dest.exists() {
            let action = resolve_conflict(ctx, &dest, conflict_policy.clone())?;
            match action {
                ConflictAction::Overwrite => {
                    fs::remove_dir_all(&dest)
                        .with_context(|| format!("removing {}", dest.display()))?;
                }
                ConflictAction::Skip => {
                    ui::log_info(ctx, format!("skipped {}", skill.name))?;
                    results.push(json!({
                        "name": skill.name,
                        "status": "skipped",
                        "reason": format!("destination {} already exists", dest.display()),
                    }));
                    continue;
                }
                ConflictAction::Abort => {
                    project_config::save(&cwd, &project_cfg)?;
                    ui::outro_cancel(ctx, "aborted")?;
                    results.push(json!({
                        "name": skill.name,
                        "status": "aborted",
                        "reason": format!("destination {} already exists", dest.display()),
                    }));
                    aborted = true;
                    break;
                }
            }
        }

        fs_util::copy_dir_all(&skill.path, &dest)?;
        let source_path = skill
            .path
            .strip_prefix(&library_root)
            .with_context(|| {
                format!(
                    "computing path of {} relative to library at {}",
                    skill.path.display(),
                    library_root.display()
                )
            })?
            .to_path_buf();
        let destination_rel = fs_util::relative_to_or_self(&dest, &cwd);
        project_cfg.installed.push(InstalledSkill {
            name: skill.name.clone(),
            source_path,
            source_sha: source_sha.clone(),
            destination: destination_rel.clone(),
            installed_at: installed_at.clone(),
        });
        ui::log_success(ctx, format!("{} → {}", skill.name, dest.display()))?;
        results.push(json!({
            "name": skill.name,
            "status": "installed",
            "path": destination_rel.display().to_string(),
            "source_sha": source_sha,
        }));
    }

    project_config::save(&cwd, &project_cfg)?;

    if !aborted {
        ui::outro(ctx, summary_text(&results))?;
    }
    emit_json(ctx, Some(&dest_root), &results);
    Ok(())
}

fn emit_json(ctx: &Context, destination: Option<&PathBuf>, results: &[Value]) {
    if !ctx.json {
        return;
    }
    let installed = results
        .iter()
        .filter(|r| r["status"] == "installed")
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let aborted = results.iter().filter(|r| r["status"] == "aborted").count();
    let out = json!({
        "command": "add",
        "destination": destination.map(|d| d.display().to_string()),
        "results": results,
        "summary": {
            "installed": installed,
            "skipped": skipped,
            "aborted": aborted,
        },
    });
    println!("{out}");
}

fn summary_text(results: &[Value]) -> String {
    let installed = results
        .iter()
        .filter(|r| r["status"] == "installed")
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let aborted = results.iter().filter(|r| r["status"] == "aborted").count();
    if aborted > 0 {
        format!("{installed} installed, {skipped} skipped, {aborted} aborted")
    } else if skipped > 0 {
        format!("{installed} installed, {skipped} skipped")
    } else {
        format!("{installed} skill(s) installed")
    }
}

fn select_skills(args: &AddArgs, ctx: &Context, skills: &[Skill]) -> Result<Vec<Skill>> {
    if args.all {
        return Ok(skills.to_vec());
    }
    if !args.skills.is_empty() {
        let mut chosen = Vec::with_capacity(args.skills.len());
        for name in &args.skills {
            let skill = skills.iter().find(|s| s.name == *name).ok_or_else(|| {
                AppError::Config(format!("no skill named `{name}` in the library"))
            })?;
            chosen.push(skill.clone());
        }
        return Ok(chosen);
    }
    if !args.tags.is_empty() {
        let matched: Vec<Skill> = skills
            .iter()
            .filter(|s| matches_tags(&s.tags, &args.tags, args.all_tags))
            .cloned()
            .collect();
        if matched.is_empty() {
            return Err(AppError::Config(format!(
                "no skills match the requested tag(s): {}",
                args.tags.join(", ")
            ))
            .into());
        }
        if !ctx.interactive {
            return Ok(matched);
        }
        let mut prompt = multiselect("Skills to install (tag-filtered)").required(true);
        for s in &matched {
            let hint = s.description.as_deref().map(short_hint).unwrap_or_default();
            prompt = prompt.item(s.clone(), &s.name, hint);
        }
        return prompt.interact();
    }
    if !ctx.interactive {
        return Err(AppError::Config(
            "no skills selected — pass --skill <name> (repeatable), --tag <name>, or --all".into(),
        )
        .into());
    }
    let mut prompt = multiselect("Skills to install").required(true);
    for s in skills {
        let hint = s.description.as_deref().map(short_hint).unwrap_or_default();
        prompt = prompt.item(s.clone(), &s.name, hint);
    }
    prompt.interact()
}

fn resolve_destination(args: &AddArgs, ctx: &Context, cwd: &std::path::Path) -> Result<PathBuf> {
    if let Some(dest) = &args.dest {
        return Ok(dest.clone());
    }
    if !ctx.interactive {
        return Err(AppError::Config("no install destination — pass --dest <path>".into()).into());
    }
    let existing = skill::find_skills_folders(cwd)?
        .into_iter()
        .map(fs_util::strip_dot_prefix)
        .collect();
    pick_destination_interactive(existing)
}

fn resolve_conflict(
    ctx: &Context,
    dest: &std::path::Path,
    policy: Option<ConflictAction>,
) -> Result<ConflictAction> {
    if let Some(policy) = policy {
        return Ok(policy);
    }
    if !ctx.interactive {
        return Err(AppError::Conflict(format!(
            "destination `{}` already exists — pass --on-conflict overwrite|skip|abort",
            dest.display()
        ))
        .into());
    }
    Ok(select(format!(
        "`{}` already exists — what do you want to do?",
        dest.display()
    ))
    .item(
        ConflictAction::Overwrite,
        "Overwrite",
        "replace the existing folder",
    )
    .item(
        ConflictAction::Skip,
        "Skip",
        "leave it and don't record this skill",
    )
    .item(
        ConflictAction::Abort,
        "Abort",
        "stop now and save what's been installed so far",
    )
    .interact()?)
}

fn pick_destination_interactive(existing: Vec<PathBuf>) -> Result<PathBuf> {
    let mut prompt = select("Install destination");

    if existing.is_empty() {
        prompt = prompt
            .item(
                DestChoice::Preset {
                    label: "claude",
                    path: PathBuf::from(".claude/skills"),
                },
                "claude",
                ".claude/skills",
            )
            .item(
                DestChoice::Preset {
                    label: "codex",
                    path: PathBuf::from(".codex/skills"),
                },
                "codex",
                ".codex/skills",
            )
            .item(
                DestChoice::Preset {
                    label: "cursor",
                    path: PathBuf::from(".cursor/skills"),
                },
                "cursor",
                ".cursor/skills",
            )
            .item(
                DestChoice::Preset {
                    label: "agents",
                    path: PathBuf::from(".agents/skills"),
                },
                "agents",
                ".agents/skills",
            );
    } else {
        for p in existing {
            let display = p.display().to_string();
            prompt = prompt.item(DestChoice::Existing(p), display, "");
        }
    }
    prompt = prompt.item(DestChoice::Custom, "Custom path…", "type your own");

    let answer = prompt.interact()?;
    match answer {
        DestChoice::Existing(p) => Ok(p),
        DestChoice::Preset { path, .. } => Ok(path),
        DestChoice::Custom => {
            let typed: String = input("Path")
                .placeholder(".claude/skills")
                .validate(|s: &String| {
                    if s.trim().is_empty() {
                        Err("path cannot be empty")
                    } else {
                        Ok(())
                    }
                })
                .interact()?;
            Ok(PathBuf::from(typed.trim()))
        }
    }
}

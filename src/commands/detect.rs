use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use cliclack::{input, select};

use crate::prompt::multiselect;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::DetectArgs;
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
enum LibDestChoice {
    Existing(PathBuf),
    Custom,
}

pub fn run(args: DetectArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl detect")?;

    let cfg = config::load()?;
    let library = cfg.library.ok_or_else(|| {
        AppError::Config("no library configured — run `skillctl init<github-url>` first".into())
    })?;

    let library_root =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !library_root.exists() {
        return Err(AppError::Config(format!(
            "library cache not found at {} — run `skillctl init{}` again",
            library_root.display(),
            library.url
        ))
        .into());
    }

    if let Err(e) = git::fetch_and_fast_forward(&library_root) {
        ui::log_warning(
            ctx,
            format!("could not refresh library cache ({e}); continuing with the cached version"),
        )?;
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let mut project_cfg = project_config::load(&cwd)?;

    let installed_canonical: HashSet<PathBuf> = project_cfg
        .installed
        .iter()
        .filter_map(|i| std::fs::canonicalize(cwd.join(&i.destination)).ok())
        .collect();

    let local_skills = skill::discover(&cwd)?;
    let new_skills: Vec<Skill> = local_skills
        .into_iter()
        .filter(|s| match std::fs::canonicalize(&s.path) {
            Ok(c) => !installed_canonical.contains(&c),
            Err(_) => true,
        })
        .collect();

    if new_skills.is_empty() {
        ui::outro(
            ctx,
            "no new skills detected (everything is tracked in .skills.toml)",
        )?;
        emit_json(ctx, None, &[], None);
        return Ok(());
    }

    let selected = select_new_skills(&args, ctx, &new_skills)?;
    if selected.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, None, &[], None);
        return Ok(());
    }

    let lib_dest_relative = resolve_target(&args, ctx, &library_root)?;

    let mut applies: Vec<(Skill, PathBuf, PathBuf)> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    for skill in selected {
        let folder_name = skill.path.file_name().ok_or_else(|| {
            AppError::Config(format!(
                "skill folder has no name: {}",
                skill.path.display()
            ))
        })?;
        let lib_relative = if is_root(&lib_dest_relative) {
            PathBuf::from(folder_name)
        } else {
            lib_dest_relative.join(folder_name)
        };
        let lib_absolute = library_root.join(&lib_relative);

        if lib_absolute.exists() {
            ui::log_warning(
                ctx,
                format!(
                    "{} → {} already exists in the library; skipping",
                    skill.name,
                    lib_relative.display()
                ),
            )?;
            results.push(json!({
                "name": skill.name,
                "status": "skipped",
                "reason": format!("{} already exists in library", lib_relative.display()),
            }));
            continue;
        }

        applies.push((skill, lib_relative, lib_absolute));
    }

    if applies.is_empty() {
        ui::outro(ctx, "nothing to add")?;
        emit_json(ctx, Some(&lib_dest_relative), &results, None);
        return Ok(());
    }

    for (skill, _lib_relative, lib_absolute) in &applies {
        fs_util::copy_dir_all(&skill.path, lib_absolute)?;
    }
    for (_, lib_relative, _) in &applies {
        git::add_all(&library_root, lib_relative).map_err(|e| AppError::Git(e.to_string()))?;
    }

    if !git::has_staged_changes(&library_root).map_err(|e| AppError::Git(e.to_string()))? {
        ui::outro(ctx, "no effective changes after adding")?;
        emit_json(ctx, Some(&lib_dest_relative), &results, None);
        return Ok(());
    }

    let names: Vec<&str> = applies.iter().map(|(s, _, _)| s.name.as_str()).collect();
    let message = if names.len() == 1 {
        format!("add skill: {}", names[0])
    } else {
        format!("add skills: {}", names.join(", "))
    };
    let new_sha = git::commit(&library_root, &message).map_err(|e| AppError::Git(e.to_string()))?;
    git::push(&library_root).map_err(|e| AppError::Git(e.to_string()))?;

    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    for (skill, lib_relative, _) in &applies {
        let local_destination = fs_util::relative_to_or_self(&skill.path, &cwd);
        project_cfg.installed.push(InstalledSkill {
            name: skill.name.clone(),
            source_path: lib_relative.clone(),
            source_sha: new_sha.clone(),
            destination: local_destination.clone(),
            installed_at: installed_at.clone(),
        });
        ui::log_success(ctx, format!("{} → {}", skill.name, lib_relative.display()))?;
        results.push(json!({
            "name": skill.name,
            "status": "added",
            "library_path": lib_relative.display().to_string(),
            "local_path": local_destination.display().to_string(),
            "source_sha": new_sha,
        }));
    }
    project_config::save(&cwd, &project_cfg)?;

    ui::outro(
        ctx,
        format!("added {} skill(s) to the library", applies.len()),
    )?;
    emit_json(
        ctx,
        Some(&lib_dest_relative),
        &results,
        Some((new_sha.as_str(), message.as_str())),
    );
    Ok(())
}

fn emit_json(
    ctx: &Context,
    target: Option<&PathBuf>,
    results: &[Value],
    commit: Option<(&str, &str)>,
) {
    if !ctx.json {
        return;
    }
    let added = results.iter().filter(|r| r["status"] == "added").count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let commit_value = commit.map(|(sha, message)| json!({"sha": sha, "message": message}));
    let target_value = target.map(|t| {
        if is_root(t) {
            ".".to_string()
        } else {
            t.display().to_string()
        }
    });
    let out = json!({
        "command": "detect",
        "target": target_value,
        "results": results,
        "commit": commit_value,
        "summary": {
            "added": added,
            "skipped": skipped,
        },
    });
    println!("{out}");
}

/// True if the target path means "library root" — either an empty path or a
/// bare `.`. Used to keep `lib_relative` clean (no leading `./`) and to
/// normalise the JSON output.
fn is_root(p: &Path) -> bool {
    p.as_os_str().is_empty() || p == Path::new(".")
}

fn select_new_skills(args: &DetectArgs, ctx: &Context, new_skills: &[Skill]) -> Result<Vec<Skill>> {
    if args.all {
        return Ok(new_skills.to_vec());
    }
    if !args.skills.is_empty() {
        let mut chosen = Vec::with_capacity(args.skills.len());
        for name in &args.skills {
            let skill = new_skills.iter().find(|s| s.name == *name).ok_or_else(|| {
                AppError::Config(format!(
                    "no detected skill named `{name}` (it may already be in .skills.toml)"
                ))
            })?;
            chosen.push(skill.clone());
        }
        return Ok(chosen);
    }
    if !args.tags.is_empty() {
        let matched: Vec<Skill> = new_skills
            .iter()
            .filter(|s| matches_tags(&s.tags, &args.tags, args.all_tags))
            .cloned()
            .collect();
        if matched.is_empty() {
            return Err(AppError::Config(format!(
                "no detected skill matches the requested tag(s): {}",
                args.tags.join(", ")
            ))
            .into());
        }
        if !ctx.interactive {
            return Ok(matched);
        }
        let mut prompt =
            multiselect("New skills to add to the library (tag-filtered)").required(true);
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
    let mut prompt = multiselect("New skills to add to the library").required(true);
    for s in new_skills {
        let hint = s.description.as_deref().map(short_hint).unwrap_or_default();
        prompt = prompt.item(s.clone(), &s.name, hint);
    }
    prompt.interact()
}

fn resolve_target(args: &DetectArgs, ctx: &Context, library_root: &Path) -> Result<PathBuf> {
    if let Some(target) = &args.target {
        if target.is_absolute() {
            return Err(AppError::Config(format!(
                "--target must be relative to the library root, got `{}`",
                target.display()
            ))
            .into());
        }
        return Ok(target.clone());
    }
    if !ctx.interactive {
        return Err(AppError::Config(
            "no library destination — pass --target <path> (relative to the library root)".into(),
        )
        .into());
    }
    pick_library_destination(library_root)
}

fn pick_library_destination(library_root: &Path) -> Result<PathBuf> {
    let folders = skill::find_skills_folders(library_root)?;

    let mut prompt = select("Library destination");

    // Library root first — it's the default for flat-layout libraries where
    // each skill is a top-level folder.
    prompt = prompt.item(
        LibDestChoice::Existing(PathBuf::from(".")),
        "Library root",
        "add the skill folder directly under the library's top level",
    );

    if folders.is_empty() {
        // Convenient preset for libraries that haven't created the conventional
        // `skills/` subfolder yet.
        prompt = prompt.item(
            LibDestChoice::Existing(PathBuf::from("skills")),
            "skills",
            "create a top-level `skills/` folder in the library",
        );
    } else {
        for folder in folders {
            let rel = folder
                .strip_prefix(library_root)
                .map(Path::to_path_buf)
                .unwrap_or(folder);
            let display = rel.display().to_string();
            prompt = prompt.item(LibDestChoice::Existing(rel), display, "");
        }
    }
    prompt = prompt.item(
        LibDestChoice::Custom,
        "Custom path…",
        "type a path relative to the library root",
    );

    let answer = prompt.interact()?;
    match answer {
        LibDestChoice::Existing(p) => Ok(p),
        LibDestChoice::Custom => {
            let typed: String = input("Path inside library")
                .placeholder("skills")
                .validate(|s: &String| {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        return Err("path cannot be empty");
                    }
                    if Path::new(trimmed).is_absolute() {
                        return Err("use a path relative to the library root");
                    }
                    Ok(())
                })
                .interact()?;
            Ok(PathBuf::from(typed.trim()))
        }
    }
}

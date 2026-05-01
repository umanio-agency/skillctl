use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use cliclack::{input, intro, log, multiselect, outro, outro_cancel, select};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::AddArgs;
use crate::commands::shared::short_hint;
use crate::config;
use crate::fs_util;
use crate::git;
use crate::project_config::{self, InstalledSkill};
use crate::skill::{self, Skill};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DestChoice {
    Existing(PathBuf),
    Preset {
        label: &'static str,
        path: PathBuf,
    },
    Custom,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ConflictAction {
    Overwrite,
    Skip,
    Abort,
}

pub fn run(_args: AddArgs) -> Result<()> {
    intro("skills add")?;

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
            "could not refresh library cache ({e}); using cached version"
        ))?;
    }

    let skills = skill::discover(&library_root)?;
    if skills.is_empty() {
        outro(format!("no skills found in {}", library.url))?;
        return Ok(());
    }

    let mut prompt = multiselect("Skills to install").required(true);
    for s in &skills {
        let hint = s.description.as_deref().map(short_hint).unwrap_or_default();
        prompt = prompt.item(s.clone(), &s.name, hint);
    }
    let selected: Vec<Skill> = prompt.interact()?;

    let cwd = std::env::current_dir().context("reading current directory")?;
    let existing = skill::find_skills_folders(&cwd)?
        .into_iter()
        .map(fs_util::strip_dot_prefix)
        .collect();
    let dest_root = pick_destination(existing)?;

    let source_sha = git::head_sha(&library_root)?;
    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    let mut project_cfg = project_config::load(&cwd)?;
    let mut installed_count = 0usize;
    let mut skipped_count = 0usize;

    for skill in selected {
        let folder_name = skill
            .path
            .file_name()
            .ok_or_else(|| anyhow!("skill has no folder name: {}", skill.path.display()))?;
        let dest = dest_root.join(folder_name);

        if dest.exists() {
            let action = select(format!(
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
            .interact()?;
            match action {
                ConflictAction::Overwrite => {
                    fs::remove_dir_all(&dest)
                        .with_context(|| format!("removing {}", dest.display()))?;
                }
                ConflictAction::Skip => {
                    log::info(format!("skipped {}", skill.name))?;
                    skipped_count += 1;
                    continue;
                }
                ConflictAction::Abort => {
                    project_config::save(&cwd, &project_cfg)?;
                    outro_cancel("aborted")?;
                    return Ok(());
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
        project_cfg.installed.push(InstalledSkill {
            name: skill.name.clone(),
            source_path,
            source_sha: source_sha.clone(),
            destination: fs_util::relative_to_or_self(&dest, &cwd),
            installed_at: installed_at.clone(),
        });
        log::success(format!("{} → {}", skill.name, dest.display()))?;
        installed_count += 1;
    }

    project_config::save(&cwd, &project_cfg)?;

    let summary = match (installed_count, skipped_count) {
        (n, 0) => format!("{n} skill(s) installed"),
        (n, s) => format!("{n} installed, {s} skipped"),
    };
    outro(summary)?;
    Ok(())
}

fn pick_destination(existing: Vec<PathBuf>) -> Result<PathBuf> {
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

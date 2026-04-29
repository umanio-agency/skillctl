use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use ignore::WalkBuilder;
use inquire::{MultiSelect, Select, Text};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::AddArgs;
use crate::config;
use crate::git;
use crate::project_config::{self, InstalledSkill};
use crate::skill::{self, Skill};

pub fn run(_args: AddArgs) -> Result<()> {
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
        eprintln!("warning: could not refresh library cache ({e}); using cached version");
    }
    let skills = skill::discover(&library_root)?;
    if skills.is_empty() {
        println!("no skills found in {}", library.url);
        return Ok(());
    }

    let choices: Vec<SkillChoice> = skills.into_iter().map(SkillChoice::from).collect();
    let selected = MultiSelect::new(
        "Skills to install (space to toggle, enter to confirm):",
        choices,
    )
    .prompt()?;
    if selected.is_empty() {
        println!("no skills selected; nothing to do.");
        return Ok(());
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let existing = find_existing_skills_folders(&cwd)?;
    let dest_root = pick_destination(existing)?;

    let source_sha = git::head_sha(&library_root)?;
    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    let mut project_cfg = project_config::load(&cwd)?;

    for choice in selected {
        let skill = choice.skill;
        let folder_name = skill
            .path
            .file_name()
            .ok_or_else(|| anyhow!("skill has no folder name: {}", skill.path.display()))?;
        let dest = dest_root.join(folder_name);

        if dest.exists() {
            let action = Select::new(
                &format!(
                    "`{}` already exists. What do you want to do?",
                    dest.display()
                ),
                vec!["Overwrite", "Skip", "Abort"],
            )
            .prompt()?;
            match action {
                "Overwrite" => {
                    fs::remove_dir_all(&dest)
                        .with_context(|| format!("removing {}", dest.display()))?;
                }
                "Skip" => {
                    println!("skipped: {}", skill.name);
                    continue;
                }
                "Abort" => {
                    project_config::save(&cwd, &project_cfg)?;
                    println!("aborted.");
                    return Ok(());
                }
                _ => unreachable!(),
            }
        }

        copy_dir_all(&skill.path, &dest)?;
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
            destination: relative_to_or_self(&dest, &cwd),
            installed_at: installed_at.clone(),
        });
        println!("installed: {} → {}", skill.name, dest.display());
    }

    project_config::save(&cwd, &project_cfg)?;
    Ok(())
}

struct SkillChoice {
    skill: Skill,
}

impl From<Skill> for SkillChoice {
    fn from(skill: Skill) -> Self {
        Self { skill }
    }
}

impl fmt::Display for SkillChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.skill.description {
            Some(desc) => write!(f, "{} — {desc}", self.skill.name),
            None => write!(f, "{}", self.skill.name),
        }
    }
}

enum DestChoice {
    Existing(PathBuf),
    Preset {
        label: &'static str,
        path: PathBuf,
    },
    Custom,
}

impl fmt::Display for DestChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DestChoice::Existing(p) => write!(f, "{}", p.display()),
            DestChoice::Preset { label, path } => write!(f, "{label} → {}", path.display()),
            DestChoice::Custom => f.write_str("Custom path…"),
        }
    }
}

fn pick_destination(existing: Vec<PathBuf>) -> Result<PathBuf> {
    let choices: Vec<DestChoice> = if existing.is_empty() {
        vec![
            DestChoice::Preset {
                label: "claude",
                path: PathBuf::from(".claude/skills"),
            },
            DestChoice::Preset {
                label: "codex",
                path: PathBuf::from(".codex/skills"),
            },
            DestChoice::Preset {
                label: "cursor",
                path: PathBuf::from(".cursor/skills"),
            },
            DestChoice::Preset {
                label: "agents",
                path: PathBuf::from(".agents/skills"),
            },
            DestChoice::Custom,
        ]
    } else {
        let mut c: Vec<DestChoice> = existing.into_iter().map(DestChoice::Existing).collect();
        c.push(DestChoice::Custom);
        c
    };

    let answer = Select::new("Install destination:", choices).prompt()?;
    match answer {
        DestChoice::Existing(p) => Ok(p),
        DestChoice::Preset { path, .. } => Ok(path),
        DestChoice::Custom => {
            let typed = Text::new("Path:").prompt()?;
            let trimmed = typed.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("destination path cannot be empty"));
            }
            Ok(PathBuf::from(trimmed))
        }
    }
}

fn find_existing_skills_folders(root: &Path) -> Result<Vec<PathBuf>> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| {
            let name = e.file_name();
            name != "node_modules" && name != "target"
        })
        .build();
    let mut found = Vec::new();
    for entry in walker {
        let entry = entry.context("walking the project tree")?;
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        if is_dir && entry.file_name() == "skills" {
            found.push(strip_dot_prefix(entry.path().to_path_buf()));
        }
    }
    found.sort();
    Ok(found)
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("copying {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn relative_to_or_self(path: &Path, base: &Path) -> PathBuf {
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| strip_dot_prefix(path.to_path_buf()))
}

fn strip_dot_prefix(p: PathBuf) -> PathBuf {
    p.strip_prefix(".").map(Path::to_path_buf).unwrap_or(p)
}

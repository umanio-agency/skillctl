use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use cliclack::{input, intro, log, multiselect, outro, select};

use crate::cli::{OnDivergence, PullArgs};
use crate::commands::diff::{SkillStatus, classify};
use crate::config;
use crate::context::Context;
use crate::fs_util;
use crate::git;
use crate::project_config::{self, InstalledSkill};

#[derive(Clone, Debug)]
struct Candidate {
    index: usize,
    name: String,
    destination: PathBuf,
    status: SkillStatus,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DivergenceChoice {
    /// Replace local content with the library version, discarding local edits.
    Overwrite,
    /// Rename the local copy under a new name, then pull the library version
    /// into the original destination.
    ForkLocal,
    /// Leave both sides untouched.
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

struct Apply {
    candidate_index: usize,
    op: ApplyOp,
}

enum ApplyOp {
    Pull,
    ForkLocal { local_fork_name: String },
}

pub fn run(args: PullArgs, ctx: &Context) -> Result<()> {
    intro("skills pull")?;

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
            SkillStatus::Unchanged => log::info(format!("{} — up to date", c.name))?,
            SkillStatus::LocalChangesOnly => log::info(format!(
                "{} — local edits without library updates (use `skills push`)",
                c.name
            ))?,
            SkillStatus::LibraryMissing => log::warning(format!(
                "{} — removed from library; consider editing .skills.toml",
                c.name
            ))?,
            SkillStatus::LocalMissing => log::warning(format!(
                "{} — destination {} no longer exists; can't pull",
                c.name,
                c.destination.display()
            ))?,
            _ => {}
        }
    }

    let pullable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            matches!(
                c.status,
                SkillStatus::LibraryAhead { .. } | SkillStatus::BothDiverged { .. }
            )
        })
        .collect();

    if pullable.is_empty() {
        outro("everything is up to date")?;
        return Ok(());
    }

    let selected_indices = select_pullable(&args, ctx, &pullable)?;
    if selected_indices.is_empty() {
        outro("no skills selected")?;
        return Ok(());
    }

    let mut applies: Vec<Apply> = Vec::new();
    for idx in &selected_indices {
        let candidate = pullable
            .iter()
            .find(|c| c.index == *idx)
            .copied()
            .ok_or_else(|| anyhow!("selected index {idx} not in pullable set"))?;
        let installed = &project_cfg.installed[candidate.index];

        let op = match &candidate.status {
            SkillStatus::LibraryAhead { .. } => Some(ApplyOp::Pull),
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
                        "Pull library, discard local edits",
                        "replace local content with the library version",
                    )
                    .item(
                        DivergenceChoice::ForkLocal,
                        "Fork locally",
                        "rename your local copy under a new name, then pull the library version into the original location",
                    )
                    .item(
                        DivergenceChoice::Skip,
                        "Skip",
                        "leave this skill untouched",
                    )
                    .interact()?
                };
                match choice {
                    DivergenceChoice::Overwrite => Some(ApplyOp::Pull),
                    DivergenceChoice::ForkLocal => Some(prompt_local_fork_op(installed, &cwd)?),
                    DivergenceChoice::Skip => {
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
        outro("nothing to pull after conflict resolution")?;
        return Ok(());
    }

    let new_sha = git::head_sha(&library_root)?;
    let mut pulled_count = 0usize;
    let mut forked_count = 0usize;

    for apply in &applies {
        let installed_name = project_cfg.installed[apply.candidate_index].name.clone();
        let installed_destination =
            project_cfg.installed[apply.candidate_index].destination.clone();
        let installed_source_path =
            project_cfg.installed[apply.candidate_index].source_path.clone();
        let local_dir = cwd.join(&installed_destination);
        let library_dir = library_root.join(&installed_source_path);

        match &apply.op {
            ApplyOp::Pull => {
                fs_util::replace_folder_contents(&library_dir, &local_dir)?;
                project_cfg.installed[apply.candidate_index].source_sha = new_sha.clone();
                log::success(format!("{} → {}", installed_name, short_sha(&new_sha)))?;
                pulled_count += 1;
            }
            ApplyOp::ForkLocal { local_fork_name } => {
                let fork_dest = local_fork_destination(
                    &project_cfg.installed[apply.candidate_index],
                    &cwd,
                    local_fork_name,
                );
                if fork_dest.exists() {
                    return Err(anyhow!(
                        "cannot fork-locally: target {} already exists",
                        fork_dest.display()
                    ));
                }
                fs::rename(&local_dir, &fork_dest).with_context(|| {
                    format!("renaming {} -> {}", local_dir.display(), fork_dest.display())
                })?;
                fs_util::copy_dir_all(&library_dir, &local_dir)?;
                project_cfg.installed[apply.candidate_index].source_sha = new_sha.clone();
                log::success(format!(
                    "{} → {} (local fork preserved at {})",
                    installed_name,
                    short_sha(&new_sha),
                    fs_util::relative_to_or_self(&fork_dest, &cwd).display()
                ))?;
                pulled_count += 1;
                forked_count += 1;
            }
        }
    }

    project_config::save(&cwd, &project_cfg)?;

    let skipped = selected_indices.len() - pulled_count;
    let summary = if forked_count > 0 {
        if skipped > 0 {
            format!("pulled {pulled_count} ({forked_count} with local fork), skipped {skipped}")
        } else {
            format!("pulled {pulled_count} ({forked_count} with local fork)")
        }
    } else if skipped > 0 {
        format!("pulled {pulled_count}, skipped {skipped}")
    } else {
        format!("pulled {pulled_count} skill(s)")
    };
    outro(summary)?;
    Ok(())
}

fn select_pullable(args: &PullArgs, ctx: &Context, pullable: &[&Candidate]) -> Result<Vec<usize>> {
    if args.all {
        return Ok(pullable.iter().map(|c| c.index).collect());
    }
    if !args.skills.is_empty() {
        let mut chosen = Vec::with_capacity(args.skills.len());
        for name in &args.skills {
            let candidate = pullable
                .iter()
                .find(|c| c.name == *name)
                .ok_or_else(|| {
                    anyhow!("no pullable skill named `{name}` (skill is up to date or unknown)")
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
    let mut prompt = multiselect("Skills to pull").required(true);
    for c in pullable {
        let hint = describe(&c.status);
        prompt = prompt.item(c.index, &c.name, hint);
    }
    Ok(prompt.interact()?)
}

fn prompt_local_fork_op(installed: &InstalledSkill, cwd: &Path) -> Result<ApplyOp> {
    let placeholder = format!("{}-local", installed.name);
    let raw_name: String = input("Local fork name")
        .placeholder(&placeholder)
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

    let fork_dest = local_fork_destination(installed, cwd, &new_name);
    if fork_dest.exists() {
        return Err(anyhow!(
            "a folder already exists at {} — pick a different name",
            fork_dest.display()
        ));
    }

    Ok(ApplyOp::ForkLocal {
        local_fork_name: new_name,
    })
}

fn local_fork_destination(installed: &InstalledSkill, cwd: &Path, name: &str) -> PathBuf {
    let local_parent = installed
        .destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    if local_parent.as_os_str().is_empty() {
        cwd.join(name)
    } else {
        cwd.join(local_parent).join(name)
    }
}

fn short_sha(sha: &str) -> &str {
    &sha[..7.min(sha.len())]
}

fn describe(status: &SkillStatus) -> String {
    match status {
        SkillStatus::LibraryAhead { library_changed } => {
            format!("library has {library_changed} update(s)")
        }
        SkillStatus::BothDiverged {
            local_changed,
            library_changed,
        } => format!("diverged: {local_changed} local, {library_changed} in library"),
        SkillStatus::Unchanged => "up to date".to_string(),
        SkillStatus::LocalChangesOnly => "local edits, library unchanged".to_string(),
        SkillStatus::LocalMissing => "destination missing locally".to_string(),
        SkillStatus::LibraryMissing => "removed from library".to_string(),
    }
}

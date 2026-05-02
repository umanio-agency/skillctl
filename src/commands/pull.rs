use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use cliclack::{input, multiselect, select};
use serde_json::{Value, json};

use crate::cli::{OnDivergence, PullArgs};
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
    ui::intro(ctx, "skills pull")?;

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
        emit_json(ctx, &[]);
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
                ui::log_info(ctx, format!("{} — up to date", c.name))?
            }
            SkillStatus::LocalChangesOnly => ui::log_info(
                ctx,
                format!(
                    "{} — local edits without library updates (use `skills push`)",
                    c.name
                ),
            )?,
            SkillStatus::LibraryMissing => ui::log_warning(
                ctx,
                format!(
                    "{} — removed from library; consider editing .skills.toml",
                    c.name
                ),
            )?,
            SkillStatus::LocalMissing => ui::log_warning(
                ctx,
                format!(
                    "{} — destination {} no longer exists; can't pull",
                    c.name,
                    c.destination.display()
                ),
            )?,
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
        ui::outro(ctx, "everything is up to date")?;
        emit_json(ctx, &[]);
        return Ok(());
    }

    let selected_indices = select_pullable(&args, ctx, &pullable)?;
    if selected_indices.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, &[]);
        return Ok(());
    }

    let mut results: Vec<Value> = Vec::new();
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
        ui::outro(ctx, "nothing to pull after conflict resolution")?;
        emit_json(ctx, &results);
        return Ok(());
    }

    let new_sha =
        git::head_sha(&library_root).map_err(|e| AppError::Git(e.to_string()))?;

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
                ui::log_success(
                    ctx,
                    format!("{} → {}", installed_name, short_sha(&new_sha)),
                )?;
                results.push(json!({
                    "name": installed_name,
                    "status": "pulled",
                    "source_sha": new_sha,
                }));
            }
            ApplyOp::ForkLocal { local_fork_name } => {
                let fork_dest = local_fork_destination(
                    &project_cfg.installed[apply.candidate_index],
                    &cwd,
                    local_fork_name,
                );
                if fork_dest.exists() {
                    return Err(AppError::Conflict(format!(
                        "cannot fork-locally: target {} already exists",
                        fork_dest.display()
                    ))
                    .into());
                }
                fs::rename(&local_dir, &fork_dest).with_context(|| {
                    format!("renaming {} -> {}", local_dir.display(), fork_dest.display())
                })?;
                fs_util::copy_dir_all(&library_dir, &local_dir)?;
                project_cfg.installed[apply.candidate_index].source_sha = new_sha.clone();
                let fork_rel = fs_util::relative_to_or_self(&fork_dest, &cwd);
                ui::log_success(
                    ctx,
                    format!(
                        "{} → {} (local fork preserved at {})",
                        installed_name,
                        short_sha(&new_sha),
                        fork_rel.display()
                    ),
                )?;
                results.push(json!({
                    "name": installed_name,
                    "status": "pulled",
                    "fork_local": local_fork_name,
                    "fork_local_path": fork_rel.display().to_string(),
                    "source_sha": new_sha,
                }));
            }
        }
    }

    project_config::save(&cwd, &project_cfg)?;

    let pulled = results.iter().filter(|r| r["status"] == "pulled").count();
    let forked = results
        .iter()
        .filter(|r| r["status"] == "pulled" && !r["fork_local"].is_null())
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let summary = if forked > 0 {
        if skipped > 0 {
            format!("pulled {pulled} ({forked} with local fork), skipped {skipped}")
        } else {
            format!("pulled {pulled} ({forked} with local fork)")
        }
    } else if skipped > 0 {
        format!("pulled {pulled}, skipped {skipped}")
    } else {
        format!("pulled {pulled} skill(s)")
    };
    ui::outro(ctx, summary)?;
    emit_json(ctx, &results);
    Ok(())
}

fn emit_json(ctx: &Context, results: &[Value]) {
    if !ctx.json {
        return;
    }
    let pulled = results.iter().filter(|r| r["status"] == "pulled").count();
    let forked_locally = results
        .iter()
        .filter(|r| r["status"] == "pulled" && !r["fork_local"].is_null())
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let out = json!({
        "command": "pull",
        "results": results,
        "summary": {
            "pulled": pulled,
            "forked_locally": forked_locally,
            "skipped": skipped,
        },
    });
    println!("{out}");
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
                    AppError::Config(format!(
                        "no pullable skill named `{name}` (skill is up to date or unknown)"
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
        return Err(AppError::Conflict(format!(
            "a folder already exists at {} — pick a different name",
            fork_dest.display()
        ))
        .into());
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

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use cliclack::{input, select};

use crate::prompt::multiselect;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::{OnDivergence, PushArgs};
use crate::commands::diff::{SkillStatus, classify};
use crate::commands::shared::matches_tags;
use crate::config;
use crate::context::Context;
use crate::error::AppError;
use crate::fs_util;
use crate::git;
use crate::lock;
use crate::path_safety::safe_join;
use crate::project_config::{self, InstalledSkill};
use crate::sanitize::{validate_fork_name, validate_message_safe};
use crate::skill;
use crate::ui;

#[derive(Clone, Debug)]
struct Candidate {
    index: usize,
    name: String,
    destination: PathBuf,
    status: SkillStatus,
    tags: Vec<String>,
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
            OnDivergence::Fork => Self::Fork,
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
    ui::intro(ctx, "skillctl push")?;

    if matches!(args.on_divergence, Some(OnDivergence::Fork))
        && !ctx.interactive
        && args.fork_suffix.is_none()
    {
        return Err(AppError::Config(
            "--on-divergence fork requires --fork-suffix in non-interactive mode".into(),
        )
        .into());
    }

    // Reject CRLF / ESC / NUL etc. in user-supplied commit message. The body
    // can be multi-line (LF is fine) but a `\r\n` would let an agent forge
    // commit trailers (`Co-Authored-By:`, etc.) that downstream bots trust.
    if let Some(msg) = &args.message {
        validate_message_safe("--message", msg)?;
    }

    let cfg = config::load()?;
    let library = cfg.library.ok_or_else(|| {
        AppError::Config("no library configured — run `skillctl init<github-url>` first".into())
    })?;

    let library_root =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !library_root.exists() {
        return Err(AppError::Config(format!(
            "library cache not found at {} — run `skillctl init{}` again",
            fs_util::display_path(&library_root),
            library.url
        ))
        .into());
    }
    // Serialise all library-cache mutations (fetch + reset + add + commit +
    // push) across concurrent skillctl processes. Released on function
    // return.
    let _cache_lock = lock::acquire_exclusive(&library_root, "library cache")?;

    if let Err(e) = git::fetch_and_fast_forward(&library_root) {
        ui::log_warning(
            ctx,
            format!(
                "could not refresh library cache ({e}); diff is computed against the cached HEAD"
            ),
        )?;
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;
    let mut project_cfg = project_config::load(&cwd)?;
    if project_cfg.installed.is_empty() {
        ui::outro(
            ctx,
            "no skills installed in this project (.skills.toml is empty)",
        )?;
        emit_json(ctx, &[], None);
        return Ok(());
    }

    let mut candidates = Vec::new();
    for (index, installed) in project_cfg.installed.iter().enumerate() {
        let status = classify(installed, &cwd, &library_root)?;
        let tags = skill::read_tags(&cwd.join(&installed.destination).join("SKILL.md"))
            .unwrap_or_default();
        candidates.push(Candidate {
            index,
            name: installed.name.clone(),
            destination: installed.destination.clone(),
            status,
            tags,
        });
    }

    for c in &candidates {
        match &c.status {
            SkillStatus::Unchanged => ui::log_info(ctx, format!("{} — no local changes", c.name))?,
            SkillStatus::LibraryAhead { .. } => ui::log_info(
                ctx,
                format!("{} — library has updates (run `skillctl pull`)", c.name),
            )?,
            SkillStatus::LocalMissing => ui::log_warning(
                ctx,
                format!(
                    "{} — destination {} no longer exists; skipping",
                    c.name,
                    c.destination.display()
                ),
            )?,
            SkillStatus::SourceShaOrphaned => ui::log_warning(
                ctx,
                format!(
                    "{} — source_sha in .skills.toml doesn't resolve in the library (force-pushed or GC'd); skipping. Run `skillctl pull` then re-install to repair.",
                    c.name
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
                    DivergenceChoice::Fork => Some(resolve_fork_op(
                        ctx,
                        installed,
                        &library_root,
                        args.fork_suffix.as_deref(),
                    )?),
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
                        OnDivergence::Fork => LibMissingChoice::Fork,
                        OnDivergence::Overwrite => {
                            ui::log_warning(
                                ctx,
                                format!(
                                    "{} is removed from the library; --on-divergence overwrite cannot apply (only fork or skip)",
                                    candidate.name
                                ),
                            )?;
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
                    .item(LibMissingChoice::Skip, "Skip", "leave this skill untracked")
                    .interact()?
                };
                match choice {
                    LibMissingChoice::Fork => Some(resolve_fork_op(
                        ctx,
                        installed,
                        &library_root,
                        args.fork_suffix.as_deref(),
                    )?),
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

    // Per-skill apply with continue-on-error. If one skill's
    // `replace_folder_contents` or `git add` fails, we (a) log a warning,
    // (b) restore the library cache's working tree for that path via
    // `git checkout HEAD -- <library_relative>` so the cache stays in sync
    // with HEAD, (c) drop the failed apply from the list, and (d) continue
    // with the remaining skills. This keeps a single bad skill (e.g. one
    // whose local copy contains a fresh hardlink) from aborting an entire
    // multi-skill push and orphaning the cache mid-batch.
    let mut applies_kept: Vec<Apply> = Vec::with_capacity(applies.len());
    for apply in applies {
        let installed = &project_cfg.installed[apply.candidate_index];
        let installed_name = installed.name.clone();
        let local_dir = safe_join(&cwd, &installed.destination)?;
        let (library_dir, library_relative) = match &apply.op {
            ApplyOp::Update => (
                safe_join(&library_root, &installed.source_path)?,
                installed.source_path.clone(),
            ),
            ApplyOp::Fork {
                new_library_path, ..
            } => (
                safe_join(&library_root, new_library_path)?,
                new_library_path.clone(),
            ),
        };
        let outcome: Result<()> = (|| {
            fs_util::replace_folder_contents(&local_dir, &library_dir)?;
            git::add_all(&library_root, &library_relative)
                .map_err(|e| AppError::Git(e.to_string()))?;
            Ok(())
        })();
        match outcome {
            Ok(()) => applies_kept.push(apply),
            Err(e) => {
                let _ = ui::log_warning(
                    ctx,
                    format!("push failed for `{installed_name}`: {e}; rolling back partial work"),
                );
                if let Err(cleanup_err) = git::checkout_paths(&library_root, &library_relative) {
                    let _ = ui::log_warning(
                        ctx,
                        format!(
                            "could not roll back `{}` in library cache: {cleanup_err}",
                            library_relative.display()
                        ),
                    );
                }
                results.push(json!({
                    "name": installed_name,
                    "status": "failed",
                    "reason": e.to_string(),
                }));
            }
        }
    }
    let applies = applies_kept;

    if applies.is_empty() {
        ui::outro(ctx, "nothing pushed (all selected skills failed to apply)")?;
        emit_json(ctx, &results, None);
        return Ok(());
    }

    if !git::has_staged_changes(&library_root).map_err(|e| AppError::Git(e.to_string()))? {
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

    let new_sha = git::commit(&library_root, &message).map_err(|e| AppError::Git(e.to_string()))?;
    // If `push` fails the just-created commit sits orphaned in the cache,
    // ahead of upstream. Roll it back explicitly so the cache returns to
    // a clean `@{upstream}`-matching state — much friendlier for the
    // operator than the M10 porcelain check refusing to refresh next run.
    if let Err(e) = git::push(&library_root) {
        if let Err(rollback_err) = git::reset_hard_to_parent(&library_root) {
            let _ = ui::log_warning(
                ctx,
                format!("could not roll back the local commit after push failure: {rollback_err}"),
            );
        }
        return Err(AppError::Git(e.to_string()).into());
    }

    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;

    // Phase 1: in-memory mutations only. Update entries' source_sha (or
    // fully replace for forks). Capture the data we need for post-save
    // logging + the local rename targets.
    enum PostSaveTask {
        Update {
            name: String,
        },
        Fork {
            orig_name: String,
            new_name: String,
            new_library_path: PathBuf,
            abs_old: PathBuf,
            abs_new: PathBuf,
        },
    }
    let mut tasks: Vec<PostSaveTask> = Vec::with_capacity(applies.len());
    for apply in &applies {
        match &apply.op {
            ApplyOp::Update => {
                project_cfg.installed[apply.candidate_index].source_sha = new_sha.clone();
                tasks.push(PostSaveTask::Update {
                    name: project_cfg.installed[apply.candidate_index].name.clone(),
                });
            }
            ApplyOp::Fork {
                new_name,
                new_library_path,
                new_local_destination,
            } => {
                let orig_name = project_cfg.installed[apply.candidate_index].name.clone();
                let abs_old = safe_join(
                    &cwd,
                    &project_cfg.installed[apply.candidate_index].destination,
                )?;
                let abs_new = safe_join(&cwd, new_local_destination)?;
                project_cfg.installed[apply.candidate_index] = InstalledSkill {
                    name: new_name.clone(),
                    source_path: new_library_path.clone(),
                    source_sha: new_sha.clone(),
                    destination: new_local_destination.clone(),
                    installed_at: installed_at.clone(),
                };
                tasks.push(PostSaveTask::Fork {
                    orig_name,
                    new_name: new_name.clone(),
                    new_library_path: new_library_path.clone(),
                    abs_old,
                    abs_new,
                });
            }
        }
    }

    // Phase 2: atomically persist .skills.toml. After this returns, the
    // operator's tracked state matches the library — even if a subsequent
    // local rename fails (Phase 3), the SHA mapping is durable and the
    // operator can repair the local folder by hand. Failure here is fatal:
    // we've already pushed upstream but the local index doesn't know yet,
    // so the next run will reclassify and offer to pull — recoverable but
    // surprising. The atomic-rename in `project_config::save` keeps the
    // failure window to "disk full" / "EACCES" rather than partial writes.
    project_config::save(&cwd, &project_cfg)?;

    // Phase 3: local renames for forks + logging. Rename errors here are
    // surfaced as warnings but do NOT propagate — the library and
    // .skills.toml are already in sync; only the local folder name lags,
    // which the operator can fix manually.
    for task in &tasks {
        match task {
            PostSaveTask::Update { name } => {
                ui::log_success(ctx, format!("{} → {}", name, short_sha(&new_sha)))?;
                results.push(json!({
                    "name": name,
                    "status": "pushed",
                    "operation": "update",
                    "source_sha": new_sha,
                }));
            }
            PostSaveTask::Fork {
                orig_name,
                new_name,
                new_library_path,
                abs_old,
                abs_new,
            } => {
                if abs_old != abs_new {
                    let rename_err: Option<String> = (|| {
                        if let Some(parent) = abs_new.parent() {
                            if let Err(e) = fs::create_dir_all(parent) {
                                return Some(format!(
                                    "creating parent of {}: {e}",
                                    abs_new.display()
                                ));
                            }
                        }
                        if let Err(e) = fs::rename(abs_old, abs_new) {
                            return Some(format!(
                                "renaming {} -> {}: {e}",
                                abs_old.display(),
                                abs_new.display()
                            ));
                        }
                        None
                    })();
                    if let Some(reason) = rename_err {
                        ui::log_warning(
                            ctx,
                            format!(
                                "library updated but local rename failed for `{orig_name}` → `{new_name}`: {reason}. `.skills.toml` records the new destination; rename the local folder by hand to clear the divergence."
                            ),
                        )?;
                    }
                }
                ui::log_success(
                    ctx,
                    format!("forked → {} ({})", new_name, short_sha(&new_sha)),
                )?;
                results.push(json!({
                    "name": orig_name,
                    "status": "forked",
                    "operation": "fork",
                    "new_name": new_name,
                    "new_source_path": new_library_path.display().to_string(),
                    "source_sha": new_sha,
                }));
            }
        }
    }

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
    emit_json(ctx, &results, Some((new_sha.as_str(), message.as_str())));
    Ok(())
}

fn emit_json(ctx: &Context, results: &[Value], commit: Option<(&str, &str)>) {
    if !ctx.json {
        return;
    }
    let pushed = results.iter().filter(|r| r["status"] == "pushed").count();
    let forked = results.iter().filter(|r| r["status"] == "forked").count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let commit_value = commit.map(|(sha, message)| json!({"sha": sha, "message": message}));
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

fn select_pushable(args: &PushArgs, ctx: &Context, pushable: &[&Candidate]) -> Result<Vec<usize>> {
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
    if !args.tags.is_empty() {
        let matched: Vec<&&Candidate> = pushable
            .iter()
            .filter(|c| matches_tags(&c.tags, &args.tags, args.all_tags))
            .collect();
        if matched.is_empty() {
            return Err(AppError::Config(format!(
                "no pushable skill matches the requested tag(s): {}",
                args.tags.join(", ")
            ))
            .into());
        }
        if !ctx.interactive {
            return Ok(matched.iter().map(|c| c.index).collect());
        }
        let mut prompt = multiselect("Skills to push (tag-filtered)").required(true);
        for c in &matched {
            let hint = describe(&c.status);
            prompt = prompt.item(c.index, &c.name, hint);
        }
        return prompt.interact();
    }
    if !ctx.interactive {
        return Err(AppError::Config(
            "no skills selected — pass --skill <name> (repeatable), --tag <name>, or --all".into(),
        )
        .into());
    }
    let mut prompt = multiselect("Skills to push").required(true);
    for c in pushable {
        let hint = describe(&c.status);
        prompt = prompt.item(c.index, &c.name, hint);
    }
    prompt.interact()
}

fn resolve_fork_op(
    ctx: &Context,
    installed: &InstalledSkill,
    library_root: &Path,
    fork_suffix: Option<&str>,
) -> Result<ApplyOp> {
    let new_name = if ctx.interactive {
        let raw_name: String = input("New skill name")
            .placeholder("foo-custom")
            .validate(|s: &String| validate_fork_name(s.trim()))
            .interact()?;
        raw_name.trim().to_string()
    } else {
        let suffix = fork_suffix.ok_or_else(|| {
            AppError::Config("fork requires --fork-suffix in non-interactive mode".into())
        })?;
        let candidate = format!("{}-{}", installed.name, suffix.trim());
        validate_fork_name(&candidate).map_err(|e| AppError::Config(e.to_string()))?;
        candidate
    };
    fork_op_for_name(installed, library_root, &new_name)
}

fn fork_op_for_name(
    installed: &InstalledSkill,
    library_root: &Path,
    new_name: &str,
) -> Result<ApplyOp> {
    let library_parent = installed
        .source_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));
    let new_library_path = if library_parent.as_os_str().is_empty() {
        PathBuf::from(new_name)
    } else {
        library_parent.join(new_name)
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
        PathBuf::from(new_name)
    } else {
        local_parent.join(new_name)
    };

    Ok(ApplyOp::Fork {
        new_name: new_name.to_string(),
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
            format!("library has {library_changed} update(s); use `skillctl pull`")
        }
        SkillStatus::Unchanged => "no local changes".to_string(),
        SkillStatus::LocalMissing => "destination missing locally".to_string(),
        SkillStatus::LibraryMissing => "removed from library".to_string(),
        SkillStatus::SourceShaOrphaned => "source_sha orphan; can't classify".to_string(),
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

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use cliclack::{confirm, input, select};

use crate::prompt::multiselect;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::{OnDivergence, PushArgs};
use crate::commands::diff::{SkillStatus, classify};
use crate::commands::propagate;
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
    /// The library this skill belongs to (its provenance) — `push` writes each
    /// skill back to its own library, so a run may commit to several.
    library_root: PathBuf,
    library_name: String,
    library_url: String,
    library_access: config::Access,
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
    library_root: PathBuf,
    library_name: String,
    library_url: String,
    library_access: config::Access,
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
    // `--pr-title` becomes the PR/MR title AND the branch commit message, so it
    // must clear the same CRLF/trailer-forgery gate as `--message` — and here,
    // before any commit/push, not only at PR-open time (the branch is pushed
    // before `review` validates, so a late check would leak a forged commit).
    if let Some(title) = &args.pr_title {
        validate_message_safe("--pr-title", title)?;
    }

    let cfg = config::load()?;
    if cfg.libraries.is_empty() {
        return Err(AppError::Config(
            "no library configured — run `skillctl init <url>` first".into(),
        )
        .into());
    }

    // Fail fast on `--propagate` with no scan roots (flag- or config-supplied)
    // before we push anything, so the operator doesn't push then hit a config
    // error with the fan-out un-run.
    if args.propagate {
        propagate::resolve_scan_roots(&args.roots, &cfg)?;
    }

    // `--to` switches push into promotion mode: publish the selected skills
    // into a chosen writable library (rewriting their provenance), rather than
    // pushing each back to where it came from.
    if args.to.is_some() {
        return run_promote(&args, ctx, &cfg);
    }

    let cwd = std::env::current_dir().context("reading current directory")?;

    // `push` writes each skill back to its own provenance library, so a run
    // may commit to several caches. Lock every configured library's existing
    // cache up front (sorted + de-duplicated), then the project lock — keeping
    // the always-cache-before-project rule. Locking by config (not by the
    // manifest) is deterministic and avoids a read-before-lock race.
    let mut lock_paths: Vec<PathBuf> = cfg
        .libraries
        .iter()
        .filter_map(|l| config::library_cache_path(&l.url).ok())
        .filter(|p| p.exists())
        .collect();
    lock_paths.sort();
    lock_paths.dedup();
    let mut _cache_locks = Vec::with_capacity(lock_paths.len());
    for p in &lock_paths {
        _cache_locks.push(lock::acquire_exclusive(p, "library cache")?);
    }
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    let mut project_cfg = project_config::load(&cwd)?;
    if project_cfg.installed.is_empty() {
        ui::outro(
            ctx,
            "no skills installed in this project (.skills.toml is empty)",
        )?;
        emit_json(ctx, &[], None, None);
        return Ok(());
    }

    let mut fetched = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    for (index, installed) in project_cfg.installed.iter().enumerate() {
        // Route each skill to the library it was installed from, then gate on
        // that library's access: `push` commits directly only to `write`
        // libraries. A `read` source can't be written back (promotion via
        // `push --to` arrives next); a `pr` source needs the branch+MR/PR flow
        // (Phase 10F). Either way, skip with a clear reason.
        let library = match cfg.resolve_provenance(
            installed.library.as_deref(),
            installed.library_url.as_deref(),
        ) {
            Some(l) => l,
            None => {
                let origin = installed
                    .library
                    .as_deref()
                    .or(installed.library_url.as_deref())
                    .unwrap_or("an unknown library");
                ui::log_warning(
                    ctx,
                    format!(
                        "{} — installed from `{origin}`, which is no longer configured; skipping (run `skillctl library add` to restore it)",
                        installed.name
                    ),
                )?;
                continue;
            }
        };
        // A `read` source can't be written back (promotion via `push --to`
        // arrives in a later release). `write` commits directly; `pr` opens a
        // branch + PR/MR (handled per-library group below).
        if library.access == config::Access::Read {
            ui::log_info(
                ctx,
                format!(
                    "{} — installed from read-only library `{}`; skipping (promotion to a writable library via `push --to` arrives in the next release)",
                    installed.name, library.name
                ),
            )?;
            continue;
        }
        let library_root = match config::library_cache_path(&library.url) {
            Ok(p) if p.exists() => p,
            _ => {
                ui::log_warning(
                    ctx,
                    format!(
                        "{} — cache for library `{}` not found; skipping (run `skillctl library add {} {}` to clone it)",
                        installed.name, library.name, library.name, library.url
                    ),
                )?;
                continue;
            }
        };
        if fetched.insert(library_root.clone()) {
            if let Err(e) = git::fetch_and_fast_forward(&library_root) {
                ui::log_warning(
                    ctx,
                    format!(
                        "could not refresh `{}` ({e}); diff is computed against the cached HEAD",
                        library.name
                    ),
                )?;
            }
        }
        let status = classify(installed, &cwd, &library_root)?;
        let tags = skill::read_tags(&cwd.join(&installed.destination).join("SKILL.md"))
            .unwrap_or_default();
        candidates.push(Candidate {
            index,
            name: installed.name.clone(),
            destination: installed.destination.clone(),
            status,
            tags,
            library_root,
            library_name: library.name.clone(),
            library_url: library.url.clone(),
            library_access: library.access,
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
        emit_json(ctx, &[], None, None);
        return Ok(());
    }

    let selected_indices = select_pushable(&args, ctx, &pushable)?;
    if selected_indices.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, &[], None, None);
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
                        &candidate.library_root,
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
                        &candidate.library_root,
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
                library_root: candidate.library_root.clone(),
                library_name: candidate.library_name.clone(),
                library_url: candidate.library_url.clone(),
                library_access: candidate.library_access,
            });
        }
    }

    if applies.is_empty() {
        ui::outro(ctx, "nothing to push after conflict resolution")?;
        emit_json(ctx, &results, None, None);
        return Ok(());
    }

    // `push` writes each skill back to its own library, and each library is a
    // separate repo with its own commit + remote. Group the resolved applies
    // by library cache and run the apply → commit → push → record sequence
    // once per library. `commit` in the JSON reflects a single commit when the
    // run touched exactly one library (the common case); across several
    // libraries it is null and each result carries its own `source_sha`.
    let mut grouped: std::collections::BTreeMap<PathBuf, Vec<Apply>> =
        std::collections::BTreeMap::new();
    for apply in applies {
        grouped
            .entry(apply.library_root.clone())
            .or_default()
            .push(apply);
    }
    let mut commits: Vec<(String, String)> = Vec::new();
    for (_root, lib_applies) in grouped {
        let is_pr = lib_applies
            .first()
            .map(|a| a.library_access == config::Access::Pr)
            .unwrap_or(false);
        if is_pr {
            push_to_library_via_pr(ctx, &args, lib_applies, &cwd, &project_cfg, &mut results)?;
        } else {
            let group_commit = push_to_library(
                ctx,
                lib_applies,
                &cwd,
                &mut project_cfg,
                args.message.as_deref(),
                &mut results,
            )?;
            if let Some(c) = group_commit {
                commits.push(c);
            }
        }
    }

    // With `--propagate`, fan each just-pushed update out to every other
    // project on disk that installed it from the same library. Runs after the
    // library groups are committed + pushed so the propagated content is the
    // new HEAD.
    let propagated = if args.propagate {
        propagate_after_push(ctx, &args, &cfg, &cwd, &project_cfg, &results)?
    } else {
        Vec::new()
    };

    let pushed = results.iter().filter(|r| r["status"] == "pushed").count();
    let forked = results.iter().filter(|r| r["status"] == "forked").count();
    let pr_opened = results
        .iter()
        .filter(|r| r["status"] == "pr_opened")
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let prop_updated = propagated
        .iter()
        .filter(|r| r["status"] == "updated")
        .count();
    let mut parts = Vec::new();
    if pushed > 0 {
        parts.push(format!("pushed {pushed}"));
    }
    if forked > 0 {
        parts.push(format!("forked {forked}"));
    }
    if pr_opened > 0 {
        parts.push(format!("opened {pr_opened} PR/MR"));
    }
    if skipped > 0 {
        parts.push(format!("skipped {skipped}"));
    }
    if prop_updated > 0 {
        parts.push(format!("propagated to {prop_updated} project(s)"));
    }
    let summary = if parts.is_empty() {
        "nothing to push".to_string()
    } else {
        parts.join(", ")
    };
    ui::outro(ctx, summary)?;
    let commit_for_json = match commits.as_slice() {
        [one] => Some((one.0.as_str(), one.1.as_str())),
        _ => None,
    };
    emit_json(
        ctx,
        &results,
        commit_for_json,
        if args.propagate {
            Some(propagated.as_slice())
        } else {
            None
        },
    );
    Ok(())
}

/// After a successful round-trip push, propagate each pushed update to every
/// other install site on disk. Only `pushed` (update) results propagate — forks
/// and `--to` promotions are new skills / new provenance and never fan out.
/// Pushed skills are grouped by their owning library (each is a separate repo
/// with its own HEAD); the project that just pushed is skipped by canonical
/// path (it's already up to date and still holds its own lock). All configured
/// caches are already locked by `run`, so this only takes per-site project
/// locks.
fn propagate_after_push(
    ctx: &Context,
    args: &PushArgs,
    cfg: &config::Config,
    cwd: &Path,
    project_cfg: &project_config::ProjectConfig,
    results: &[Value],
) -> Result<Vec<Value>> {
    let pushed_names: Vec<String> = results
        .iter()
        .filter(|r| r["status"] == "pushed")
        .filter_map(|r| r["name"].as_str().map(str::to_string))
        .collect();
    if pushed_names.is_empty() {
        return Ok(Vec::new());
    }

    let roots = propagate::resolve_scan_roots(&args.roots, cfg)?;
    let cwd_canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());

    // Group the pushed skills by the library they belong to.
    let mut by_library: std::collections::BTreeMap<String, (config::Library, Vec<String>)> =
        std::collections::BTreeMap::new();
    for name in &pushed_names {
        let Some(entry) = project_cfg.installed.iter().find(|i| &i.name == name) else {
            continue;
        };
        let Some(library) =
            cfg.resolve_provenance(entry.library.as_deref(), entry.library_url.as_deref())
        else {
            continue;
        };
        by_library
            .entry(library.name.clone())
            .or_insert_with(|| (library.clone(), Vec::new()))
            .1
            .push(name.clone());
    }

    let mut out: Vec<Value> = Vec::new();
    for (_lib_name, (library, names)) in by_library {
        let library_root = match config::library_cache_path(&library.url) {
            Ok(p) if p.exists() => p,
            _ => continue,
        };
        let head_sha = match git::head_sha(&library_root) {
            Ok(s) => s,
            Err(e) => {
                ui::log_warning(
                    ctx,
                    format!(
                        "could not read HEAD of `{}` for propagation: {e}",
                        library.name
                    ),
                )?;
                continue;
            }
        };
        let wanted: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
        let mut lib_results = propagate::propagate_core(
            ctx,
            &library,
            &library_root,
            &head_sha,
            &wanted,
            &roots,
            false,
            Some(&cwd_canonical),
        )?;
        out.append(&mut lib_results);
    }
    Ok(out)
}

/// Apply, commit, and push one library group. Mutates `project_cfg` in memory
/// (source_sha updates / fork replacements), persists it, performs the
/// fork local-folder renames, and appends per-skill JSON entries to `results`.
/// Returns the new commit `(sha, message)` if the group committed anything.
#[allow(clippy::too_many_arguments)]
fn push_to_library(
    ctx: &Context,
    applies: Vec<Apply>,
    cwd: &Path,
    project_cfg: &mut project_config::ProjectConfig,
    message_override: Option<&str>,
    results: &mut Vec<Value>,
) -> Result<Option<(String, String)>> {
    let library_root = applies[0].library_root.clone();
    let library_name = applies[0].library_name.clone();
    let library_url = applies[0].library_url.clone();

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
        let local_dir = safe_join(cwd, &installed.destination)?;
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
        ui::log_warning(
            ctx,
            format!("nothing pushed to `{library_name}` (all selected skills failed to apply)"),
        )?;
        return Ok(None);
    }

    if !git::has_staged_changes(&library_root).map_err(|e| AppError::Git(e.to_string()))? {
        ui::log_info(
            ctx,
            format!("no effective changes after applying selections for `{library_name}`"),
        )?;
        return Ok(None);
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
    let message = message_override
        .map(str::to_string)
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
                    cwd,
                    &project_cfg.installed[apply.candidate_index].destination,
                )?;
                let abs_new = safe_join(cwd, new_local_destination)?;
                project_cfg.installed[apply.candidate_index] = InstalledSkill {
                    name: new_name.clone(),
                    source_path: new_library_path.clone(),
                    source_sha: new_sha.clone(),
                    destination: new_local_destination.clone(),
                    installed_at: installed_at.clone(),
                    library: Some(library_name.clone()),
                    library_url: Some(library_url.clone()),
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
    project_config::save(cwd, project_cfg)?;

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

    Ok(Some((new_sha, message)))
}

/// Open a PR/MR for one `pr`-access library group. Unlike `push_to_library`,
/// this never commits to the library's default branch and never touches
/// `.skills.toml`: the skill isn't merged yet. It creates a `skillctl/<slug>`
/// branch off the base, applies the selected skills' content, commits, pushes
/// the branch, and opens a PR/MR via `gh`/`glab`, then returns the cache to its
/// base branch. Appends a `pr_opened` result (with the request URL) per group.
fn push_to_library_via_pr(
    ctx: &Context,
    args: &PushArgs,
    applies: Vec<Apply>,
    cwd: &Path,
    project_cfg: &project_config::ProjectConfig,
    results: &mut Vec<Value>,
) -> Result<()> {
    let library_root = applies[0].library_root.clone();
    let library_name = applies[0].library_name.clone();
    let library_url = applies[0].library_url.clone();

    let host = crate::review::detect_host(
        &crate::host::parse_remote_url(&library_url)
            .map_err(|e| AppError::Config(e.to_string()))?
            .host,
    );

    // The skills' display names (fork ops use the new name) drive the branch
    // slug, title, and body.
    let names: Vec<String> = applies
        .iter()
        .map(|a| match &a.op {
            ApplyOp::Fork { new_name, .. } => new_name.clone(),
            ApplyOp::Update => project_cfg.installed[a.candidate_index].name.clone(),
        })
        .collect();
    let branch = pr_branch_name(&names);
    let auto_title = pr_title(&names);
    let mut title = args.pr_title.clone().unwrap_or(auto_title);
    let body = args
        .message
        .clone()
        .unwrap_or_else(|| format!("Opened by skillctl.\n\nSkills: {}", names.join(", ")));

    // Confirm before any branch/push when interactive. Title is editable.
    if ctx.interactive && !args.yes {
        title = input("PR/MR title")
            .default_input(&title)
            .interact()
            .unwrap_or(title);
        let proceed = confirm(format!(
            "Open a PR/MR to `{library_name}` (branch `{branch}` → {} skill(s))?",
            names.len()
        ))
        .interact()?;
        if !proceed {
            ui::log_info(ctx, format!("skipped PR/MR to `{library_name}`"))?;
            for name in &names {
                results.push(
                    json!({"name": name, "status": "skipped", "reason": "PR/MR not confirmed"}),
                );
            }
            return Ok(());
        }
    }

    let base = git::current_branch(&library_root).map_err(|e| AppError::Git(e.to_string()))?;
    git::create_branch(&library_root, &branch).map_err(|e| AppError::Git(e.to_string()))?;

    // Apply each skill's content onto the branch (continue-on-error).
    let mut applied_any = false;
    for apply in &applies {
        let installed = &project_cfg.installed[apply.candidate_index];
        let installed_name = installed.name.clone();
        let local_dir = safe_join(cwd, &installed.destination)?;
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
            Ok(()) => applied_any = true,
            Err(e) => {
                let _ =
                    ui::log_warning(ctx, format!("PR apply failed for `{installed_name}`: {e}"));
                let _ = git::checkout_paths(&library_root, &library_relative);
                results.push(
                    json!({"name": installed_name, "status": "failed", "reason": e.to_string()}),
                );
            }
        }
    }

    // Always return the cache to its base branch on the way out (success or
    // not), so the next command sees a clean default branch.
    let restore_base = |ctx: &Context| {
        if let Err(e) = git::checkout_branch(&library_root, &base) {
            let _ = ui::log_warning(
                ctx,
                format!("could not return `{library_name}` cache to `{base}`: {e}"),
            );
        }
    };

    if !applied_any
        || !git::has_staged_changes(&library_root).map_err(|e| AppError::Git(e.to_string()))?
    {
        ui::log_info(
            ctx,
            format!("no effective changes for `{library_name}`; no PR/MR opened"),
        )?;
        restore_base(ctx);
        return Ok(());
    }

    if let Err(e) = git::commit(&library_root, &title) {
        restore_base(ctx);
        return Err(AppError::Git(e.to_string()).into());
    }
    if let Err(e) = git::push_branch(&library_root, &branch) {
        restore_base(ctx);
        return Err(AppError::Git(e.to_string()).into());
    }

    let url = match crate::review::open_review_request(
        &host,
        &library_root,
        &branch,
        &base,
        &title,
        &body,
    ) {
        Ok(u) => u,
        Err(e) => {
            // The branch is pushed; only the request failed. Don't lose that.
            ui::log_warning(
                ctx,
                format!(
                    "branch `{branch}` pushed to `{library_name}`, but opening the PR/MR failed: {e}"
                ),
            )?;
            restore_base(ctx);
            return Err(e.into());
        }
    };
    restore_base(ctx);

    ui::log_success(ctx, format!("PR/MR opened for `{library_name}`: {url}"))?;
    for name in &names {
        results.push(json!({
            "name": name,
            "status": "pr_opened",
            "operation": "pr",
            "branch": branch,
            "pr_url": url,
        }));
    }
    Ok(())
}

/// Branch name for a PR/MR run: `skillctl/<slug>`, where the slug joins the
/// skill names, sanitised to a git-ref-safe charset and length-capped.
fn pr_branch_name(names: &[String]) -> String {
    let joined = names.join("-");
    let mut slug: String = joined
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    slug.truncate(40);
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "skillctl/update".to_string()
    } else {
        format!("skillctl/{slug}")
    }
}

fn pr_title(names: &[String]) -> String {
    match names {
        [one] => format!("Update skill `{one}`"),
        many => format!("Update {} skills: {}", many.len(), many.join(", ")),
    }
}

/// One planned promotion: which installed entry, where it lands in the target
/// library, and — when a collision was resolved by forking — the new name and
/// new local destination.
struct PromotePlan {
    index: usize,
    target_relative: PathBuf,
    fork: Option<(String, PathBuf)>,
}

/// Promotion mode (`push --to <lib>`): publish the selected installed skills
/// into a chosen writable library, rewriting their provenance to it. Unlike the
/// round-trip, the skill need not already exist in the target — its local
/// content is added there. On a target-path collision the `--on-divergence`
/// policy (overwrite / fork = add as new skill / skip) decides.
fn run_promote(args: &PushArgs, ctx: &Context, cfg: &config::Config) -> Result<()> {
    let to = args.to.as_deref().unwrap_or_default();
    let target = cfg.resolve_write(Some(to))?.clone();
    if target.access == config::Access::Pr {
        return Err(AppError::Config(format!(
            "promotion to `{}` (pr access) isn't supported yet — choose a write-access library",
            target.name
        ))
        .into());
    }
    let target_root =
        config::library_cache_path(&target.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !target_root.exists() {
        return Err(AppError::Config(format!(
            "cache for library `{}` not found — run `skillctl library add {} {}` to clone it",
            target.name, target.name, target.url
        ))
        .into());
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let _cache_lock = lock::acquire_exclusive(&target_root, "library cache")?;
    if let Err(e) = git::fetch_and_fast_forward(&target_root) {
        ui::log_warning(
            ctx,
            format!(
                "could not refresh `{}` ({e}); promoting against the cached HEAD",
                target.name
            ),
        )?;
    }
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;
    let mut project_cfg = project_config::load(&cwd)?;
    if project_cfg.installed.is_empty() {
        ui::outro(
            ctx,
            "no skills installed in this project (.skills.toml is empty)",
        )?;
        emit_json(ctx, &[], None, None);
        return Ok(());
    }

    let selected = select_for_promotion(args, ctx, &cwd, &project_cfg)?;
    if selected.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, &[], None, None);
        return Ok(());
    }

    let mut results: Vec<Value> = Vec::new();
    let mut plans: Vec<PromotePlan> = Vec::new();
    // Target paths already claimed by an earlier plan in THIS run. A path is a
    // collision if it exists on disk OR another selected skill is already
    // landing there — otherwise two skills sharing a `source_path` would both
    // pass the on-disk check (nothing is written until the apply loop) and the
    // second would silently overwrite the first, while the first's provenance
    // still gets rewritten to claim it.
    let mut planned: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for idx in selected {
        let installed = &project_cfg.installed[idx];
        let name = installed.name.clone();
        let local_dir = safe_join(&cwd, &installed.destination)?;
        if !local_dir.exists() {
            ui::log_warning(
                ctx,
                format!(
                    "{name} — local folder {} not found; skipping",
                    installed.destination.display()
                ),
            )?;
            results
                .push(json!({"name": name, "status": "skipped", "reason": "local folder missing"}));
            continue;
        }
        let target_relative = installed.source_path.clone();
        let collides = safe_join(&target_root, &target_relative)?.exists()
            || planned.contains(&crate::path_safety::normalize_lexical(&target_relative));
        if !collides {
            planned.insert(crate::path_safety::normalize_lexical(&target_relative));
            plans.push(PromotePlan {
                index: idx,
                target_relative,
                fork: None,
            });
            continue;
        }
        // Collision in the target library (or with another skill in this run):
        // resolve.
        match resolve_promotion_collision(ctx, args, &name, &target.name)? {
            DivergenceChoice::Overwrite => {
                planned.insert(crate::path_safety::normalize_lexical(&target_relative));
                plans.push(PromotePlan {
                    index: idx,
                    target_relative,
                    fork: None,
                });
            }
            DivergenceChoice::Fork => {
                if let ApplyOp::Fork {
                    new_name,
                    new_library_path,
                    new_local_destination,
                } = resolve_fork_op(ctx, installed, &target_root, args.fork_suffix.as_deref())?
                {
                    planned.insert(crate::path_safety::normalize_lexical(&new_library_path));
                    plans.push(PromotePlan {
                        index: idx,
                        target_relative: new_library_path,
                        fork: Some((new_name, new_local_destination)),
                    });
                }
            }
            DivergenceChoice::Skip => {
                ui::log_info(ctx, format!("skipped {name}"))?;
                results.push(json!({
                    "name": name,
                    "status": "skipped",
                    "reason": format!("already exists in `{}`", target.name),
                }));
            }
        }
    }

    if plans.is_empty() {
        ui::outro(ctx, "nothing to promote")?;
        emit_json(ctx, &results, None, None);
        return Ok(());
    }

    // Apply each plan's content onto the target cache (continue-on-error).
    let mut applied: Vec<PromotePlan> = Vec::new();
    for plan in plans {
        let installed = &project_cfg.installed[plan.index];
        let name = installed.name.clone();
        let local_dir = safe_join(&cwd, &installed.destination)?;
        let target_dir = safe_join(&target_root, &plan.target_relative)?;
        let outcome: Result<()> = (|| {
            fs_util::replace_folder_contents(&local_dir, &target_dir)?;
            git::add_all(&target_root, &plan.target_relative)
                .map_err(|e| AppError::Git(e.to_string()))?;
            Ok(())
        })();
        match outcome {
            Ok(()) => applied.push(plan),
            Err(e) => {
                let _ = ui::log_warning(ctx, format!("promotion failed for `{name}`: {e}"));
                let _ = git::checkout_paths(&target_root, &plan.target_relative);
                results.push(json!({"name": name, "status": "failed", "reason": e.to_string()}));
            }
        }
    }

    if applied.is_empty()
        || !git::has_staged_changes(&target_root).map_err(|e| AppError::Git(e.to_string()))?
    {
        ui::outro(ctx, "nothing promoted")?;
        emit_json(ctx, &results, None, None);
        return Ok(());
    }

    let names: Vec<String> = applied
        .iter()
        .map(|p| match &p.fork {
            Some((new_name, _)) => new_name.clone(),
            None => project_cfg.installed[p.index].name.clone(),
        })
        .collect();
    let message = args.message.clone().unwrap_or_else(|| {
        if names.len() == 1 {
            format!("add skill: {}", names[0])
        } else {
            format!("add skills: {}", names.join(", "))
        }
    });
    let new_sha = git::commit(&target_root, &message).map_err(|e| AppError::Git(e.to_string()))?;
    if let Err(e) = git::push(&target_root) {
        if let Err(rollback_err) = git::reset_hard_to_parent(&target_root) {
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

    for plan in &applied {
        let idx = plan.index;
        let orig_name = project_cfg.installed[idx].name.clone();
        match &plan.fork {
            None => {
                let entry = &mut project_cfg.installed[idx];
                entry.source_path = plan.target_relative.clone();
                entry.source_sha = new_sha.clone();
                entry.library = Some(target.name.clone());
                entry.library_url = Some(target.url.clone());
                entry.installed_at = installed_at.clone();
                ui::log_success(ctx, format!("promoted {orig_name} → `{}`", target.name))?;
                results.push(json!({
                    "name": orig_name,
                    "status": "promoted",
                    "library": target.name,
                    "source_sha": new_sha,
                }));
            }
            Some((new_name, new_local_destination)) => {
                let abs_old = safe_join(&cwd, &project_cfg.installed[idx].destination)?;
                let abs_new = safe_join(&cwd, new_local_destination)?;
                project_cfg.installed[idx] = InstalledSkill {
                    name: new_name.clone(),
                    source_path: plan.target_relative.clone(),
                    source_sha: new_sha.clone(),
                    destination: new_local_destination.clone(),
                    installed_at: installed_at.clone(),
                    library: Some(target.name.clone()),
                    library_url: Some(target.url.clone()),
                };
                if abs_old != abs_new {
                    if let Some(parent) = abs_new.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    if let Err(e) = fs::rename(&abs_old, &abs_new) {
                        ui::log_warning(
                            ctx,
                            format!(
                                "promoted `{orig_name}` → `{new_name}` in `{}`, but the local rename failed: {e}. `.skills.toml` records the new destination; rename the local folder by hand.",
                                target.name
                            ),
                        )?;
                    }
                }
                ui::log_success(
                    ctx,
                    format!("promoted {orig_name} → `{new_name}` in `{}`", target.name),
                )?;
                results.push(json!({
                    "name": orig_name,
                    "status": "promoted",
                    "new_name": new_name,
                    "library": target.name,
                    "source_sha": new_sha,
                }));
            }
        }
    }
    project_config::save(&cwd, &project_cfg)?;

    let promoted = results.iter().filter(|r| r["status"] == "promoted").count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let summary = if skipped > 0 {
        format!("promoted {promoted}, skipped {skipped}")
    } else {
        format!("promoted {promoted} skill(s)")
    };
    ui::outro(ctx, summary)?;
    emit_json(
        ctx,
        &results,
        Some((new_sha.as_str(), message.as_str())),
        None,
    );
    Ok(())
}

/// Resolve a target-library path collision during promotion. Interactive: a
/// three-way Select; non-interactive: the `--on-divergence` policy, or skip
/// with a warning when none was given (never clobber by default).
fn resolve_promotion_collision(
    ctx: &Context,
    args: &PushArgs,
    name: &str,
    target_name: &str,
) -> Result<DivergenceChoice> {
    if let Some(policy) = args.on_divergence {
        return Ok(DivergenceChoice::from(policy));
    }
    if !ctx.interactive {
        ui::log_warning(
            ctx,
            format!(
                "{name} already exists in `{target_name}` and no --on-divergence policy was given; skipping"
            ),
        )?;
        return Ok(DivergenceChoice::Skip);
    }
    Ok(select(format!(
        "`{name}` already exists in `{target_name}` — what do you want to do?"
    ))
    .item(
        DivergenceChoice::Overwrite,
        "Overwrite",
        "replace the target library's version with your local content",
    )
    .item(
        DivergenceChoice::Fork,
        "Add as a new skill",
        "publish under a new name; the existing skill stays untouched",
    )
    .item(
        DivergenceChoice::Skip,
        "Cancel",
        "leave the target library untouched",
    )
    .interact()?)
}

/// Pick which installed skills to promote. Any installed skill is eligible
/// (including those installed from a read-only source — that's the point).
fn select_for_promotion(
    args: &PushArgs,
    ctx: &Context,
    cwd: &Path,
    project_cfg: &project_config::ProjectConfig,
) -> Result<Vec<usize>> {
    let installed = &project_cfg.installed;
    if args.all {
        return Ok((0..installed.len()).collect());
    }
    if !args.skills.is_empty() {
        let mut chosen = Vec::with_capacity(args.skills.len());
        for name in &args.skills {
            let idx = installed
                .iter()
                .position(|i| &i.name == name)
                .ok_or_else(|| AppError::Config(format!("no installed skill named `{name}`")))?;
            chosen.push(idx);
        }
        return Ok(chosen);
    }
    if !args.tags.is_empty() {
        let matched: Vec<usize> = installed
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                let tags = skill::read_tags(&cwd.join(&i.destination).join("SKILL.md"))
                    .unwrap_or_default();
                matches_tags(&tags, &args.tags, args.all_tags)
            })
            .map(|(idx, _)| idx)
            .collect();
        if matched.is_empty() {
            return Err(AppError::Config(format!(
                "no installed skill matches the requested tag(s): {}",
                args.tags.join(", ")
            ))
            .into());
        }
        if !ctx.interactive {
            return Ok(matched);
        }
        let mut prompt = multiselect("Skills to promote (tag-filtered)").required(true);
        for idx in &matched {
            prompt = prompt.item(*idx, &installed[*idx].name, "", Vec::new());
        }
        return prompt.interact();
    }
    if !ctx.interactive {
        return Err(AppError::Config(
            "no skills selected — pass --skill <name> (repeatable), --tag <name>, or --all".into(),
        )
        .into());
    }
    let mut prompt = multiselect("Skills to promote").required(true);
    for (idx, i) in installed.iter().enumerate() {
        let hint = i.library.as_deref().unwrap_or("default");
        prompt = prompt.item(idx, &i.name, hint, Vec::new());
    }
    prompt.interact()
}

fn emit_json(
    ctx: &Context,
    results: &[Value],
    commit: Option<(&str, &str)>,
    propagated: Option<&[Value]>,
) {
    if !ctx.json {
        return;
    }
    let promoted = results.iter().filter(|r| r["status"] == "promoted").count();
    let pushed = results.iter().filter(|r| r["status"] == "pushed").count();
    let forked = results.iter().filter(|r| r["status"] == "forked").count();
    let pr_opened = results
        .iter()
        .filter(|r| r["status"] == "pr_opened")
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let commit_value = commit.map(|(sha, message)| json!({"sha": sha, "message": message}));
    let mut out = json!({
        "command": "push",
        "results": results,
        "commit": commit_value,
        "summary": {
            "pushed": pushed,
            "forked": forked,
            "promoted": promoted,
            "pr_opened": pr_opened,
            "skipped": skipped,
        },
    });
    // Only present when `--propagate` ran, so a plain push's JSON is unchanged.
    if let Some(prop) = propagated {
        let updated = prop.iter().filter(|r| r["status"] == "updated").count();
        let would = prop
            .iter()
            .filter(|r| r["status"] == "would-update")
            .count();
        let sk = prop.iter().filter(|r| r["status"] == "skipped").count();
        out["propagated"] = json!({
            "results": prop,
            "summary": { "updated": updated, "would_update": would, "skipped": sk },
        });
    }
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
            prompt = prompt.item(c.index, &c.name, hint, c.tags.clone());
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
        prompt = prompt.item(c.index, &c.name, hint, c.tags.clone());
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

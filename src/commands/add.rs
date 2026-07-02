use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use cliclack::{input, select};

use crate::prompt::multiselect;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::{AddArgs, OnConflict};
use crate::commands::shared::{audit_gate, matches_tags, short_hint};
use crate::config;
use crate::context::Context;
use crate::error::AppError;
use crate::fs_util;
use crate::git;
use crate::lock;
use crate::path_safety::normalize_lexical;
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
    ui::intro(ctx, "skillctl add")?;

    let cfg = config::load()?;
    let has_selection = args.all || !args.skills.is_empty() || !args.tags.is_empty();
    let from_all = args.from.as_deref() == Some("all");

    // `--from <url>` (a git URL or `github:`/`gitlab:` shorthand that isn't an
    // already-configured library name) installs ad-hoc from a remote source.
    // A known library name always wins, so this can't shadow a configured lib.
    if let Some(from) = args.from.as_deref() {
        if !from_all && cfg.by_name(from).is_none() && looks_like_remote_source(from) {
            return run_remote(&args, ctx, &cfg, from);
        }
    }
    // Interactive cross-library browsing via tabs: an explicit `--from all`, or
    // a plain `add` when several libraries are configured — both only when no
    // selection flag pins the choice. `--from <name>` always targets one
    // library (no tabs).
    if ctx.interactive
        && !has_selection
        && (from_all || (args.from.is_none() && cfg.libraries.len() > 1))
    {
        return run_tabbed(&args, ctx, &cfg);
    }
    if from_all {
        return run_multi(&args, ctx, &cfg);
    }
    let library = cfg.resolve_read(args.from.as_deref())?.clone();

    // Installing from a non-default library means third-party (untrusted)
    // content, for which the content audit is mandatory — `--no-audit` is
    // refused so the audit can never be silenced for content you don't own.
    if args.no_audit && !library.default {
        return Err(AppError::Config(format!(
            "refusing --no-audit when installing from the non-default library `{}`: third-party content is always audited; drop --no-audit",
            library.name
        ))
        .into());
    }

    let library_root =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !library_root.exists() {
        return Err(AppError::Config(format!(
            "cache for library `{}` not found at {} — re-clone it with `skillctl library add {} <url>` (or `skillctl init <url>` for the default library)",
            library.name,
            fs_util::display_path(&library_root),
            library.name
        ))
        .into());
    }
    // Serialise all library-cache mutations across concurrent skillctl
    // processes. Released on function return.
    let _cache_lock = lock::acquire_exclusive(&library_root, "library cache")?;

    if let Err(e) = git::fetch_and_fast_forward(&library_root) {
        ui::log_warning(
            ctx,
            format!("could not refresh library cache ({e}); using cached version"),
        )?;
    }

    let discovered = skill::discover(&library_root, false)?;
    for w in &discovered.warnings {
        ui::log_warning(ctx, w)?;
    }
    let skills = discovered.skills;
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

    // Content audit of the (untrusted) skill content before anything lands in
    // the project. Warn-only by default; `--fail-on <severity>` refuses the
    // whole batch (nothing is installed) if any selected skill reaches the
    // threshold; `--no-audit` skips entirely. The returned map (skill name →
    // verdict) is surfaced per-skill in the JSON output so non-interactive
    // consumers get the audit signal even in warn-only mode.
    let audit_verdicts = if args.no_audit {
        HashMap::new()
    } else {
        audit_gate(
            ctx,
            selected.iter().map(|s| (s.name.as_str(), s.path.as_path())),
            args.fail_on.map(Into::into),
        )?
    };

    let cwd = std::env::current_dir().context("reading current directory")?;
    // Serialise concurrent skillctl runs on this project's .skills.toml.
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;
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
        let folder_name = match skill.path.file_name() {
            Some(n) => n.to_os_string(),
            None => {
                let _ = ui::log_warning(
                    ctx,
                    format!(
                        "skipping {}: source has no folder name ({})",
                        skill.name,
                        skill.path.display()
                    ),
                );
                results.push(json!({
                    "name": skill.name,
                    "status": "failed",
                    "reason": "source has no folder name",
                }));
                continue;
            }
        };
        let dest = dest_root.join(&folder_name);

        if dest.exists() {
            let action = resolve_conflict(ctx, &dest, conflict_policy.clone())?;
            match action {
                ConflictAction::Overwrite => {
                    if let Err(e) = fs::remove_dir_all(&dest)
                        .with_context(|| format!("removing {}", dest.display()))
                    {
                        let _ =
                            ui::log_warning(ctx, format!("add failed for `{}`: {e}", skill.name));
                        results.push(json!({
                            "name": skill.name,
                            "status": "failed",
                            "reason": e.to_string(),
                        }));
                        continue;
                    }
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
                    // Save what's been installed up to this point before
                    // bailing — we may already have pushed entries for
                    // earlier-loop-iteration skills.
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

        // Per-skill IIFE: a single skill's copy failure (symlink reject,
        // hardlink reject, FIFO inside, oversize SKILL.md, ...) logs and
        // continues with the next skill instead of aborting the batch and
        // orphaning partial work. The final `project_config::save` below
        // captures every successful entry, so a half-finished `--all` run
        // still produces a usable `.skills.toml`.
        let outcome: Result<InstalledSkill> = (|| {
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
            Ok(InstalledSkill {
                name: skill.name.clone(),
                source_path,
                source_sha: source_sha.clone(),
                destination: destination_rel,
                installed_at: installed_at.clone(),
                library: Some(library.name.clone()),
                library_url: Some(library.url.clone()),
            })
        })();
        match outcome {
            Ok(entry) => {
                let dest_display = entry.destination.display().to_string();
                let source_sha_for_json = entry.source_sha.clone();
                project_cfg.installed.push(entry);
                ui::log_success(ctx, format!("{} → {}", skill.name, dest.display()))?;
                results.push(json!({
                    "name": skill.name,
                    "status": "installed",
                    "path": dest_display,
                    "source_sha": source_sha_for_json,
                    "audit_verdict": audit_verdicts.get(skill.name.as_str()).copied(),
                }));
            }
            Err(e) => {
                // Best-effort cleanup of any partial copy left behind by
                // copy_dir_all's tempdir-or-final-rename path. The atomic
                // swap pattern means dst is either old or new content, so
                // there's typically nothing to undo — but if the failure
                // happened during the staging copy we may still have a
                // `.skillctl-tmp.*` sibling around (it normally cleans
                // itself up; this is a paranoid sweep).
                let _ = ui::log_warning(ctx, format!("add failed for `{}`: {e}", skill.name));
                results.push(json!({
                    "name": skill.name,
                    "status": "failed",
                    "reason": e.to_string(),
                }));
            }
        }
    }

    // Always save: captures both fully-successful batches and partial
    // successes after one or more per-skill failures.
    project_config::save(&cwd, &project_cfg)?;

    if !aborted {
        ui::outro(ctx, summary_text(&results))?;
    }
    emit_json(ctx, Some(&dest_root), &results);
    Ok(())
}

/// Multi-source install (`--from all`): install matching skills from **every**
/// configured library in one run. Selection is flag-driven (`--all` / `--skill`
/// / `--tag`) — interactive cross-library browsing (tabs) is a later step. The
/// default library is processed first so it keeps the un-suffixed name when two
/// libraries offer the same skill; later collisions are suffixed `-<library>`.
/// The content audit is mandatory across the span (the span is third-party by
/// definition unless it is the lone default library).
fn run_multi(args: &AddArgs, ctx: &Context, cfg: &config::Config) -> Result<()> {
    if cfg.libraries.is_empty() {
        return Err(AppError::Config(
            "no libraries configured — run `skillctl init <url>` or `skillctl library add <name> <url>` first".into(),
        )
        .into());
    }
    if !args.all && args.skills.is_empty() && args.tags.is_empty() {
        return Err(AppError::Config(
            "`--from all` needs a selection in non-interactive mode: pass --all, --skill <name>, or --tag <name> (or run interactively to browse libraries with tabs)".into(),
        )
        .into());
    }
    // `--no-audit` is refused as soon as the span includes a non-default
    // (untrusted third-party) library.
    if args.no_audit && cfg.libraries.iter().any(|l| !l.default) {
        return Err(AppError::Config(
            "refusing --no-audit with `--from all`: the span includes non-default (third-party) libraries, whose content is always audited; drop --no-audit".into(),
        )
        .into());
    }

    // Default library first so it keeps the bare name on a collision.
    let mut libs: Vec<&config::Library> = Vec::new();
    if let Some(d) = cfg.default_library() {
        libs.push(d);
    }
    for l in &cfg.libraries {
        if !l.default {
            libs.push(l);
        }
    }

    // Resolve cache paths; drop unreachable libraries with a warning.
    let mut reachable: Vec<(&config::Library, PathBuf)> = Vec::new();
    for lib in libs {
        match config::library_cache_path(&lib.url) {
            Ok(p) if p.exists() => reachable.push((lib, p)),
            Ok(p) => ui::log_warning(
                ctx,
                format!(
                    "skipping `{}`: cache not found at {} (re-clone with `skillctl library add {} <url>`)",
                    lib.name,
                    fs_util::display_path(&p),
                    lib.name
                ),
            )?,
            Err(e) => ui::log_warning(ctx, format!("skipping `{}`: {e}", lib.name))?,
        }
    }
    if reachable.is_empty() {
        return Err(AppError::Config("no reachable libraries to install from".into()).into());
    }

    let cwd = std::env::current_dir().context("reading current directory")?;

    // Acquire every distinct cache lock up front (sorted, de-duplicated path
    // order so concurrent `--from all` runs request them in the same order),
    // then the project lock — preserving the codebase-wide cache-before-project
    // ordering. The locks are non-blocking, so a contended run fails fast
    // rather than hanging regardless.
    let mut lock_paths: Vec<PathBuf> = reachable.iter().map(|(_, p)| p.clone()).collect();
    lock_paths.sort();
    lock_paths.dedup();
    let mut _cache_locks = Vec::with_capacity(lock_paths.len());
    for p in &lock_paths {
        _cache_locks.push(lock::acquire_exclusive(p, "library cache")?);
    }
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    // Refresh + discover + select per library (default-first order).
    let mut selected: Vec<(Skill, config::Library, PathBuf)> = Vec::new();
    let mut matched_skill_names: HashSet<String> = HashSet::new();
    for (lib, root) in &reachable {
        if let Err(e) = git::fetch_and_fast_forward(root) {
            ui::log_warning(
                ctx,
                format!(
                    "could not refresh `{}` ({e}); using cached version",
                    lib.name
                ),
            )?;
        }
        let discovered = skill::discover(root, false)?;
        for w in &discovered.warnings {
            ui::log_warning(ctx, w)?;
        }
        for s in discovered.skills {
            let take = if args.all {
                true
            } else if !args.skills.is_empty() {
                let hit = args.skills.iter().any(|n| n == &s.name);
                if hit {
                    matched_skill_names.insert(s.name.clone());
                }
                hit
            } else {
                matches_tags(&s.tags, &args.tags, args.all_tags)
            };
            if take {
                selected.push((s, (*lib).clone(), root.clone()));
            }
        }
    }

    // Requested `--skill` names that matched in no library.
    for name in &args.skills {
        if !matched_skill_names.contains(name) {
            ui::log_warning(
                ctx,
                format!("no skill named `{name}` in any library; skipped"),
            )?;
        }
    }

    if selected.is_empty() {
        ui::outro(ctx, "no matching skills found")?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    install_multi_source(args, ctx, &cwd, selected)
}

/// Interactive multi-library install: open the picker with a tab per library
/// (default first), discovered eagerly before entering raw mode so no git work
/// runs inside the TUI. Selection accumulates across tabs and feeds the same
/// install path as `--from all`. Reached only in interactive mode with no
/// selection flags.
fn run_tabbed(args: &AddArgs, ctx: &Context, cfg: &config::Config) -> Result<()> {
    if cfg.libraries.is_empty() {
        return Err(AppError::Config(
            "no libraries configured — run `skillctl init <url>` or `skillctl library add <name> <url>` first".into(),
        )
        .into());
    }
    if args.no_audit && cfg.libraries.iter().any(|l| !l.default) {
        return Err(AppError::Config(
            "refusing --no-audit: a non-default (third-party) library is configured and its content is always audited; drop --no-audit".into(),
        )
        .into());
    }

    // Default library first → it becomes the opening tab.
    let mut libs: Vec<&config::Library> = Vec::new();
    if let Some(d) = cfg.default_library() {
        libs.push(d);
    }
    for l in &cfg.libraries {
        if !l.default {
            libs.push(l);
        }
    }

    let mut reachable: Vec<(&config::Library, PathBuf)> = Vec::new();
    for lib in libs {
        match config::library_cache_path(&lib.url) {
            Ok(p) if p.exists() => reachable.push((lib, p)),
            Ok(p) => ui::log_warning(
                ctx,
                format!(
                    "skipping `{}`: cache not found at {} (re-clone with `skillctl library add {} <url>`)",
                    lib.name,
                    fs_util::display_path(&p),
                    lib.name
                ),
            )?,
            Err(e) => ui::log_warning(ctx, format!("skipping `{}`: {e}", lib.name))?,
        }
    }
    if reachable.is_empty() {
        return Err(AppError::Config("no reachable libraries to install from".into()).into());
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    // Every distinct cache lock first (sorted + deduped, so concurrent runs
    // request them in the same order), then the project lock — the same
    // cache-before-project ordering as `run_multi`; non-blocking, so a
    // contended run fails fast rather than hanging.
    let mut lock_paths: Vec<PathBuf> = reachable.iter().map(|(_, p)| p.clone()).collect();
    lock_paths.sort();
    lock_paths.dedup();
    let mut _cache_locks = Vec::with_capacity(lock_paths.len());
    for p in &lock_paths {
        _cache_locks.push(lock::acquire_exclusive(p, "library cache")?);
    }
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    // Eager per-tab discovery (before raw mode).
    let mut prompt = crate::prompt::tabbed::<(Skill, config::Library, PathBuf)>(
        "Skills to install — ←/→ switch library",
    )
    .required(true);
    let mut total = 0usize;
    for (lib, root) in &reachable {
        if let Err(e) = git::fetch_and_fast_forward(root) {
            ui::log_warning(
                ctx,
                format!(
                    "could not refresh `{}` ({e}); using cached version",
                    lib.name
                ),
            )?;
        }
        let discovered = skill::discover(root, false)?;
        for w in &discovered.warnings {
            ui::log_warning(ctx, w)?;
        }
        let items: Vec<((Skill, config::Library, PathBuf), String, String)> = discovered
            .skills
            .into_iter()
            .map(|s| {
                let label = s.name.clone();
                let hint = s.description.as_deref().map(short_hint).unwrap_or_default();
                ((s, (*lib).clone(), root.clone()), label, hint)
            })
            .collect();
        total += items.len();
        prompt = prompt.tab(lib.name.clone(), items);
    }

    if total == 0 {
        ui::outro(ctx, "no skills found in any configured library")?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    let selected = prompt.interact()?;
    if selected.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    install_multi_source(args, ctx, &cwd, selected)
}

/// Shared install tail for multi-source add: plan collision-suffixed names,
/// audit the whole span (atomic on `--fail-on`), copy each skill, and record
/// provenance. Used by both flag-driven `--from all` and the interactive
/// library-tabs path. Cache + project locks are held by the caller.
/// True if a `--from` value looks like a remote git source rather than a
/// configured library name: a `github:`/`gitlab:` shorthand or an explicit
/// HTTPS / SSH / scp URL. Library names never carry a scheme or `@host:` shape.
fn looks_like_remote_source(s: &str) -> bool {
    s.starts_with("github:")
        || s.starts_with("gitlab:")
        || s.starts_with("https://")
        || s.starts_with("ssh://")
        || s.starts_with("git@")
        || s.contains("://")
}

/// Expand a `github:owner/repo` / `gitlab:owner/repo` shorthand to a full HTTPS
/// URL; pass anything else through unchanged for `host::parse_remote_url`.
fn expand_remote_source(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("github:") {
        format!("https://github.com/{rest}")
    } else if let Some(rest) = s.strip_prefix("gitlab:") {
        format!("https://gitlab.com/{rest}")
    } else {
        s.to_string()
    }
}

/// Default library name for an ad-hoc remote source: the last path segment,
/// reduced to a filesystem/identifier-safe form.
fn derive_library_name(path: &str) -> String {
    let last = path.rsplit('/').next().unwrap_or(path);
    let name: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        "library".to_string()
    } else {
        name
    }
}

/// Ad-hoc install from a remote git source passed to `--from` (a full URL or a
/// `github:`/`gitlab:` shorthand) that isn't a configured library. The source
/// is cloned into the URL-keyed cache (refreshed if already present); its
/// content is **always** audited (third-party by definition — `--no-audit` is
/// refused); and the selected skills are installed with URL provenance.
/// Optionally the source is registered as a `read` library (`--save-as`, or an
/// interactive offer) so `skillctl pull` can track it afterward.
fn run_remote(args: &AddArgs, ctx: &Context, cfg: &config::Config, source: &str) -> Result<()> {
    if args.no_audit {
        return Err(AppError::Config(
            "refusing --no-audit when installing from a remote URL: third-party content is always audited; drop --no-audit".into(),
        )
        .into());
    }

    let expanded = expand_remote_source(source);
    let remote = crate::host::parse_remote_url(&expanded)
        .map_err(|e| AppError::Config(format!("invalid source `{source}`: {e}")))?;
    let display_url = config::sanitize_url_for_display(&expanded);

    // If the URL resolves to an already-configured library, install from it as
    // that library (its own name/access/provenance) — `--from <url>` becomes an
    // alias for `--from <name>`, and we never re-save it.
    let existing = cfg.libraries.iter().find(|l| {
        crate::host::parse_remote_url(&l.url)
            .map(|r| r.normalized == remote.normalized)
            .unwrap_or(false)
    });

    let cache_path =
        config::library_cache_path(&display_url).map_err(|e| AppError::Config(e.to_string()))?;
    let existed = cache_path.exists();
    if !existed {
        if let Some(parent) = cache_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating cache dir {}", parent.display()))?;
        }
        ui::log_info(ctx, format!("cloning {display_url} …"))?;
        git::clone(&expanded, &cache_path).map_err(|e| AppError::Git(e.to_string()))?;
    }
    let _cache_lock = lock::acquire_exclusive(&cache_path, "library cache")?;
    if existed {
        if let Err(e) = git::fetch_and_fast_forward(&cache_path) {
            ui::log_warning(
                ctx,
                format!("could not refresh {display_url} ({e}); using cached version"),
            )?;
        }
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    let discovered = skill::discover(&cache_path, false)?;
    for w in &discovered.warnings {
        ui::log_warning(ctx, w)?;
    }
    let skills = discovered.skills;
    if skills.is_empty() {
        ui::outro(ctx, format!("no skills found at {display_url}"))?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    let selected = select_skills(args, ctx, &skills)?;
    if selected.is_empty() {
        ui::outro(ctx, "no skills selected")?;
        emit_json(ctx, None, &[]);
        return Ok(());
    }

    // Decide which library this install is recorded under, and whether to
    // persist it as a read-access source so `pull` can track it later.
    let (lib, save) = if let Some(e) = existing {
        ((*e).clone(), false)
    } else if let Some(name) = &args.save_as {
        crate::sanitize::validate_identifier("--save-as", name)?;
        (
            config::Library {
                name: name.clone(),
                url: display_url.clone(),
                access: config::Access::Read,
                default: false,
            },
            true,
        )
    } else if ctx.interactive
        && cliclack::confirm(format!(
            "Keep {display_url} as a library to pull updates from later?"
        ))
        .interact()?
    {
        let default_name = derive_library_name(&remote.path);
        let name: String = input("Library name")
            .default_input(&default_name)
            .validate(|s: &String| crate::sanitize::validate_identifier("library name", s.trim()))
            .interact()?;
        (
            config::Library {
                name: name.trim().to_string(),
                url: display_url.clone(),
                access: config::Access::Read,
                default: false,
            },
            true,
        )
    } else {
        (
            config::Library {
                name: derive_library_name(&remote.path),
                url: display_url.clone(),
                access: config::Access::Read,
                default: false,
            },
            false,
        )
    };

    // Persist the library BEFORE the install closes the UI (its outro/JSON is
    // emitted by install_multi_source). A duplicate name is a soft failure.
    if save {
        let outcome: Result<()> = (|| {
            let mut cfg2 = config::load()?;
            cfg2.add_library(lib.clone(), false)?;
            config::save(&cfg2)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                ui::log_success(ctx, format!("saved library `{}` ({display_url})", lib.name))?
            }
            Err(e) => ui::log_warning(
                ctx,
                format!("installed, but could not save the library: {e}"),
            )?,
        }
    }

    let selected_tuples: Vec<(Skill, config::Library, PathBuf)> = selected
        .into_iter()
        .map(|s| (s, lib.clone(), cache_path.clone()))
        .collect();
    install_multi_source(args, ctx, &cwd, selected_tuples)
}

fn install_multi_source(
    args: &AddArgs,
    ctx: &Context,
    cwd: &Path,
    selected: Vec<(Skill, config::Library, PathBuf)>,
) -> Result<()> {
    let cwd = cwd.to_path_buf();
    let dest_root = resolve_destination(args, ctx, &cwd)?;
    let conflict_policy: Option<ConflictAction> = args.on_conflict.map(Into::into);
    let mut project_cfg = project_config::load(&cwd)?;

    // Plan final names, suffixing `-<library>` on a collision with an
    // already-tracked entry or an earlier skill in this same run.
    struct Plan {
        skill: Skill,
        lib: config::Library,
        root: PathBuf,
        name: String,
        folder: OsString,
    }
    let mut taken_names: HashSet<String> = project_cfg
        .installed
        .iter()
        .map(|i| i.name.clone())
        .collect();
    // Keyed by a normalized absolute path so the seed (from existing
    // `.skills.toml` entries) and the per-skill check (computed from a possibly
    // relative `--dest`) compare in the same form — otherwise a relative
    // `--dest` never matches a seeded entry and a differently-named incoming
    // skill could clobber a tracked folder / duplicate a destination.
    let mut taken_dests: HashSet<PathBuf> = project_cfg
        .installed
        .iter()
        .map(|i| normalize_lexical(&cwd.join(&i.destination)))
        .collect();
    let mut plans: Vec<Plan> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    for (skill, lib, root) in selected {
        let folder_base = match skill.path.file_name() {
            Some(n) => n.to_os_string(),
            None => {
                let _ = ui::log_warning(
                    ctx,
                    format!("skipping {}: source has no folder name", skill.name),
                );
                results.push(json!({
                    "name": skill.name,
                    "status": "failed",
                    "reason": "source has no folder name",
                }));
                continue;
            }
        };
        let base_name = skill.name.clone();
        let mut name = base_name.clone();
        let mut folder = folder_base.clone();
        if name_or_dest_taken(&taken_names, &taken_dests, &cwd, &dest_root, &name, &folder) {
            let folder_str = folder_base.to_string_lossy();
            name = format!("{base_name}-{}", lib.name);
            folder = OsString::from(format!("{folder_str}-{}", lib.name));
            let mut n = 2;
            while name_or_dest_taken(&taken_names, &taken_dests, &cwd, &dest_root, &name, &folder) {
                name = format!("{base_name}-{}-{n}", lib.name);
                folder = OsString::from(format!("{folder_str}-{}-{n}", lib.name));
                n += 1;
            }
        }
        taken_names.insert(name.clone());
        taken_dests.insert(dest_key(&cwd, &dest_root, &folder));
        plans.push(Plan {
            skill,
            lib,
            root,
            name,
            folder,
        });
    }

    // Audit the whole span. Warn-only unless `--fail-on`; a breach refuses the
    // entire batch atomically (nothing is copied below). Keyed by the planned
    // (collision-suffixed) name so the per-skill JSON verdict lines up.
    let verdicts = audit_gate(
        ctx,
        plans
            .iter()
            .map(|p| (p.name.as_str(), p.skill.path.as_path())),
        args.fail_on.map(Into::into),
    )?;

    let installed_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting installation timestamp")?;
    let mut head_cache: HashMap<PathBuf, String> = HashMap::new();
    let mut aborted = false;
    for plan in plans {
        let dest = dest_root.join(&plan.folder);
        if dest.exists() {
            match resolve_conflict(ctx, &dest, conflict_policy.clone())? {
                ConflictAction::Overwrite => {
                    if let Err(e) = fs::remove_dir_all(&dest)
                        .with_context(|| format!("removing {}", dest.display()))
                    {
                        let _ =
                            ui::log_warning(ctx, format!("add failed for `{}`: {e}", plan.name));
                        results.push(json!({
                            "name": plan.name,
                            "status": "failed",
                            "reason": e.to_string(),
                        }));
                        continue;
                    }
                }
                ConflictAction::Skip => {
                    ui::log_info(ctx, format!("skipped {}", plan.name))?;
                    results.push(json!({
                        "name": plan.name,
                        "status": "skipped",
                        "reason": format!("destination {} already exists", dest.display()),
                    }));
                    continue;
                }
                ConflictAction::Abort => {
                    project_config::save(&cwd, &project_cfg)?;
                    ui::outro_cancel(ctx, "aborted")?;
                    results.push(json!({
                        "name": plan.name,
                        "status": "aborted",
                        "reason": format!("destination {} already exists", dest.display()),
                    }));
                    aborted = true;
                    break;
                }
            }
        }

        let source_sha = match head_cache.get(&plan.root) {
            Some(s) => s.clone(),
            None => {
                let s = git::head_sha(&plan.root).map_err(|e| AppError::Git(e.to_string()))?;
                head_cache.insert(plan.root.clone(), s.clone());
                s
            }
        };

        let outcome: Result<InstalledSkill> = (|| {
            fs_util::copy_dir_all(&plan.skill.path, &dest)?;
            let source_path = plan
                .skill
                .path
                .strip_prefix(&plan.root)
                .with_context(|| {
                    format!(
                        "computing path of {} relative to library at {}",
                        plan.skill.path.display(),
                        plan.root.display()
                    )
                })?
                .to_path_buf();
            let destination_rel = fs_util::relative_to_or_self(&dest, &cwd);
            Ok(InstalledSkill {
                name: plan.name.clone(),
                source_path,
                source_sha: source_sha.clone(),
                destination: destination_rel,
                installed_at: installed_at.clone(),
                library: Some(plan.lib.name.clone()),
                library_url: Some(plan.lib.url.clone()),
            })
        })();
        match outcome {
            Ok(entry) => {
                let dest_display = entry.destination.display().to_string();
                let source_sha_for_json = entry.source_sha.clone();
                project_cfg.installed.push(entry);
                ui::log_success(
                    ctx,
                    format!(
                        "{} → {} (from {})",
                        plan.name,
                        dest.display(),
                        plan.lib.name
                    ),
                )?;
                results.push(json!({
                    "name": plan.name,
                    "status": "installed",
                    "path": dest_display,
                    "library": plan.lib.name,
                    "source_sha": source_sha_for_json,
                    "audit_verdict": verdicts.get(plan.name.as_str()).copied(),
                }));
            }
            Err(e) => {
                let _ = ui::log_warning(ctx, format!("add failed for `{}`: {e}", plan.name));
                results.push(json!({
                    "name": plan.name,
                    "status": "failed",
                    "reason": e.to_string(),
                }));
            }
        }
    }

    project_config::save(&cwd, &project_cfg)?;
    if !aborted {
        ui::outro(ctx, summary_text(&results))?;
    }
    emit_json(ctx, Some(&dest_root), &results);
    Ok(())
}

/// Normalized absolute key for a destination folder, so destinations expressed
/// relative to `cwd` (a relative `--dest`) and absolute seeds compare equal.
fn dest_key(cwd: &Path, dest_root: &Path, folder: &OsStr) -> PathBuf {
    let joined = dest_root.join(folder);
    let abs = if joined.is_absolute() {
        joined
    } else {
        cwd.join(joined)
    };
    normalize_lexical(&abs)
}

/// True when `name` (as a `.skills.toml` entry) or the destination folder is
/// already claimed — drives the `-<library>` collision suffix in multi-source
/// add. Destinations are compared via [`dest_key`] so relative and absolute
/// spellings of the same path match.
fn name_or_dest_taken(
    taken_names: &HashSet<String>,
    taken_dests: &HashSet<PathBuf>,
    cwd: &Path,
    dest_root: &Path,
    name: &str,
    folder: &OsStr,
) -> bool {
    taken_names.contains(name) || taken_dests.contains(&dest_key(cwd, dest_root, folder))
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
        // Reject parent traversal unconditionally — there is no legitimate
        // workflow that needs `..` in `--dest`. Reject absolute paths in
        // non-interactive mode (agent-mode threat model: flag values may be
        // attacker-supplied via the agent's prompt). In interactive mode the
        // operator is typing the value themselves, so absolute is allowed.
        for component in dest.components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(AppError::Config(format!(
                    "invalid --dest `{}`: parent traversal (`..`) is not allowed",
                    dest.display()
                ))
                .into());
            }
        }
        if dest.is_absolute() && !ctx.interactive {
            return Err(AppError::Config(format!(
                "invalid --dest `{}`: absolute paths are not allowed in non-interactive mode (the operator's flag values may be agent-supplied; use a path relative to the current directory)",
                dest.display()
            ))
            .into());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_remote_source_distinguishes_urls_from_names() {
        assert!(looks_like_remote_source("github:owner/repo"));
        assert!(looks_like_remote_source("gitlab:group/sub/proj"));
        assert!(looks_like_remote_source("https://github.com/o/r"));
        assert!(looks_like_remote_source("ssh://git@h/o/r"));
        assert!(looks_like_remote_source("git@github.com:o/r.git"));
        // Plain library names are not remote sources.
        assert!(!looks_like_remote_source("personal"));
        assert!(!looks_like_remote_source("team-skills"));
        assert!(!looks_like_remote_source("all"));
    }

    #[test]
    fn expand_remote_source_expands_shorthands_only() {
        assert_eq!(expand_remote_source("github:o/r"), "https://github.com/o/r");
        assert_eq!(
            expand_remote_source("gitlab:g/s/p"),
            "https://gitlab.com/g/s/p"
        );
        // Full URLs pass through untouched.
        assert_eq!(
            expand_remote_source("https://github.com/o/r"),
            "https://github.com/o/r"
        );
        assert_eq!(
            expand_remote_source("git@github.com:o/r.git"),
            "git@github.com:o/r.git"
        );
    }

    #[test]
    fn derive_library_name_takes_last_segment() {
        assert_eq!(derive_library_name("owner/repo"), "repo");
        assert_eq!(derive_library_name("group/sub/project"), "project");
        assert_eq!(derive_library_name("repo"), "repo");
        // Non-identifier characters are reduced to `-`.
        assert_eq!(derive_library_name("o/weird name!"), "weird-name-");
    }

    #[test]
    fn dest_key_unifies_relative_and_seeded_forms() {
        let cwd = Path::new("/proj");
        // Seed form: an existing `.skills.toml` destination joined onto cwd.
        let seeded = normalize_lexical(&cwd.join(".claude/skills/shared"));
        // Check form: a relative `--dest` plus the folder name.
        let computed = dest_key(cwd, Path::new(".claude/skills"), OsStr::new("shared"));
        assert_eq!(
            seeded, computed,
            "relative --dest must collide with a seeded tracked destination"
        );
    }

    #[test]
    fn dest_key_absolute_dest_root_is_respected() {
        let cwd = Path::new("/proj");
        let computed = dest_key(cwd, Path::new("/abs/skills"), OsStr::new("foo"));
        assert_eq!(computed, Path::new("/abs/skills/foo"));
    }

    #[test]
    fn name_or_dest_taken_matches_seeded_destination() {
        let cwd = Path::new("/proj");
        let names: HashSet<String> = HashSet::new();
        let mut dests: HashSet<PathBuf> = HashSet::new();
        dests.insert(normalize_lexical(&cwd.join(".claude/skills/foo")));
        // Same folder via relative --dest must be reported taken.
        assert!(name_or_dest_taken(
            &names,
            &dests,
            cwd,
            Path::new(".claude/skills"),
            "anything",
            OsStr::new("foo")
        ));
        // A different folder is free.
        assert!(!name_or_dest_taken(
            &names,
            &dests,
            cwd,
            Path::new(".claude/skills"),
            "anything",
            OsStr::new("bar")
        ));
    }
}

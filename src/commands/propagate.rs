use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ignore::WalkBuilder;
use serde_json::{Value, json};

use crate::cli::PropagateArgs;
use crate::commands::diff::{SkillStatus, classify};
use crate::config::{self, Library};
use crate::context::Context;
use crate::error::AppError;
use crate::fs_util;
use crate::git;
use crate::lock;
use crate::path_safety::safe_join;
use crate::project_config;
use crate::ui;

pub fn run(args: PropagateArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl propagate")?;

    let cfg = config::load()?;
    let library = cfg.resolve_read(args.from.as_deref())?.clone();
    let library_root =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !library_root.exists() {
        return Err(AppError::Config(format!(
            "cache for library `{}` not found at {} — run `skillctl library add {} <url>` (or `skillctl init <url>` for the default library)",
            library.name,
            fs_util::display_path(&library_root),
            library.name
        ))
        .into());
    }

    // Hold the cache lock for the whole run so HEAD can't move under us while we
    // fan out. Cache-before-project ordering: this is taken before any per-site
    // project lock below.
    let _cache_lock = lock::acquire_exclusive(&library_root, "library cache")?;
    if let Err(e) = git::fetch_and_fast_forward(&library_root) {
        ui::log_warning(
            ctx,
            format!(
                "could not refresh `{}` ({e}); propagating the cached HEAD",
                library.name
            ),
        )?;
    }
    let head_sha = git::head_sha(&library_root).map_err(|e| AppError::Git(e.to_string()))?;

    let wanted: HashSet<&str> = args.skills.iter().map(String::as_str).collect();
    let roots = resolve_scan_roots(&args.roots, &cfg)?;
    let results = propagate_core(
        ctx,
        &library,
        &library_root,
        &head_sha,
        &wanted,
        &roots,
        args.dry_run,
        None,
    )?;

    let updated = results.iter().filter(|r| r["status"] == "updated").count();
    let would = results
        .iter()
        .filter(|r| r["status"] == "would-update")
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let up_to_date = results
        .iter()
        .filter(|r| r["status"] == "up-to-date")
        .count();

    if results
        .iter()
        .all(|r| r["status"] == "up-to-date" || r["status"] == "site-error")
        && updated == 0
        && would == 0
        && skipped == 0
    {
        ui::log_info(
            ctx,
            format!(
                "no install site references `{}` from library `{}`",
                args.skills.join(", "),
                library.name
            ),
        )?;
    }

    let summary = if args.dry_run {
        format!(
            "{would} project(s) would update, {up_to_date} up to date, {skipped} skipped (dry run)"
        )
    } else {
        format!("updated {updated} project(s), {up_to_date} up to date, {skipped} skipped")
    };
    ui::outro(ctx, summary)?;
    emit_json(ctx, &args, &library.name, &results);
    Ok(())
}

/// Resolve which directories to scan for install sites: the `--root` flags
/// when given, otherwise the configured `[propagate] roots`. Errors when
/// neither supplies a root so the caller never scans nothing by accident.
pub fn resolve_scan_roots(cli_roots: &[PathBuf], cfg: &config::Config) -> Result<Vec<PathBuf>> {
    if !cli_roots.is_empty() {
        return Ok(cli_roots.to_vec());
    }
    if !cfg.propagate.roots.is_empty() {
        return Ok(cfg.propagate.roots.clone());
    }
    Err(AppError::Config(
        "no scan roots — pass `--root <path>` (repeatable) or set `[propagate] roots` in config.toml"
            .into(),
    )
    .into())
}

/// Fan `library`'s current version of the `wanted` skills out to every install
/// site discovered under `roots`. The caller is responsible for holding the
/// cache lock and resolving `head_sha` (the version to propagate) — this walks,
/// classifies, and applies but never touches the library. `skip_site`, when
/// set, is a canonicalized project path to exclude: `push --propagate` passes
/// the project it just wrote, which is already up to date and whose project
/// lock is still held.
#[allow(clippy::too_many_arguments)]
pub fn propagate_core(
    ctx: &Context,
    library: &Library,
    library_root: &Path,
    head_sha: &str,
    wanted: &HashSet<&str>,
    roots: &[PathBuf],
    dry_run: bool,
    skip_site: Option<&Path>,
) -> Result<Vec<Value>> {
    let sites = discover_sites(roots);
    ui::log_info(
        ctx,
        format!(
            "scanned {} root(s); found {} project(s) with a .skills.toml",
            roots.len(),
            sites.len()
        ),
    )?;

    let mut results: Vec<Value> = Vec::new();
    for site in &sites {
        if let Some(skip) = skip_site {
            let canonical = std::fs::canonicalize(site).unwrap_or_else(|_| site.clone());
            if canonical == skip {
                continue;
            }
        }
        match process_site(ctx, site, library, library_root, head_sha, wanted, dry_run) {
            Ok(mut site_results) => results.append(&mut site_results),
            Err(e) => {
                ui::log_warning(
                    ctx,
                    format!("{}: skipped ({e})", fs_util::display_path(site)),
                )?;
                results.push(json!({
                    "project": fs_util::display_path(site),
                    "status": "site-error",
                    "reason": e.to_string(),
                }));
            }
        }
    }
    Ok(results)
}

/// Walk each root for files literally named `.skills.toml`, returning the
/// project directory (its parent). Skips `node_modules`/`target`/`.git` and
/// `.gitignore`d paths; de-duplicates by canonical path so a project reachable
/// from two roots is visited once.
fn discover_sites(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut sites = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .filter_entry(|e| {
                let n = e.file_name();
                n != "node_modules" && n != "target" && n != ".git"
            })
            .build();
        for entry in walker.flatten() {
            if entry.file_name() != ".skills.toml" {
                continue;
            }
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let Some(dir) = entry.path().parent() else {
                continue;
            };
            let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            if seen.insert(canonical) {
                sites.push(dir.to_path_buf());
            }
        }
    }
    sites.sort();
    sites
}

#[allow(clippy::too_many_arguments)]
fn process_site(
    ctx: &Context,
    site: &Path,
    library: &Library,
    library_root: &Path,
    head_sha: &str,
    wanted: &HashSet<&str>,
    dry_run: bool,
) -> Result<Vec<Value>> {
    // Lock before reading: serialise against a concurrent skillctl run in this
    // project and protect the whole read-modify-write. Non-blocking, so a
    // contended site is skipped rather than hanging the whole fan-out.
    let _lock = lock::acquire_exclusive(site, "project")
        .map_err(|e| anyhow::anyhow!("could not lock project ({e})"))?;

    let mut project_cfg = project_config::load(site)?;
    let site_label = fs_util::display_path(site);
    let mut results: Vec<Value> = Vec::new();
    let mut changed = false;

    for i in 0..project_cfg.installed.len() {
        let entry = &project_cfg.installed[i];
        if !wanted.contains(entry.name.as_str()) {
            continue;
        }
        if !library.matches_provenance(entry.library.as_deref(), entry.library_url.as_deref()) {
            continue;
        }
        let name = entry.name.clone();
        let status = match classify(entry, site, library_root) {
            Ok(s) => s,
            Err(e) => {
                results.push(skip(&site_label, &name, format!("classify failed: {e}")));
                continue;
            }
        };
        match status {
            SkillStatus::LibraryAhead { .. } => {
                if dry_run {
                    results.push(json!({
                        "project": site_label,
                        "skill": name,
                        "status": "would-update",
                    }));
                    continue;
                }
                let entry = &project_cfg.installed[i];
                let outcome: Result<()> = (|| {
                    let library_dir = safe_join(library_root, &entry.source_path)?;
                    let local_dir = safe_join(site, &entry.destination)?;
                    fs_util::replace_folder_contents(&library_dir, &local_dir)?;
                    Ok(())
                })();
                match outcome {
                    Ok(()) => {
                        project_cfg.installed[i].source_sha = head_sha.to_string();
                        changed = true;
                        ui::log_success(
                            ctx,
                            format!("{site_label}: {name} → {}", short(head_sha)),
                        )?;
                        results.push(json!({
                            "project": site_label,
                            "skill": name,
                            "status": "updated",
                            "source_sha": head_sha,
                        }));
                    }
                    Err(e) => {
                        let _ = ui::log_warning(ctx, format!("{site_label}: {name} failed ({e})"));
                        results.push(json!({
                            "project": site_label,
                            "skill": name,
                            "status": "failed",
                            "reason": e.to_string(),
                        }));
                    }
                }
            }
            SkillStatus::Unchanged => results.push(json!({
                "project": site_label,
                "skill": name,
                "status": "up-to-date",
            })),
            SkillStatus::LocalChangesOnly | SkillStatus::BothDiverged { .. } => results.push(skip(
                &site_label,
                &name,
                "local edits — run `skillctl push`/`pull` here".into(),
            )),
            SkillStatus::LocalMissing => results.push(skip(
                &site_label,
                &name,
                "destination folder is missing".into(),
            )),
            SkillStatus::LibraryMissing => results.push(skip(
                &site_label,
                &name,
                "no longer exists in the library".into(),
            )),
            SkillStatus::SourceShaOrphaned => results.push(skip(
                &site_label,
                &name,
                "recorded source_sha not in the library".into(),
            )),
        }
    }

    if changed {
        project_config::save(site, &project_cfg)?;
    }
    Ok(results)
}

fn skip(project: &str, skill: &str, reason: String) -> Value {
    json!({ "project": project, "skill": skill, "status": "skipped", "reason": reason })
}

fn short(sha: &str) -> &str {
    &sha[..7.min(sha.len())]
}

fn emit_json(ctx: &Context, args: &PropagateArgs, library: &str, results: &[Value]) {
    if !ctx.json {
        return;
    }
    let updated = results.iter().filter(|r| r["status"] == "updated").count();
    let would = results
        .iter()
        .filter(|r| r["status"] == "would-update")
        .count();
    let skipped = results.iter().filter(|r| r["status"] == "skipped").count();
    let up_to_date = results
        .iter()
        .filter(|r| r["status"] == "up-to-date")
        .count();
    let out = json!({
        "command": "propagate",
        "skills": args.skills,
        "library": library,
        "dry_run": args.dry_run,
        "results": results,
        "summary": {
            "updated": updated,
            "would_update": would,
            "skipped": skipped,
            "up_to_date": up_to_date,
        },
    });
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "").unwrap();
    }

    fn site_names(sites: &[PathBuf]) -> HashSet<String> {
        sites
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn discover_finds_nested_projects_and_prunes_vendor_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(&root.join("a/.skills.toml"));
        touch(&root.join("sub/b/.skills.toml"));
        // A `.skills.toml` buried in node_modules must NOT be discovered.
        touch(&root.join("a/node_modules/pkg/.skills.toml"));
        touch(&root.join("c/target/.skills.toml"));

        let names = site_names(&discover_sites(&[root.to_path_buf()]));
        assert!(names.contains("a"), "top-level project found");
        assert!(names.contains("b"), "nested project found");
        assert!(!names.contains("pkg"), "node_modules pruned");
        assert!(
            !names.iter().any(|n| n == "c"),
            "a .skills.toml only under target/ is pruned"
        );
    }

    #[test]
    fn discover_dedups_a_project_reachable_from_two_roots() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        touch(&root.join("proj/.skills.toml"));
        let sites = discover_sites(&[root.to_path_buf(), root.to_path_buf()]);
        let count = sites
            .iter()
            .filter(|p| p.file_name().unwrap() == "proj")
            .count();
        assert_eq!(count, 1, "the same project is visited once");
    }
}

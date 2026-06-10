use anyhow::Result;
use serde_json::json;

use crate::cli::ListArgs;
use crate::commands::shared::matches_tags;
use crate::config::{self, Library};
use crate::context::Context;
use crate::error::AppError;
use crate::git;
use crate::lock;
use crate::skill::{self, Skill};
use crate::ui;

pub fn run(args: ListArgs, ctx: &Context) -> Result<()> {
    if args.from.as_deref() == Some("all") {
        return run_all(&args, ctx);
    }

    let cfg = config::load()?;
    let library = cfg.resolve_read(args.from.as_deref())?.clone();
    let (repo, filtered) = list_library(&library, &args, ctx)?;

    if ctx.json {
        let entries: Vec<_> = filtered.iter().map(|s| skill_json(s, &repo)).collect();
        let out = json!({
            "command": "list",
            "library": library.url,
            "skills": entries,
        });
        println!("{out}");
        return Ok(());
    }

    if filtered.is_empty() {
        if args.tags.is_empty() {
            println!("no skills found in {}", library.url);
        } else {
            println!(
                "no skills match the requested tag(s): {}",
                args.tags.join(", ")
            );
        }
        return Ok(());
    }

    for s in &filtered {
        print_skill(s);
    }
    Ok(())
}

fn run_all(args: &ListArgs, ctx: &Context) -> Result<()> {
    let cfg = config::load()?;
    if cfg.libraries.is_empty() {
        return Err(AppError::Config(
            "no libraries configured — run `skillctl init <url>` or `skillctl library add <name> <url>` first".into(),
        )
        .into());
    }

    let mut per_lib: Vec<(Library, std::path::PathBuf, Vec<Skill>)> =
        Vec::with_capacity(cfg.libraries.len());
    for lib in &cfg.libraries {
        match list_library(lib, args, ctx) {
            Ok((repo, skills)) => per_lib.push((lib.clone(), repo, skills)),
            // A single unreachable library (bad URL, missing cache) shouldn't
            // sink the whole span — warn and carry on with the others.
            Err(e) => ui::log_warning(ctx, format!("skipping `{}`: {e}", lib.name))?,
        }
    }

    if ctx.json {
        let libraries: Vec<_> = per_lib
            .iter()
            .map(|(lib, repo, skills)| {
                json!({
                    "name": lib.name,
                    "url": lib.url,
                    "access": lib.access.as_str(),
                    "default": lib.default,
                    "skills": skills.iter().map(|s| skill_json(s, repo)).collect::<Vec<_>>(),
                })
            })
            .collect();
        let out = json!({
            "command": "list",
            "from": "all",
            "libraries": libraries,
        });
        println!("{out}");
        return Ok(());
    }

    for (lib, _repo, skills) in &per_lib {
        let marker = if lib.default { " (default)" } else { "" };
        println!(
            "{} [{}]{marker} — {}",
            lib.name,
            lib.access.as_str(),
            lib.url
        );
        if skills.is_empty() {
            let note = if args.tags.is_empty() {
                "(no skills)"
            } else {
                "(no skills match the requested tag(s))"
            };
            println!("  {note}");
        } else {
            for s in skills {
                print_skill(s);
            }
        }
    }
    Ok(())
}

/// Lock + refresh one library's cache, discover its skills, and apply the tag
/// filter. Returns the cache root (for repo-relative path display) and the
/// filtered skills. Emits cache-refresh and per-skill parse warnings via `ui`.
/// The cache lock is released when this function returns.
fn list_library(
    library: &Library,
    args: &ListArgs,
    ctx: &Context,
) -> Result<(std::path::PathBuf, Vec<Skill>)> {
    let repo =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !repo.exists() {
        return Err(AppError::Config(format!(
            "cache for library `{}` not found at {} — re-clone it with `skillctl library add {} <url>` (or `skillctl init <url>` for the default library)",
            library.name,
            crate::fs_util::display_path(&repo),
            library.name
        ))
        .into());
    }
    // Even read-only `list` mutates the cache via `git fetch && reset --hard`;
    // serialise to prevent concurrent index corruption with a sibling `push`
    // or `pull`. Released on function return.
    let _cache_lock = lock::acquire_exclusive(&repo, "library cache")?;

    if let Err(e) = git::fetch_and_fast_forward(&repo) {
        ui::log_warning(
            ctx,
            format!("could not refresh library cache ({e}); using cached version"),
        )?;
    }

    let discovered = skill::discover(&repo, false)?;
    for w in &discovered.warnings {
        ui::log_warning(ctx, w)?;
    }
    let filtered = discovered
        .skills
        .into_iter()
        .filter(|s| matches_tags(&s.tags, &args.tags, args.all_tags))
        .collect();
    Ok((repo, filtered))
}

fn skill_json(s: &Skill, repo: &std::path::Path) -> serde_json::Value {
    json!({
        "name": s.name,
        "path": s.path.strip_prefix(repo).unwrap_or(&s.path).display().to_string(),
        "description": s.description,
        "tags": s.tags,
    })
}

fn print_skill(s: &Skill) {
    let tags_suffix = if s.tags.is_empty() {
        String::new()
    } else {
        format!(" [{}]", s.tags.join(", "))
    };
    match &s.description {
        Some(desc) => println!("  {}{tags_suffix} — {desc}", s.name),
        None => println!("  {}{tags_suffix}", s.name),
    }
}

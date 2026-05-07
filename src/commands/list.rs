use anyhow::Result;
use serde_json::json;

use crate::cli::ListArgs;
use crate::commands::shared::matches_tags;
use crate::config;
use crate::context::Context;
use crate::error::AppError;
use crate::git;
use crate::skill;

pub fn run(args: ListArgs, ctx: &Context) -> Result<()> {
    let cfg = config::load()?;
    let library = cfg.library.ok_or_else(|| {
        AppError::Config("no library configured — run `skillctl init<github-url>` first".into())
    })?;

    let repo =
        config::library_cache_path(&library.url).map_err(|e| AppError::Config(e.to_string()))?;
    if !repo.exists() {
        return Err(AppError::Config(format!(
            "library cache not found at {} — run `skillctl init{}` again",
            repo.display(),
            library.url
        ))
        .into());
    }

    if let Err(e) = git::fetch_and_fast_forward(&repo)
        && !ctx.json
    {
        eprintln!("warning: could not refresh library cache ({e}); using cached version");
    }

    let skills = skill::discover(&repo)?;
    let filtered: Vec<_> = skills
        .into_iter()
        .filter(|s| matches_tags(&s.tags, &args.tags, args.all_tags))
        .collect();

    if ctx.json {
        let entries: Vec<_> = filtered
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "path": s.path.strip_prefix(&repo).unwrap_or(&s.path).display().to_string(),
                    "description": s.description,
                    "tags": s.tags,
                })
            })
            .collect();
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

    for s in filtered {
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
    Ok(())
}

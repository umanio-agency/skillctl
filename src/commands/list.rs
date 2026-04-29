use anyhow::{Result, anyhow};

use crate::cli::ListArgs;
use crate::config;
use crate::git;
use crate::skill;

pub fn run(_args: ListArgs) -> Result<()> {
    let cfg = config::load()?;
    let library = cfg.library.ok_or_else(|| {
        anyhow!("no library configured — run `skills init <github-url>` first")
    })?;

    let repo = config::library_cache_path(&library.url)?;
    if !repo.exists() {
        return Err(anyhow!(
            "library cache not found at {} — run `skills init {}` again",
            repo.display(),
            library.url
        ));
    }

    if let Err(e) = git::fetch_and_fast_forward(&repo) {
        eprintln!("warning: could not refresh library cache ({e}); using cached version");
    }

    let skills = skill::discover(&repo)?;
    if skills.is_empty() {
        println!("no skills found in {}", library.url);
        return Ok(());
    }

    for s in skills {
        match &s.description {
            Some(desc) => println!("  {} — {desc}", s.name),
            None => println!("  {}", s.name),
        }
    }
    Ok(())
}

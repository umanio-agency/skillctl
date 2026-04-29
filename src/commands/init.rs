use std::fs;

use anyhow::{Context, Result};

use crate::cli::InitArgs;
use crate::config::{self, Config, Library};
use crate::git;

pub fn run(args: InitArgs) -> Result<()> {
    git::ensure_available()?;

    let url = args.url;
    let dest = config::library_cache_path(&url)?;

    if dest.exists() {
        fs::remove_dir_all(&dest)
            .with_context(|| format!("removing existing cache at {}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating cache dir at {}", parent.display()))?;
    }

    println!("cloning {url} into {} ...", dest.display());
    git::clone(&url, &dest)?;

    let config = Config {
        library: Some(Library { url: url.clone() }),
    };
    config::save(&config)?;

    println!("library configured: {url}");
    Ok(())
}

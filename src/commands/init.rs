use std::fs;

use anyhow::{Context as _, Result};

use crate::cli::InitArgs;
use crate::config::{self, Config, Library};
use crate::context::Context;
use crate::error::AppError;
use crate::git;
use crate::ui;

pub fn run(args: InitArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl init")?;

    git::ensure_available().map_err(|e| AppError::Git(e.to_string()))?;

    let url = args.url;
    let dest = config::library_cache_path(&url).map_err(|e| AppError::Config(e.to_string()))?;

    if dest.exists() {
        fs::remove_dir_all(&dest)
            .with_context(|| format!("removing existing cache at {}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating cache dir at {}", parent.display()))?;
    }

    ui::log_info(ctx, format!("cloning {url} into {} ...", dest.display()))?;
    git::clone(&url, &dest).map_err(|e| AppError::Git(e.to_string()))?;

    let config = Config {
        library: Some(Library { url: url.clone() }),
    };
    config::save(&config).map_err(|e| AppError::Config(e.to_string()))?;

    ui::log_success(ctx, format!("library configured: {url}"))?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "init",
            "library": {
                "url": url,
                "cache_path": dest.display().to_string(),
            }
        });
        println!("{out}");
    } else {
        ui::outro(ctx, "done")?;
    }
    Ok(())
}

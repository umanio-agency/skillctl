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

    // `args.url` may carry an embedded `user:token@` for the one-time clone;
    // we use it as-is to call `git clone`, but never persist or echo it.
    // `display_url` is the sanitised form that lands in `config.toml`,
    // logs, JSON output, and any later error chain referencing `library.url`.
    let clone_url = args.url;
    let display_url = config::sanitize_url_for_display(&clone_url);
    let dest =
        config::library_cache_path(&display_url).map_err(|e| AppError::Config(e.to_string()))?;

    let dest_display = crate::fs_util::display_path(&dest);
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .with_context(|| format!("removing existing cache at {dest_display}"))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "creating cache dir at {}",
                crate::fs_util::display_path(parent)
            )
        })?;
    }

    ui::log_info(
        ctx,
        format!("cloning {display_url} into {dest_display} ..."),
    )?;
    git::clone(&clone_url, &dest).map_err(|e| AppError::Git(e.to_string()))?;

    let config = Config {
        library: Some(Library {
            url: display_url.clone(),
        }),
    };
    config::save(&config).map_err(|e| AppError::Config(e.to_string()))?;

    ui::log_success(ctx, format!("library configured: {display_url}"))?;

    if ctx.json {
        let out = serde_json::json!({
            "command": "init",
            "library": {
                "url": display_url,
                "cache_path": dest_display,
            }
        });
        println!("{out}");
    } else {
        ui::outro(ctx, "done")?;
    }
    Ok(())
}

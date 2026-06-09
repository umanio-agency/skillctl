use anyhow::Result;

use crate::cli::InitArgs;
use crate::commands::library::clone_into_cache;
use crate::config::{self, Access, Library, PRIMARY_LIBRARY_NAME};
use crate::context::Context;
use crate::error::AppError;
use crate::ui;

pub fn run(args: InitArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl init")?;

    // Clone first (validates the URL + credentials), then record/re-point the
    // primary library. `clone_into_cache` returns the sanitised display URL —
    // the raw `args.url` (which may carry one-time `user:token@`) is never
    // persisted.
    let (display_url, dest) = clone_into_cache(ctx, &args.url)?;
    let dest_display = crate::fs_util::display_path(&dest);

    let mut cfg = config::load()?;
    if let Some(personal) = cfg
        .libraries
        .iter_mut()
        .find(|l| l.name == PRIMARY_LIBRARY_NAME)
    {
        // Re-point the `personal` library specifically; keep its access and
        // default flag. (Re-pointing "whatever is default" would silently
        // rewrite a user-chosen team library's URL — `init` manages the
        // operator's own primary library, not whichever happens to be default.)
        personal.url = display_url.clone();
    } else {
        // No `personal` library yet: create it with write access, marked
        // default (a fresh primary is the default).
        cfg.add_library(
            Library {
                name: PRIMARY_LIBRARY_NAME.to_string(),
                url: display_url.clone(),
                access: Access::Write,
                default: false,
            },
            true,
        )
        .map_err(|e| AppError::Config(e.to_string()))?;
    }
    config::save(&cfg).map_err(|e| AppError::Config(e.to_string()))?;

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

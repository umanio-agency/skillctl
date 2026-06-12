use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde_json::json;

use crate::cli::{AccessArg, LibraryAddArgs, LibraryCommand, LibraryRefArgs};
use crate::config::{self, Access, Library};
use crate::context::Context;
use crate::error::AppError;
use crate::sanitize::validate_identifier;
use crate::{fs_util, git, ui};

impl From<AccessArg> for Access {
    fn from(a: AccessArg) -> Self {
        match a {
            AccessArg::Read => Access::Read,
            AccessArg::Write => Access::Write,
            AccessArg::Pr => Access::Pr,
        }
    }
}

pub fn run(cmd: LibraryCommand, ctx: &Context) -> Result<()> {
    match cmd {
        LibraryCommand::Add(args) => add(args, ctx),
        LibraryCommand::List => list(ctx),
        LibraryCommand::Remove(args) => remove(args, ctx),
        LibraryCommand::SetDefault(args) => set_default(args, ctx),
    }
}

/// Clone (or re-clone) `clone_url` into the per-library cache. Returns the
/// sanitised display URL (what gets persisted) and the cache path. The raw
/// `clone_url` may carry one-time `user:token@` userinfo for the clone; it is
/// never persisted or echoed — only `display_url` is. Shared with `init`,
/// which is sugar over "add the first library and mark it default".
pub fn clone_into_cache(ctx: &Context, clone_url: &str) -> Result<(String, PathBuf)> {
    git::ensure_available().map_err(|e| AppError::Git(e.to_string()))?;

    let display_url = config::sanitize_url_for_display(clone_url);
    let dest =
        config::library_cache_path(&display_url).map_err(|e| AppError::Config(e.to_string()))?;
    let dest_display = fs_util::display_path(&dest);

    if dest.exists() {
        fs::remove_dir_all(&dest)
            .with_context(|| format!("removing existing cache at {dest_display}"))?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating cache dir at {}", fs_util::display_path(parent)))?;
    }

    ui::log_info(
        ctx,
        format!("cloning {display_url} into {dest_display} ..."),
    )?;
    git::clone(clone_url, &dest).map_err(|e| AppError::Git(e.to_string()))?;

    Ok((display_url, dest))
}

fn add(args: LibraryAddArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl library add")?;

    // Validate the name and reject a duplicate before any network work, so a
    // typo fails fast instead of after a clone.
    validate_identifier("library name", &args.name)?;
    // `all` is reserved — `--from all` means "span every library", so a
    // library literally named `all` could never be addressed by name.
    if args.name == "all" {
        return Err(AppError::Conflict(
            "`all` is a reserved library name (used by `--from all`); choose another name".into(),
        )
        .into());
    }
    let mut cfg = config::load()?;
    if cfg.by_name(&args.name).is_some() {
        return Err(
            AppError::Conflict(format!("a library named `{}` already exists", args.name)).into(),
        );
    }
    // Reject a second library pointing at the same repository before cloning.
    // Two libraries sharing one normalized URL would share a cache and let the
    // access gate be decided by config order rather than intent (see
    // `Config::validate`, which is the load/save-time backstop).
    if let Ok(want) = crate::host::parse_remote_url(&args.url) {
        if let Some(existing) = cfg.libraries.iter().find(|l| {
            crate::host::parse_remote_url(&l.url)
                .map(|have| have.normalized == want.normalized)
                .unwrap_or(false)
        }) {
            return Err(AppError::Conflict(format!(
                "library `{}` already points at that repository (`{}`); configure each repository at most once",
                existing.name, want.normalized
            ))
            .into());
        }
    }

    let access: Access = args.access.into();
    let make_default = args.default || cfg.libraries.is_empty();

    let (display_url, _cache) = clone_into_cache(ctx, &args.url)?;

    cfg.add_library(
        Library {
            name: args.name.clone(),
            url: display_url.clone(),
            access,
            default: false,
        },
        make_default,
    )?;
    config::save(&cfg).map_err(|e| AppError::Config(e.to_string()))?;

    let default_note = if make_default { " (default)" } else { "" };
    ui::log_success(
        ctx,
        format!(
            "library `{}` added [{}]{default_note} → {display_url}",
            args.name,
            access.as_str()
        ),
    )?;

    if ctx.json {
        let out = json!({
            "command": "library add",
            "library": {
                "name": args.name,
                "url": display_url,
                "access": access.as_str(),
                "default": make_default,
            }
        });
        println!("{out}");
    } else {
        ui::outro(ctx, "done")?;
    }
    Ok(())
}

fn list(ctx: &Context) -> Result<()> {
    let cfg = config::load()?;

    if ctx.json {
        let libraries: Vec<_> = cfg
            .libraries
            .iter()
            .map(|l| {
                json!({
                    "name": l.name,
                    "url": l.url,
                    "access": l.access.as_str(),
                    "default": l.default,
                })
            })
            .collect();
        println!(
            "{}",
            json!({ "command": "library list", "libraries": libraries })
        );
        return Ok(());
    }

    if cfg.libraries.is_empty() {
        println!(
            "no libraries configured — run `skillctl init <url>` or `skillctl library add <name> <url>`"
        );
        return Ok(());
    }

    for l in &cfg.libraries {
        let marker = if l.default { " (default)" } else { "" };
        println!("  {} [{}]{marker} — {}", l.name, l.access.as_str(), l.url);
    }
    Ok(())
}

fn remove(args: LibraryRefArgs, ctx: &Context) -> Result<()> {
    let mut cfg = config::load()?;
    let removed = cfg.remove_library(&args.name)?;
    config::save(&cfg).map_err(|e| AppError::Config(e.to_string()))?;

    if ctx.json {
        println!(
            "{}",
            json!({
                "command": "library remove",
                "removed": { "name": removed.name, "url": removed.url },
            })
        );
    } else {
        println!("removed library `{}` ({})", removed.name, removed.url);
        println!("note: its local cache was left in place (harmless; re-created on demand)");
    }
    Ok(())
}

fn set_default(args: LibraryRefArgs, ctx: &Context) -> Result<()> {
    let mut cfg = config::load()?;
    cfg.set_default(&args.name)?;
    config::save(&cfg).map_err(|e| AppError::Config(e.to_string()))?;

    if ctx.json {
        println!(
            "{}",
            json!({ "command": "library set-default", "default": args.name })
        );
    } else {
        println!("default library is now `{}`", args.name);
    }
    Ok(())
}

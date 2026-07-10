use std::fs;

use anyhow::{Context as _, Result};
use serde_json::json;

use crate::cli::UsageArgs;
use crate::commands::shared::resolve_dest_root;
use crate::context::Context;
use crate::error::AppError;
use crate::{fs_util, lock, ui};

/// Folder + `name:` of the bundled skill.
const SKILL_NAME: &str = "skillctl-usage";
/// The agent-facing reference, embedded at build time so the installed skill
/// always matches this binary's version (and a fresh `usage` run after an
/// upgrade refreshes it). Cargo re-runs the build when the file changes.
const SKILL_MD: &str = include_str!("../../.claude/skills/skillctl-usage/SKILL.md");

pub fn run(args: UsageArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl usage")?;

    let cwd = std::env::current_dir().context("reading current directory")?;
    // Same destination UX as `add`/`create` — keeps the tool harness-agnostic:
    // interactive picks from detected `skills/` folders + presets (claude /
    // codex / cursor / agents) or a custom path, rather than hardwiring
    // `.claude/skills`; non-interactive requires `--dest`.
    let dest_root = resolve_dest_root(args.dest.as_deref(), ctx, &cwd, "Install destination")?;

    // Take the project lock only after any interactive prompt, then close the
    // exists-check → write TOCTOU. `dest_root` carries no `..`
    // (`resolve_dest_root` guarantees it) and `SKILL_NAME` is a single
    // component, so the join stays inside the chosen destination (an absolute
    // interactive custom path is honoured, exactly like `create`).
    let _lock = lock::acquire_exclusive(&cwd, "project")?;
    let skill_dir = cwd.join(&dest_root).join(SKILL_NAME);
    let skill_md = skill_dir.join("SKILL.md");

    // Never write through a symlink: a link at the skill folder or its SKILL.md
    // would redirect the write outside the project (same stance as the copy /
    // replace / remove paths).
    for p in [&skill_dir, &skill_md] {
        if let Ok(meta) = fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() {
                return Err(AppError::Conflict(format!(
                    "refusing to write `{}`: it is a symlink",
                    fs_util::display_path(p)
                ))
                .into());
            }
        }
    }

    let existing = fs::read_to_string(&skill_md).ok();
    let status = install_status(existing.as_deref());

    if status != "up-to-date" {
        fs::create_dir_all(&skill_dir)
            .with_context(|| format!("creating {}", fs_util::display_path(&skill_dir)))?;
        fs::write(&skill_md, SKILL_MD)
            .with_context(|| format!("writing {}", fs_util::display_path(&skill_md)))?;
    }

    let version = env!("CARGO_PKG_VERSION");
    let dest_display = fs_util::relative_to_or_self(&skill_md, &cwd)
        .display()
        .to_string();
    let summary = match status {
        "installed" => format!("installed {SKILL_NAME} v{version} → {dest_display}"),
        "updated" => format!("updated {SKILL_NAME} to v{version} → {dest_display}"),
        _ => format!("{SKILL_NAME} already up to date (v{version})"),
    };
    ui::log_success(ctx, summary)?;
    ui::outro(ctx, "done")?;

    if ctx.json {
        let out = json!({
            "command": "usage",
            "skill": SKILL_NAME,
            "version": version,
            "path": dest_display,
            "status": status,
        });
        println!("{out}");
    }
    Ok(())
}

/// Decide the write outcome from the current on-disk content: `installed` when
/// absent, `up-to-date` when byte-identical to the bundled copy, `updated`
/// otherwise. Pure so the status logic is unit-tested without touching disk.
fn install_status(existing: Option<&str>) -> &'static str {
    match existing {
        None => "installed",
        Some(current) if current == SKILL_MD => "up-to-date",
        Some(_) => "updated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn bundled_skill_md_is_non_empty() {
        assert!(SKILL_MD.contains("skillctl"), "embedded doc looks wrong");
    }

    #[test]
    fn install_status_transitions() {
        assert_eq!(install_status(None), "installed");
        assert_eq!(install_status(Some(SKILL_MD)), "up-to-date");
        assert_eq!(install_status(Some("something else")), "updated");
    }

    #[test]
    fn bundled_doc_round_trips_as_a_skill_named_skillctl_usage() {
        // The strongest guarantee: what we write is a valid skill that
        // skillctl's own discovery reads back under the expected name.
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join(SKILL_NAME);
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), SKILL_MD).unwrap();
        let found = skill::discover(dir.path(), false).unwrap();
        assert!(
            found.skills.iter().any(|s| s.name == SKILL_NAME),
            "discovery did not surface `{SKILL_NAME}`"
        );
    }
}

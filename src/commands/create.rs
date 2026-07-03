use std::fs;

use anyhow::{Context as _, Result};
use serde_json::json;

use crate::cli::CreateArgs;
use crate::commands::shared::resolve_dest_root;
use crate::commands::tag::validate_tag;
use crate::context::Context;
use crate::error::AppError;
use crate::sanitize::{validate_fork_name, validate_identifier};
use crate::{fs_util, lock, ui};

/// Written into the frontmatter when `--description` is omitted — a nudge to
/// fill in the field agents use to decide when to load the skill.
const DEFAULT_DESCRIPTION: &str =
    "TODO: one-line summary of what this skill does and when an agent should use it.";

pub fn run(args: CreateArgs, ctx: &Context) -> Result<()> {
    ui::intro(ctx, "skillctl create")?;

    let name = args.name.trim();
    validate_fork_name(name)
        .map_err(|e| AppError::Config(format!("invalid skill name `{name}`: {e}")))?;
    for t in &args.tags {
        validate_tag(t)?;
    }
    let description = match args.description.as_deref() {
        // Single-line: an inline `description:` value can't span lines, and
        // rejecting control chars keeps the frontmatter free of ANSI/CRLF tricks.
        Some(d) => {
            validate_identifier("--description", d)?;
            d.to_string()
        }
        None => DEFAULT_DESCRIPTION.to_string(),
    };

    let cwd = std::env::current_dir().context("reading current directory")?;
    let dest_root = resolve_dest_root(args.dest.as_deref(), ctx, &cwd, "New skill location")?;

    // Serialise against concurrent project mutations, and close the TOCTOU
    // between the exists-check and the create below (two racing `create`s of the
    // same name would otherwise both pass the check).
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    // `dest_root` (no `..`) and `name` (a single path component) are both
    // traversal-validated, so this join stays inside the project; `cwd.join`
    // absorbs an absolute `dest_root` (an interactive custom path).
    let skill_dir = cwd.join(&dest_root).join(name);
    if skill_dir.exists() {
        return Err(AppError::Conflict(format!(
            "{} already exists — refusing to overwrite; pick another name or remove it first",
            skill_dir.display()
        ))
        .into());
    }

    fs::create_dir_all(&skill_dir).with_context(|| format!("creating {}", skill_dir.display()))?;
    let skill_md = skill_dir.join("SKILL.md");
    fs::write(&skill_md, render_skill_md(name, &description, &args.tags))
        .with_context(|| format!("writing {}", skill_md.display()))?;

    let shown = fs_util::relative_to_or_self(&skill_dir, &cwd);
    ui::log_success(ctx, format!("created {}/SKILL.md", shown.display()))?;
    ui::outro(ctx, format!("new skill `{name}` scaffolded"))?;
    emit_json(ctx, name, &shown);
    Ok(())
}

/// Render a template `SKILL.md`: frontmatter (name, description, optional inline
/// `tags:` array) the tolerant parser accepts, plus a body skeleton.
fn render_skill_md(name: &str, description: &str, tags: &[String]) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {name}\n"));
    out.push_str(&format!("description: {description}\n"));
    if !tags.is_empty() {
        out.push_str(&format!("tags: [{}]\n", tags.join(", ")));
    }
    out.push_str("---\n\n");
    out.push_str(&format!("# {name}\n\n{description}\n\n"));
    out.push_str(
        "## Instructions\n\n<!-- Describe what this skill does and the steps to follow. -->\n\n",
    );
    out.push_str("## Examples\n\n<!-- Optional: show example usage. -->\n");
    out
}

fn emit_json(ctx: &Context, name: &str, path: &std::path::Path) {
    if !ctx.json {
        return;
    }
    let out = json!({
        "command": "create",
        "name": name,
        "path": path.display().to_string(),
        "created": true,
    });
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill;
    use tempfile::TempDir;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// The strongest test: whatever `create` writes must parse back cleanly
    /// through skillctl's own discovery — a scaffold that `detect`/`add` can't
    /// read would be worse than useless.
    fn parse_one(md: &str) -> skill::Skill {
        let dir = TempDir::new().unwrap();
        let sdir = dir.path().join("some-skill");
        fs::create_dir(&sdir).unwrap();
        fs::write(sdir.join("SKILL.md"), md).unwrap();
        let mut d = skill::discover(dir.path(), false).unwrap();
        assert_eq!(d.skills.len(), 1, "expected exactly one skill");
        d.skills.pop().unwrap()
    }

    #[test]
    fn template_round_trips_through_the_parser() {
        let md = render_skill_md("my-skill", "Does a thing.", &s(&["video", "tools"]));
        let sk = parse_one(&md);
        assert_eq!(sk.name, "my-skill");
        assert_eq!(sk.description.as_deref(), Some("Does a thing."));
        assert_eq!(sk.tags, s(&["video", "tools"]));
    }

    #[test]
    fn template_without_tags_omits_the_line_and_still_parses() {
        let md = render_skill_md("x", "desc", &[]);
        assert!(!md.contains("tags:"), "no tags line when none supplied");
        assert_eq!(parse_one(&md).tags, Vec::<String>::new());
    }

    #[test]
    fn template_has_frontmatter_fences_and_body() {
        let md = render_skill_md("foo", "bar", &[]);
        assert!(md.starts_with("---\nname: foo\ndescription: bar\n---\n"));
        assert!(md.contains("# foo"));
        assert!(md.contains("## Instructions"));
    }
}

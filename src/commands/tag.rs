use anyhow::{Context as _, Result};
use serde_json::json;

use crate::cli::TagCommand;
use crate::context::Context;
use crate::error::AppError;
use crate::lock;
use crate::sanitize::validate_identifier;
use crate::skill::{self, Skill};
use crate::ui;

pub fn run(cmd: TagCommand, ctx: &Context) -> Result<()> {
    let (adding, args) = match &cmd {
        TagCommand::Add(a) => (true, a),
        TagCommand::Remove(a) => (false, a),
    };
    ui::intro(
        ctx,
        if adding {
            "skillctl tag add"
        } else {
            "skillctl tag remove"
        },
    )?;

    for t in &args.tags {
        validate_tag(t)?;
    }

    let cwd = std::env::current_dir().context("reading current directory")?;
    // Editing a project file; serialise against concurrent project mutations.
    let _project_lock = lock::acquire_exclusive(&cwd, "project")?;

    let target = find_skill(&cwd, &args.skill)?;
    let skill_md = target.path.join("SKILL.md");

    // Propagate a real read failure (oversize / non-UTF-8) rather than masking
    // it as "no current tags" — for `tag remove` that would otherwise look like
    // a confusing no-op instead of surfacing the problem.
    let current = skill::read_tags(&skill_md)?;
    let mut updated = current.clone();
    if adding {
        for t in &args.tags {
            if !updated.iter().any(|e| e == t) {
                updated.push(t.clone());
            }
        }
    } else {
        updated.retain(|e| !args.tags.iter().any(|t| t == e));
    }

    if updated == current {
        ui::outro(ctx, format!("{} — tags unchanged", args.skill))?;
        emit_json(ctx, &args.skill, &updated, false);
        return Ok(());
    }

    skill::set_tags(&skill_md, &updated)?;
    let shown = if updated.is_empty() {
        "(none)".to_string()
    } else {
        updated.join(", ")
    };
    ui::log_success(ctx, format!("{} → tags: [{shown}]", args.skill))?;
    ui::outro(ctx, format!("updated tags on {}", args.skill))?;
    emit_json(ctx, &args.skill, &updated, true);
    Ok(())
}

/// Find exactly one project skill by name; error on none or ambiguity (two
/// skills sharing a name), mirroring `remove`'s fail-closed selection.
fn find_skill(cwd: &std::path::Path, name: &str) -> Result<Skill> {
    let discovered = skill::discover(cwd, false)?;
    let mut matches = discovered.skills.into_iter().filter(|s| s.name == name);
    let first = matches.next().ok_or_else(|| {
        AppError::Config(format!("no skill named `{name}` found in this project"))
    })?;
    if matches.next().is_some() {
        return Err(AppError::Conflict(format!(
            "more than one skill named `{name}` found in this project; rename one or run from a narrower directory"
        ))
        .into());
    }
    Ok(first)
}

/// A tag must be a single-line token with no structural characters, so it
/// round-trips cleanly through the inline `tags: [..]` frontmatter form.
pub(crate) fn validate_tag(tag: &str) -> Result<()> {
    validate_identifier("tag", tag)?;
    if tag.trim().is_empty() {
        return Err(AppError::Config("a tag cannot be empty".into()).into());
    }
    if tag
        .chars()
        .any(|c| matches!(c, ',' | '[' | ']' | '"' | '\''))
    {
        return Err(AppError::Config(format!(
            "invalid tag `{tag}`: tags cannot contain `,` `[` `]` `\"` or `'`"
        ))
        .into());
    }
    // Reject exotic whitespace (U+2028/U+2029 line/paragraph separators, NBSP,
    // …) that `validate_identifier` lets through: a third-party YAML reader
    // might treat some of these as line breaks even though skillctl doesn't.
    // A plain ASCII space is fine (it round-trips quoted).
    if tag.chars().any(|c| c != ' ' && c.is_whitespace()) {
        return Err(AppError::Config(format!(
            "invalid tag `{tag}`: tags cannot contain non-space whitespace"
        ))
        .into());
    }
    Ok(())
}

fn emit_json(ctx: &Context, skill: &str, tags: &[String], changed: bool) {
    if !ctx.json {
        return;
    }
    let out = json!({
        "command": "tag",
        "skill": skill,
        "tags": tags,
        "changed": changed,
    });
    println!("{out}");
}

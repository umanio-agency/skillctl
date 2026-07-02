use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::audit::{self, Severity};
use crate::context::Context;
use crate::error::AppError;
use crate::ui;

/// Scan each `(name, folder)` pair's content and surface a verdict. Shared by
/// every command that moves untrusted content across the library/project
/// boundary (`add`, `pull`, `detect`). Findings are logged as warnings (a no-op
/// under `--json`); when `fail_on` is set, any item reaching that severity
/// blocks the **whole batch** (returns `AppError::Audit` → exit 5), so nothing
/// is applied on a failed bar. Returns a map of name → verdict so callers can
/// surface it per-item in their JSON output — the audit signal a non-interactive
/// consumer would otherwise miss in warn-only mode.
pub fn audit_gate<'a, I>(
    ctx: &Context,
    items: I,
    fail_on: Option<Severity>,
) -> Result<HashMap<String, &'static str>>
where
    I: IntoIterator<Item = (&'a str, &'a Path)>,
{
    let mut blocked: Vec<String> = Vec::new();
    let mut verdicts: HashMap<String, &'static str> = HashMap::new();
    for (name, path) in items {
        let report = audit::scan_skill(path);
        verdicts.insert(name.to_string(), report.verdict().as_str());
        for f in &report.findings {
            ui::log_warning(
                ctx,
                format!(
                    "audit[{}] {}: {} ({}:{})",
                    name,
                    f.severity.as_str(),
                    f.label,
                    f.file,
                    f.line
                ),
            )?;
        }
        if fail_on.is_some_and(|t| report.max_severity().is_some_and(|m| m >= t)) {
            blocked.push(format!("{} ({})", name, report.verdict().as_str()));
        }
    }
    if !blocked.is_empty() {
        // `blocked` is only populated when `fail_on` is `Some`.
        let threshold = fail_on.map(|t| t.as_str()).unwrap_or("warning");
        return Err(AppError::Audit(format!(
            "refusing to proceed (content audit ≥ `{threshold}`): {}",
            blocked.join(", ")
        ))
        .into());
    }
    Ok(verdicts)
}

/// True iff the skill's tags satisfy a `--tag` filter. An empty `filter` is
/// treated as "no filter" and matches everything. With `all_tags = false`
/// (default) the skill needs at least one of the filter tags; with
/// `all_tags = true` it needs all of them.
pub fn matches_tags(skill_tags: &[String], filter: &[String], all_tags: bool) -> bool {
    if filter.is_empty() {
        return true;
    }
    if all_tags {
        filter.iter().all(|t| skill_tags.contains(t))
    } else {
        filter.iter().any(|t| skill_tags.contains(t))
    }
}

/// Compact a possibly-long description into a single hint line:
/// strip newlines/runs of whitespace, cut at the first sentence end (`. `)
/// when reasonable, otherwise cap at ~100 chars with an ellipsis.
pub fn short_hint(desc: &str) -> String {
    let normalized = desc.split_whitespace().collect::<Vec<_>>().join(" ");
    const CAP: usize = 100;
    if let Some(period) = normalized.find('.')
        && period <= CAP
    {
        return normalized[..=period].to_string();
    }
    if normalized.chars().count() <= CAP {
        return normalized;
    }
    let truncated: String = normalized.chars().take(CAP).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_descriptions() {
        assert_eq!(short_hint("simple"), "simple");
    }

    #[test]
    fn cuts_at_first_sentence() {
        assert_eq!(
            short_hint("First sentence. Second sentence."),
            "First sentence."
        );
    }

    #[test]
    fn truncates_when_no_period() {
        let desc = "a".repeat(200);
        let out = short_hint(&desc);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 101);
    }

    #[test]
    fn normalizes_whitespace() {
        assert_eq!(short_hint("  multi\n line   spaces"), "multi line spaces");
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matches_tags_empty_filter_matches_all() {
        assert!(matches_tags(&s(&["x"]), &[], false));
        assert!(matches_tags(&[], &[], false));
    }

    #[test]
    fn matches_tags_union_matches_when_any_present() {
        assert!(matches_tags(&s(&["a", "b"]), &s(&["b", "z"]), false));
    }

    #[test]
    fn matches_tags_union_misses_when_none_present() {
        assert!(!matches_tags(&s(&["a", "b"]), &s(&["x", "y"]), false));
    }

    #[test]
    fn matches_tags_intersection_requires_all() {
        assert!(matches_tags(&s(&["a", "b", "c"]), &s(&["a", "b"]), true));
        assert!(!matches_tags(&s(&["a"]), &s(&["a", "b"]), true));
    }
}

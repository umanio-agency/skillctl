use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use cliclack::select;

use crate::audit::{self, Finding, Severity};
use crate::context::Context;
use crate::error::AppError;
use crate::ui;

/// Outcome of the pre-apply content audit for a batch of skills.
pub enum AuditGate {
    /// Proceed, acting only on `approved`: every input name on the
    /// non-interactive / no-danger paths, or a user-chosen subset after the
    /// interactive triage. `verdicts` maps name → verdict for JSON output.
    Proceed {
        approved: HashSet<String>,
        verdicts: HashMap<String, &'static str>,
    },
    /// The interactive user chose to cancel the whole run — the caller should
    /// bail gracefully (nothing applied), not treat it as an error.
    Cancelled,
}

/// One skill's audit result, retained so the interactive triage can re-show a
/// skill's findings on demand.
struct Scan {
    name: String,
    verdict: &'static str,
    max_severity: Option<Severity>,
    findings: Vec<Finding>,
}

/// A verdict at or above this severity is "potentially dangerous" and, in an
/// interactive warn-only run, triggers the batch-triage menu.
const TRIAGE_SEVERITY: Severity = Severity::Warning;

/// Content-audit gate shared by every command that moves skill content across
/// the library/project boundary (`add`, `pull`, `detect`).
///
/// - `no_audit` short-circuits: approve every input, no scan.
/// - Otherwise each skill is scanned, findings are logged as warnings (a no-op
///   under `--json`), and a verdict map is built for the caller's JSON output.
/// - `fail_on` is the non-interactive bar: any skill reaching that severity
///   refuses the **whole batch** (`AppError::Audit` → exit 5), so nothing is
///   applied on a failed bar. When `--fail-on` is set it is the sole policy —
///   the interactive triage is suppressed.
/// - Otherwise, in an interactive TTY, if any skill is flagged
///   (verdict ≥ [`TRIAGE_SEVERITY`]) the user is offered a batch-triage menu
///   (decide per skill / proceed with all / cancel). Non-flagged skills always
///   proceed. The returned `approved` set is what the caller must act on.
pub fn audit_gate<'a, I>(
    ctx: &Context,
    items: I,
    no_audit: bool,
    fail_on: Option<Severity>,
) -> Result<AuditGate>
where
    I: IntoIterator<Item = (&'a str, &'a Path)>,
{
    if no_audit {
        let approved = items.into_iter().map(|(n, _)| n.to_string()).collect();
        return Ok(AuditGate::Proceed {
            approved,
            verdicts: HashMap::new(),
        });
    }

    let mut scans: Vec<Scan> = Vec::new();
    let mut verdicts: HashMap<String, &'static str> = HashMap::new();
    for (name, path) in items {
        let report = audit::scan_skill(path);
        let verdict = report.verdict().as_str();
        verdicts.insert(name.to_string(), verdict);
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
        scans.push(Scan {
            name: name.to_string(),
            verdict,
            max_severity: report.max_severity(),
            findings: report.findings,
        });
    }

    // Non-interactive bar: `--fail-on` refuses the whole batch (no triage).
    if let Some(t) = fail_on {
        let blocked: Vec<String> = scans
            .iter()
            .filter(|s| s.max_severity.is_some_and(|m| m >= t))
            .map(|s| format!("{} ({})", s.name, s.verdict))
            .collect();
        if !blocked.is_empty() {
            return Err(AppError::Audit(format!(
                "refusing to proceed (content audit ≥ `{}`): {}",
                t.as_str(),
                blocked.join(", ")
            ))
            .into());
        }
    }

    // Interactive batch-triage: only in a TTY, only in warn-only mode
    // (`--fail-on` already expressed the policy), and only when something is
    // actually flagged.
    let flagged = scans
        .iter()
        .filter(|s| s.max_severity.is_some_and(|m| m >= TRIAGE_SEVERITY))
        .count();
    if !ctx.interactive || fail_on.is_some() || flagged == 0 {
        let approved = scans.into_iter().map(|s| s.name).collect();
        return Ok(AuditGate::Proceed { approved, verdicts });
    }

    match triage(ctx, &scans, flagged)? {
        Some(approved) => Ok(AuditGate::Proceed { approved, verdicts }),
        None => Ok(AuditGate::Cancelled),
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum TopChoice {
    Review,
    Cancel,
    All,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum SkillChoice {
    Include,
    Skip,
    View,
}

/// The interactive triage. Returns `Some(approved names)` to proceed, or `None`
/// if the user cancelled the whole run. Non-flagged skills are always approved;
/// only the `flagged` ones are decided (all at once, or one by one).
fn triage(ctx: &Context, scans: &[Scan], flagged: usize) -> Result<Option<HashSet<String>>> {
    let top = select(format!(
        "⚠ {flagged} of the selected skill(s) look potentially dangerous — how do you want to proceed?"
    ))
    .item(
        TopChoice::Review,
        "Decide for each",
        "review the findings and choose include or skip per skill",
    )
    .item(
        TopChoice::Cancel,
        "Cancel everything",
        "abort the run — nothing is applied",
    )
    .item(
        TopChoice::All,
        "Proceed with all",
        "apply every skill despite the audit warnings",
    )
    .interact()?;

    match top {
        TopChoice::Cancel => Ok(None),
        TopChoice::All => Ok(Some(scans.iter().map(|s| s.name.clone()).collect())),
        TopChoice::Review => {
            let is_flagged = |s: &Scan| s.max_severity.is_some_and(|m| m >= TRIAGE_SEVERITY);
            // Non-flagged skills carry no risk signal → always approved.
            let mut approved: HashSet<String> = scans
                .iter()
                .filter(|s| !is_flagged(s))
                .map(|s| s.name.clone())
                .collect();
            for s in scans.iter().filter(|s| is_flagged(s)) {
                loop {
                    let choice = select(format!(
                        "`{}` — audit verdict: {} — include it?",
                        s.name, s.verdict
                    ))
                    .item(
                        SkillChoice::Include,
                        "Include",
                        "accept the risk for this skill",
                    )
                    .item(SkillChoice::Skip, "Skip", "leave this skill out")
                    .item(
                        SkillChoice::View,
                        "View findings",
                        "list what the audit flagged",
                    )
                    .interact()?;
                    match choice {
                        SkillChoice::Include => {
                            approved.insert(s.name.clone());
                            break;
                        }
                        SkillChoice::Skip => {
                            ui::log_info(ctx, format!("skipping {}", s.name))?;
                            break;
                        }
                        SkillChoice::View => {
                            for f in &s.findings {
                                ui::log_warning(
                                    ctx,
                                    format!(
                                        "  {} {}: {} ({}:{}) — {}",
                                        f.severity.as_str(),
                                        f.category,
                                        f.label,
                                        f.file,
                                        f.line,
                                        f.snippet
                                    ),
                                )?;
                            }
                        }
                    }
                }
            }
            Ok(Some(approved))
        }
    }
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

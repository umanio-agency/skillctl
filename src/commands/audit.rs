use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::audit::{self, Severity};
use crate::cli::{AuditArgs, SeverityArg};
use crate::context::Context;
use crate::error::AppError;
use crate::{skill, ui};

impl From<SeverityArg> for Severity {
    fn from(s: SeverityArg) -> Self {
        match s {
            SeverityArg::Info => Severity::Info,
            SeverityArg::Warning => Severity::Warning,
            SeverityArg::Critical => Severity::Critical,
        }
    }
}

pub fn run(args: AuditArgs, ctx: &Context) -> Result<()> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let discovered = skill::discover(&cwd, false)?;
    for w in &discovered.warnings {
        ui::log_warning(ctx, w)?;
    }
    let mut skills = discovered.skills;

    if !args.skills.is_empty() {
        for name in &args.skills {
            if !skills.iter().any(|s| &s.name == name) {
                return Err(AppError::Config(format!(
                    "no skill named `{name}` found in this project"
                ))
                .into());
            }
        }
        skills.retain(|s| args.skills.contains(&s.name));
    }

    let threshold: Option<Severity> = args.fail_on.map(Into::into);
    let mut worst: Option<Severity> = None;
    let mut json_skills: Vec<Value> = Vec::new();

    if skills.is_empty() {
        if ctx.json {
            println!(
                "{}",
                json!({ "command": "audit", "skills": [], "summary": { "scanned": 0 } })
            );
        } else {
            println!("no skills found in this project");
        }
        return Ok(());
    }

    for s in &skills {
        let report = audit::scan_skill(&s.path);
        worst = worst.max(report.max_severity());
        let verdict = report.verdict();

        if ctx.json {
            let findings: Vec<Value> = report
                .findings
                .iter()
                .map(|f| {
                    json!({
                        "severity": f.severity.as_str(),
                        "category": f.category,
                        "label": f.label,
                        "file": f.file,
                        "line": f.line,
                        "snippet": f.snippet,
                    })
                })
                .collect();
            json_skills.push(json!({
                "name": s.name,
                "verdict": verdict.as_str(),
                "findings": findings,
            }));
        } else {
            println!("{}: {}", s.name, verdict.as_str());
            for f in &report.findings {
                println!(
                    "  [{}] {}: {} ({}:{})",
                    f.severity.as_str(),
                    f.category,
                    f.label,
                    f.file,
                    f.line
                );
            }
        }
    }

    let worst_str = worst.map(|s| s.as_str()).unwrap_or("none");
    if ctx.json {
        println!(
            "{}",
            json!({
                "command": "audit",
                "skills": json_skills,
                "summary": { "scanned": skills.len(), "worst_severity": worst_str },
            })
        );
    } else {
        println!(
            "\n{} skill(s) scanned; worst severity: {worst_str}",
            skills.len()
        );
    }

    if let Some(threshold) = threshold {
        if worst.is_some_and(|w| w >= threshold) {
            return Err(AppError::Audit(format!(
                "content audit found at least one `{}`-or-higher finding (threshold: `{}`)",
                worst_str,
                threshold.as_str()
            ))
            .into());
        }
    }
    Ok(())
}

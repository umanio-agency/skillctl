use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Skill {
    pub path: PathBuf,
    pub name: String,
    pub description: Option<String>,
}

pub fn discover(root: &Path) -> Result<Vec<Skill>> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| {
            let name = e.file_name();
            name != "node_modules" && name != "target"
        })
        .build();
    let mut skills = Vec::new();
    for entry in walker {
        let entry = entry.context("walking the library tree")?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        if entry.file_name() != "SKILL.md" {
            continue;
        }
        let skill_md = entry.path();
        let folder = match skill_md.parent() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let folder_name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("(unknown)")
            .to_string();
        let raw = std::fs::read_to_string(skill_md)
            .with_context(|| format!("reading {}", skill_md.display()))?;
        let (name, description) = parse_frontmatter(&raw);
        skills.push(Skill {
            path: folder,
            name: name.unwrap_or(folder_name),
            description,
        });
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

/// Recursively find every directory literally named `skills` under `root`.
/// Honours `.gitignore`, plus skips `node_modules` and `target` outright.
/// Hidden directories are walked so that conventions like `.claude/skills/`
/// are reachable. Returns the paths walker-style (relative to `root` if
/// `root` is `.`, absolute otherwise) — callers normalise as needed.
pub fn find_skills_folders(root: &Path) -> Result<Vec<PathBuf>> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| {
            let name = e.file_name();
            name != "node_modules" && name != "target"
        })
        .build();
    let mut found = Vec::new();
    for entry in walker {
        let entry = entry.context("walking the directory tree")?;
        let is_dir = entry.file_type().is_some_and(|ft| ft.is_dir());
        if is_dir && entry.file_name() == "skills" {
            found.push(entry.path().to_path_buf());
        }
    }
    found.sort();
    Ok(found)
}

/// Extract `name` and `description` from a leading YAML-style frontmatter block.
/// Tolerant by design — only single-line values are parsed, and any other key is ignored.
fn parse_frontmatter(raw: &str) -> (Option<String>, Option<String>) {
    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None);
    }
    let mut name = None;
    let mut description = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(clean_value(rest));
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = Some(clean_value(rest));
        }
    }
    (name, description)
}

fn clean_value(raw: &str) -> String {
    raw.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let raw = "---\nname: foo\ndescription: a tiny skill\n---\n# body\n";
        let (name, desc) = parse_frontmatter(raw);
        assert_eq!(name.as_deref(), Some("foo"));
        assert_eq!(desc.as_deref(), Some("a tiny skill"));
    }

    #[test]
    fn no_frontmatter_returns_none() {
        let (name, desc) = parse_frontmatter("# just a heading\n");
        assert!(name.is_none());
        assert!(desc.is_none());
    }

    #[test]
    fn strips_quotes() {
        let raw = "---\nname: \"quoted\"\n---\n";
        let (name, _) = parse_frontmatter(raw);
        assert_eq!(name.as_deref(), Some("quoted"));
    }
}

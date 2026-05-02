use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Skill {
    pub path: PathBuf,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
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
        let (name, description, tags) = parse_frontmatter(&raw);
        skills.push(Skill {
            path: folder,
            name: name.unwrap_or(folder_name),
            description,
            tags,
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

/// Extract `name`, `description`, and `tags` from a leading YAML-style
/// frontmatter block. Tolerant by design — only single-line values and the
/// inline array form `tags: [a, b, c]` are parsed; any other key is ignored.
fn parse_frontmatter(raw: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let mut lines = raw.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None, Vec::new());
    }
    let mut name = None;
    let mut description = None;
    let mut tags = Vec::new();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(clean_value(rest));
        } else if let Some(rest) = line.strip_prefix("description:") {
            description = Some(clean_value(rest));
        } else if let Some(rest) = line.strip_prefix("tags:") {
            tags = parse_tags_inline(rest);
        }
    }
    (name, description, tags)
}

fn clean_value(raw: &str) -> String {
    raw.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}

/// Parse the inline-array tag form: `[a, b, "c"]`. Empty input or `[]` returns
/// an empty vector. A bare scalar `tags: foo` is accepted as `["foo"]`.
/// Block-style YAML is intentionally not supported in v1.
fn parse_tags_inline(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return inner
            .split(',')
            .map(clean_value)
            .filter(|s| !s.is_empty())
            .collect();
    }
    // Forgiving fallback: a single scalar value is treated as one tag.
    let single = clean_value(trimmed);
    if single.is_empty() {
        Vec::new()
    } else {
        vec![single]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter() {
        let raw = "---\nname: foo\ndescription: a tiny skill\n---\n# body\n";
        let (name, desc, tags) = parse_frontmatter(raw);
        assert_eq!(name.as_deref(), Some("foo"));
        assert_eq!(desc.as_deref(), Some("a tiny skill"));
        assert!(tags.is_empty());
    }

    #[test]
    fn no_frontmatter_returns_none() {
        let (name, desc, tags) = parse_frontmatter("# just a heading\n");
        assert!(name.is_none());
        assert!(desc.is_none());
        assert!(tags.is_empty());
    }

    #[test]
    fn strips_quotes() {
        let raw = "---\nname: \"quoted\"\n---\n";
        let (name, _, _) = parse_frontmatter(raw);
        assert_eq!(name.as_deref(), Some("quoted"));
    }

    #[test]
    fn parses_inline_tag_array() {
        let raw = "---\nname: foo\ntags: [a, b, c]\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert_eq!(tags, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn parses_quoted_tag_array() {
        let raw = "---\ntags: [\"hello world\", 'foo']\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert_eq!(tags, vec!["hello world".to_string(), "foo".to_string()]);
    }

    #[test]
    fn parses_empty_tag_array() {
        let raw = "---\ntags: []\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert!(tags.is_empty());
    }

    #[test]
    fn missing_tags_field_is_empty() {
        let raw = "---\nname: foo\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert!(tags.is_empty());
    }

    #[test]
    fn scalar_tag_value_becomes_singleton() {
        let raw = "---\ntags: solo\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert_eq!(tags, vec!["solo".to_string()]);
    }
}

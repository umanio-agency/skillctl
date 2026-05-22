use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::sanitize::{validate_identifier, validate_message_safe};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Skill {
    pub path: PathBuf,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
}

/// Walk `root` for skills (folders containing a `SKILL.md`).
///
/// `include_vendored=false` (the safe default for project-side discovery):
/// respects `.gitignore` / global ignore / `.ignore`, and additionally
/// hard-skips `node_modules` / `target` folders. A malicious npm package
/// that ships its own `SKILL.md` under `node_modules/...` cannot be
/// detected and uploaded to the library by an automated `skillctl detect`
/// (e.g. running in CI) unless the operator explicitly opts in.
///
/// `include_vendored=true`: walks every directory regardless of ignore
/// files. Use only when the operator has typed the flag themselves.
pub fn discover(root: &Path, include_vendored: bool) -> Result<Vec<Skill>> {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    if include_vendored {
        builder
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .ignore(false);
    } else {
        builder.filter_entry(|e| {
            let name = e.file_name();
            name != "node_modules" && name != "target"
        });
    }
    let walker = builder.build();
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
        let resolved_name = name.unwrap_or_else(|| folder_name.clone());
        // Sanitisation at the discovery boundary: a malicious library may
        // smuggle control bytes or ANSI escapes into `name`, `description`,
        // or `tags`, all of which flow into terminal logs, JSON output, and
        // commit messages. Skills with an invalid name are silently dropped
        // (we cannot safely display them either); descriptions and tags
        // that fail validation are stripped from the otherwise-valid skill.
        if validate_identifier("skill name", &resolved_name).is_err() {
            continue;
        }
        let description = description.filter(|d| validate_message_safe("description", d).is_ok());
        let tags: Vec<String> = tags
            .into_iter()
            .filter(|t| validate_identifier("tag", t).is_ok())
            .collect();
        skills.push(Skill {
            path: folder,
            name: resolved_name,
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

/// Maximum number of lines we will read inside a YAML frontmatter block
/// before assuming the closing `---` fence is missing or the file is
/// adversarial. Without this cap, a SKILL.md with an opening `---` but no
/// closer would force the parser to scan the entire (potentially multi-GiB)
/// body — a cheap DoS.
const MAX_FRONTMATTER_LINES: usize = 200;

/// Extract `name`, `description`, and `tags` from a leading YAML-style
/// frontmatter block. Tolerant by design — single-line values are parsed,
/// `description:` accepts both inline values and YAML block scalars (`|`
/// literal, `>` folded), `tags:` accepts both the inline array form
/// (`[a, b, c]`) and the block form (subsequent indented `- item` lines),
/// and any other key is ignored.
///
/// Returns `(None, None, vec![])` (i.e. "no frontmatter") if the opening
/// `---` is missing OR if the closing `---` is not seen within
/// [`MAX_FRONTMATTER_LINES`] lines.
fn parse_frontmatter(raw: &str) -> (Option<String>, Option<String>, Vec<String>) {
    let mut lines = raw.lines().take(MAX_FRONTMATTER_LINES + 1).peekable();
    if lines.next().map(str::trim) != Some("---") {
        return (None, None, Vec::new());
    }
    let mut name = None;
    let mut description = None;
    let mut tags = Vec::new();
    let mut saw_closing = false;
    while let Some(line) = lines.next() {
        if line.trim() == "---" {
            saw_closing = true;
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            name = Some(clean_value(rest));
        } else if let Some(rest) = line.strip_prefix("description:") {
            let trimmed = rest.trim();
            if trimmed == "|" || trimmed == ">" {
                let folded = trimmed == ">";
                let mut parts: Vec<String> = Vec::new();
                while let Some(peek) = lines.peek() {
                    if peek.trim() == "---" {
                        break;
                    }
                    if peek.starts_with(' ') || peek.starts_with('\t') {
                        parts.push(peek.trim().to_string());
                        lines.next();
                    } else if peek.trim().is_empty() {
                        parts.push(String::new());
                        lines.next();
                    } else {
                        break;
                    }
                }
                let joined = if folded {
                    parts
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    parts.join("\n").trim_end().to_string()
                };
                description = Some(joined);
            } else {
                description = Some(clean_value(trimmed));
            }
        } else if let Some(rest) = line.strip_prefix("tags:") {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                // Block form: peek and consume `- item` lines until something else shows up.
                while let Some(peek) = lines.peek() {
                    let pt = peek.trim_start();
                    if let Some(item) = pt.strip_prefix("- ") {
                        let v = clean_value(item);
                        if !v.is_empty() {
                            tags.push(v);
                        }
                        lines.next();
                    } else if pt == "-" {
                        // Empty list item — skip.
                        lines.next();
                    } else {
                        break;
                    }
                }
            } else {
                tags = parse_tags_inline(trimmed);
            }
        }
    }
    if !saw_closing {
        // Unterminated frontmatter: treat the file as having no frontmatter
        // at all rather than half-parsed metadata. Either the file is
        // malformed (operator edit) or adversarial (DoS via unbounded body).
        return (None, None, Vec::new());
    }
    (name, description, tags)
}

/// Read just the tags from a single SKILL.md. Returns an empty vector if the
/// file has no frontmatter or no `tags:` field.
pub fn read_tags(skill_md: &Path) -> Result<Vec<String>> {
    if !skill_md.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(skill_md)
        .with_context(|| format!("reading {}", skill_md.display()))?;
    let (_, _, tags) = parse_frontmatter(&raw);
    // Mirror the sanitisation in `discover`: filter out tags with control
    // bytes so a malicious local SKILL.md cannot inject ANSI/OSC-8 into
    // tag-matching errors or `--json` output.
    Ok(tags
        .into_iter()
        .filter(|t| validate_identifier("tag", t).is_ok())
        .collect())
}

fn clean_value(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string()
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
        assert_eq!(
            tags,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
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

    #[test]
    fn parses_block_tag_form() {
        let raw = "---\nname: foo\ntags:\n  - a\n  - b\n  - c\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert_eq!(
            tags,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn parses_block_tag_with_quotes() {
        let raw = "---\ntags:\n  - \"hello world\"\n  - 'foo'\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert_eq!(tags, vec!["hello world".to_string(), "foo".to_string()]);
    }

    #[test]
    fn block_tag_followed_by_more_keys() {
        let raw = "---\ntags:\n  - a\n  - b\nname: after\n---\n";
        let (name, _, tags) = parse_frontmatter(raw);
        assert_eq!(tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(name.as_deref(), Some("after"));
    }

    #[test]
    fn empty_block_tag_form() {
        let raw = "---\ntags:\nname: solo\n---\n";
        let (_, _, tags) = parse_frontmatter(raw);
        assert!(tags.is_empty());
    }

    #[test]
    fn parses_literal_multiline_description() {
        let raw = "---\nname: foo\ndescription: |\n  first line\n  second line\n---\n";
        let (_, desc, _) = parse_frontmatter(raw);
        assert_eq!(desc.as_deref(), Some("first line\nsecond line"));
    }

    #[test]
    fn parses_folded_multiline_description() {
        let raw = "---\ndescription: >\n  first line\n  second line\n---\n";
        let (_, desc, _) = parse_frontmatter(raw);
        assert_eq!(desc.as_deref(), Some("first line second line"));
    }

    #[test]
    fn multiline_description_followed_by_tags() {
        let raw = "---\ndescription: |\n  body line 1\n  body line 2\ntags: [x, y]\n---\n";
        let (_, desc, tags) = parse_frontmatter(raw);
        assert_eq!(desc.as_deref(), Some("body line 1\nbody line 2"));
        assert_eq!(tags, vec!["x".to_string(), "y".to_string()]);
    }

    use tempfile::TempDir;

    fn write_skill(root: &std::path::Path, folder: &str, frontmatter: &str) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), format!("{frontmatter}\n\n# body\n")).unwrap();
    }

    #[test]
    fn discover_drops_skills_with_control_chars_in_name() {
        let lib = TempDir::new().unwrap();
        write_skill(
            lib.path(),
            "ok-skill",
            "---\nname: clean\ndescription: fine\n---",
        );
        // Newline in name would forge commit trailers; ANSI escape would
        // hijack the terminal. Both must be dropped silently.
        write_skill(
            lib.path(),
            "evil-newline",
            "---\nname: \"foo\\nCo-Authored-By: evil\"\n---",
        );
        // Note: the parser strips wrapping quotes but leaves the literal `\n`
        // characters from the YAML source. To get a real LF in the parsed
        // name, the SKILL.md must contain a real LF — write that directly.
        let evil_dir = lib.path().join("evil-literal");
        std::fs::create_dir_all(&evil_dir).unwrap();
        std::fs::write(
            evil_dir.join("SKILL.md"),
            "---\nname: foo\u{1b}[31mBAD\u{1b}[0m\n---\n",
        )
        .unwrap();

        let skills = discover(lib.path(), false).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"clean"));
        assert!(
            !names.iter().any(|n| n.contains('\u{1b}')),
            "ANSI-poisoned skill must be dropped, got: {names:?}"
        );
    }

    #[test]
    fn discover_strips_bad_tags_keeps_skill() {
        let lib = TempDir::new().unwrap();
        let dir = lib.path().join("foo");
        std::fs::create_dir_all(&dir).unwrap();
        // Block-form tags: one clean, one with ESC.
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: foo\ntags:\n  - clean\n  - \"bad\u{1b}[31m\"\n---\n",
        )
        .unwrap();

        let skills = discover(lib.path(), false).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "foo");
        assert_eq!(skills[0].tags, vec!["clean".to_string()]);
    }

    #[test]
    fn discover_skips_node_modules_by_default() {
        let work = TempDir::new().unwrap();
        write_skill(work.path(), "ok-skill", "---\nname: ok\n---");
        let evil_dir = work.path().join("node_modules/evil-pkg");
        std::fs::create_dir_all(&evil_dir).unwrap();
        std::fs::write(evil_dir.join("SKILL.md"), "---\nname: evil\n---\n").unwrap();

        let skills = discover(work.path(), false).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ok"));
        assert!(
            !names.contains(&"evil"),
            "node_modules SKILL.md must not be detected without --include-vendored: {names:?}"
        );
    }

    #[test]
    fn discover_walks_node_modules_when_include_vendored() {
        let work = TempDir::new().unwrap();
        let evil_dir = work.path().join("node_modules/evil-pkg");
        std::fs::create_dir_all(&evil_dir).unwrap();
        std::fs::write(evil_dir.join("SKILL.md"), "---\nname: evil\n---\n").unwrap();

        let skills = discover(work.path(), true).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"evil"),
            "with --include-vendored, node_modules is walked: {names:?}"
        );
    }

    #[test]
    fn discover_respects_gitignore_by_default() {
        let work = TempDir::new().unwrap();
        std::fs::create_dir_all(work.path().join(".git")).unwrap();
        std::fs::write(work.path().join(".gitignore"), "vendor/\n").unwrap();
        write_skill(work.path(), "ok-skill", "---\nname: ok\n---");
        let vendor_skill = work.path().join("vendor/foo");
        std::fs::create_dir_all(&vendor_skill).unwrap();
        std::fs::write(vendor_skill.join("SKILL.md"), "---\nname: vendored\n---\n").unwrap();

        let skills = discover(work.path(), false).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"ok"));
        assert!(
            !names.contains(&"vendored"),
            ".gitignore must be respected by default: {names:?}"
        );
    }

    #[test]
    fn unterminated_frontmatter_is_treated_as_none() {
        // Opening `---` with no closing fence within MAX_FRONTMATTER_LINES.
        // Without the cap this would scan the entire body — here we build a
        // large enough body to exceed the cap.
        let mut raw = String::from("---\nname: foo\n");
        for i in 0..(MAX_FRONTMATTER_LINES + 50) {
            raw.push_str(&format!("filler-{i}\n"));
        }
        let (name, desc, tags) = parse_frontmatter(&raw);
        assert!(
            name.is_none(),
            "name from unterminated frontmatter must be dropped"
        );
        assert!(desc.is_none());
        assert!(tags.is_empty());
    }

    #[test]
    fn frontmatter_within_cap_parses_normally() {
        // 50 ignored keys, well under the 200-line cap.
        let mut raw = String::from("---\nname: foo\n");
        for i in 0..50 {
            raw.push_str(&format!("ignored_key_{i}: {i}\n"));
        }
        raw.push_str("---\n");
        let (name, _, _) = parse_frontmatter(&raw);
        assert_eq!(name.as_deref(), Some("foo"));
    }

    #[test]
    fn discover_strips_bad_description_keeps_skill() {
        let lib = TempDir::new().unwrap();
        let dir = lib.path().join("foo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: foo\ndescription: hello\u{1b}[31mEVIL\n---\n",
        )
        .unwrap();

        let skills = discover(lib.path(), false).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "foo");
        assert!(
            skills[0].description.is_none(),
            "ANSI-poisoned description should be stripped"
        );
    }
}

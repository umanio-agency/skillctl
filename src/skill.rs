use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::sanitize::{validate_identifier, validate_message_safe};

/// Hard cap on the bytes we will read from a SKILL.md. The parser only
/// looks at the frontmatter (already bounded at MAX_FRONTMATTER_LINES);
/// the rest of the body is prose for the agent, not for us. A 1 MiB cap
/// is generous for any legitimate SKILL.md and stops a 5 GiB file from
/// being silently slurped into memory during `discover`.
const MAX_SKILL_MD_BYTES: usize = 1 << 20; // 1 MiB

/// Read a SKILL.md, refusing to load more than [`MAX_SKILL_MD_BYTES`].
/// If the file exceeds the cap we return an error rather than truncating,
/// because a truncated frontmatter could be silently mis-parsed.
fn read_skill_md_bounded(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut take = file.take((MAX_SKILL_MD_BYTES as u64) + 1);
    let mut buf = Vec::with_capacity(8 * 1024);
    take.read_to_end(&mut buf)
        .with_context(|| format!("reading {}", path.display()))?;
    if buf.len() > MAX_SKILL_MD_BYTES {
        return Err(anyhow::anyhow!(
            "refusing to read {}: file exceeds {} bytes (this is unusual for a SKILL.md and would force unbounded memory use)",
            path.display(),
            MAX_SKILL_MD_BYTES
        ));
    }
    String::from_utf8(buf).with_context(|| format!("{} is not valid UTF-8", path.display()))
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Skill {
    pub path: PathBuf,
    pub name: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
}

/// Result of [`discover`]: the accepted skills plus non-fatal warnings
/// surfaced during the walk (oversize SKILL.md skipped, case-insensitive
/// name collisions, mixed-script "lookalike" names). Callers are expected
/// to forward `warnings` through `ui::log_warning` so they respect the
/// `--json` gating and don't pollute stderr in machine-consumed output.
#[derive(Debug, Default)]
pub struct DiscoverOutput {
    pub skills: Vec<Skill>,
    pub warnings: Vec<String>,
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
pub fn discover(root: &Path, include_vendored: bool) -> Result<DiscoverOutput> {
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
    let mut warnings: Vec<String> = Vec::new();
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
        let raw = match read_skill_md_bounded(skill_md) {
            Ok(s) => s,
            Err(e) => {
                // A SKILL.md that exceeds the cap or fails to read at all is
                // a per-skill problem, not a reason to abort the whole walk.
                warnings.push(format!("skipping {}: {e}", skill_md.display()));
                continue;
            }
        };
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
        // L3 (Phase 9): warn on names mixing distinct Unicode scripts —
        // e.g. Cyrillic `а` next to Latin `a` are visually identical but
        // would be different skills to skillctl. We accept the skill (the
        // operator may have a legitimate multilingual name) but surface a
        // warning so the operator can spot homograph attacks from a
        // malicious library author.
        if mixes_distinct_scripts(&resolved_name) {
            warnings.push(format!(
                "skill name `{resolved_name}` mixes distinct Unicode scripts (e.g. Latin + Cyrillic); double-check this isn't a homograph attack on a similarly-named skill"
            ));
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

    // L6 (Phase 9): warn on case-insensitive name collisions. Two skills
    // named `Foo` and `foo` are distinct under skillctl's canonical
    // comparison (we treat names as identifier-class strings, case
    // significant) but resolve to the same folder on case-insensitive
    // filesystems (APFS-CI, HFS+, NTFS) — `add` would clobber one onto
    // the other silently. Warn once per collision group.
    let mut by_lower: std::collections::HashMap<String, Vec<&str>> =
        std::collections::HashMap::new();
    for s in &skills {
        by_lower
            .entry(s.name.to_lowercase())
            .or_default()
            .push(&s.name);
    }
    let mut collision_groups: Vec<Vec<String>> = by_lower
        .into_iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(_, names)| {
            let mut v: Vec<String> = names.into_iter().map(String::from).collect();
            v.sort();
            v
        })
        .collect();
    collision_groups.sort();
    for group in collision_groups {
        warnings.push(format!(
            "case-insensitive collision: {group:?} resolve to the same identifier on case-insensitive filesystems (APFS-CI, HFS+, NTFS)"
        ));
    }

    Ok(DiscoverOutput { skills, warnings })
}

/// True when `s` contains characters from two or more distinct Unicode
/// scripts (ignoring `Common`, `Inherited`, and `Unknown` — digits,
/// punctuation, and emoji don't count). Used as a homograph heuristic
/// at the discovery boundary.
fn mixes_distinct_scripts(s: &str) -> bool {
    use unicode_script::{Script, UnicodeScript};
    let mut seen: Option<Script> = None;
    for c in s.chars() {
        let script = c.script();
        if matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
            continue;
        }
        match seen {
            None => seen = Some(script),
            Some(prev) if prev == script => {}
            Some(_) => return true,
        }
    }
    false
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
    // Strip a leading UTF-8 BOM (U+FEFF, encoded as `EF BB BF`). Common
    // editors (Notepad on Windows, sometimes VS Code on first save with a
    // certain encoding setting) add one silently — without this strip the
    // first line is `\u{FEFF}---` instead of `---` and the whole file is
    // treated as "no frontmatter."
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
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
    let raw = read_skill_md_bounded(skill_md)?;
    let (_, _, tags) = parse_frontmatter(&raw);
    // Mirror the sanitisation in `discover`: filter out tags with control
    // bytes so a malicious local SKILL.md cannot inject ANSI/OSC-8 into
    // tag-matching errors or `--json` output.
    Ok(tags
        .into_iter()
        .filter(|t| validate_identifier("tag", t).is_ok())
        .collect())
}

/// Rewrite a SKILL.md's `tags:` frontmatter to exactly `new_tags`, preserving
/// every other byte of the file (other frontmatter keys, the body, and the
/// line-ending style). An existing inline (`tags: [..]`) or block (`tags:\n  -
/// x`) representation is replaced in place with a canonical inline `tags: [a,
/// b, c]`; with no existing `tags:`, one is inserted just before the closing
/// `---`. Empty `new_tags` drops the field. Errors if the file has no
/// frontmatter (we won't guess where metadata belongs). The write is atomic
/// (temp + rename), so a crash mid-write never truncates the SKILL.md.
pub fn set_tags(skill_md: &Path, new_tags: &[String]) -> Result<()> {
    let raw = read_skill_md_bounded(skill_md)?;
    let (bom, body) = match raw.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest),
        None => (false, raw.as_str()),
    };
    let newline = if body.contains("\r\n") { "\r\n" } else { "\n" };

    // Keep each line's trailing newline so concatenation round-trips exactly.
    let segments: Vec<&str> = body.split_inclusive('\n').collect();
    if segments.first().map(|s| s.trim()) != Some("---") {
        return Err(anyhow::anyhow!(
            "{} has no `---` frontmatter block to edit; add a `tags:` field by hand",
            skill_md.display()
        ));
    }
    let close = segments
        .iter()
        .enumerate()
        .skip(1)
        .take(MAX_FRONTMATTER_LINES)
        .find(|(_, s)| s.trim() == "---")
        .map(|(i, _)| i)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{} has an unterminated frontmatter block",
                skill_md.display()
            )
        })?;

    // Locate the column-0 `tags:` key (as the parser reads it) plus any
    // block-form `- item` lines that follow it.
    let mut tags_start = None;
    let mut tags_end = close; // exclusive
    for (i, seg) in segments.iter().enumerate().take(close).skip(1) {
        if let Some(rest) = seg.trim_end_matches(['\n', '\r']).strip_prefix("tags:") {
            tags_start = Some(i);
            let mut end = i + 1;
            if rest.trim().is_empty() {
                while end < close {
                    let item = segments[end].trim();
                    if item == "-" || item.starts_with("- ") {
                        end += 1;
                    } else {
                        break;
                    }
                }
            }
            tags_end = end;
            break;
        }
    }

    let canonical = if new_tags.is_empty() {
        None
    } else {
        let inner = new_tags
            .iter()
            .map(|t| format_tag(t))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("tags: [{inner}]{newline}"))
    };

    // With no existing `tags:`, insert at the closing fence; otherwise replace
    // the existing representation in place.
    let insert_at = tags_start.unwrap_or(close);
    let remove_end = if tags_start.is_some() {
        tags_end
    } else {
        close
    };

    let mut out = String::with_capacity(raw.len() + 32);
    if bom {
        out.push('\u{feff}');
    }
    for (i, seg) in segments.iter().enumerate() {
        if i == insert_at {
            if let Some(c) = &canonical {
                out.push_str(c);
            }
        }
        if i >= insert_at && i < remove_end {
            continue;
        }
        out.push_str(seg);
    }

    atomic_write(skill_md, out.as_bytes())
}

/// Format one tag for a canonical inline `tags: [...]` list: bare when it's a
/// simple token, double-quoted when it contains a structural or whitespace
/// character so it round-trips through `parse_tags_inline`.
fn format_tag(t: &str) -> String {
    let needs_quote = t.is_empty()
        || t.chars()
            .any(|c| matches!(c, ',' | '[' | ']' | '"' | '\'') || c.is_whitespace());
    if needs_quote {
        format!("\"{}\"", t.replace('"', "\\\""))
    } else {
        t.to_string()
    }
}

/// Write `bytes` to `path` atomically: stage in a sibling temp file, then
/// rename over the target so a crash never leaves a half-written SKILL.md.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".skillctl-tags.tmp.{pid}.{nanos}"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()));
    }
    Ok(())
}

/// Strip a balanced pair of wrapping quotes (`"..."` or `'...'`) from a
/// scalar value. Mismatched quotes (`"foo'`) are left as-is — silently
/// stripping them used to mask malformed input and let mixed-quote values
/// flow downstream where they could be mis-parsed by tools that don't
/// share our tolerance.
fn clean_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.chars().next();
        let last = trimmed.chars().next_back();
        if (first == Some('"') && last == Some('"')) || (first == Some('\'') && last == Some('\''))
        {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
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

    fn write_set_read(initial: &str, new_tags: &[&str]) -> (String, Vec<String>) {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("SKILL.md");
        std::fs::write(&p, initial).unwrap();
        let owned: Vec<String> = new_tags.iter().map(|s| s.to_string()).collect();
        set_tags(&p, &owned).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        let tags = read_tags(&p).unwrap();
        (raw, tags)
    }

    #[test]
    fn set_tags_replaces_inline_and_preserves_other_lines() {
        let initial = "---\nname: foo\ndescription: d\ntags: [old]\n---\n\n# Body\nkeep me\n";
        let (raw, tags) = write_set_read(initial, &["a", "b"]);
        assert_eq!(tags, vec!["a".to_string(), "b".to_string()]);
        assert!(raw.contains("tags: [a, b]"), "got:\n{raw}");
        assert!(raw.contains("name: foo") && raw.contains("description: d"));
        assert!(
            raw.contains("# Body\nkeep me\n"),
            "body must survive:\n{raw}"
        );
        assert!(!raw.contains("[old]"));
    }

    #[test]
    fn set_tags_replaces_block_form() {
        let initial = "---\nname: foo\ntags:\n  - x\n  - y\n---\nbody\n";
        let (raw, tags) = write_set_read(initial, &["z"]);
        assert_eq!(tags, vec!["z".to_string()]);
        assert!(raw.contains("tags: [z]"), "got:\n{raw}");
        // The old block items are gone and the body is intact.
        assert!(!raw.contains("- x") && !raw.contains("- y"));
        assert!(raw.contains("name: foo") && raw.ends_with("body\n"));
    }

    #[test]
    fn set_tags_inserts_when_absent() {
        let initial = "---\nname: foo\ndescription: d\n---\nbody\n";
        let (raw, tags) = write_set_read(initial, &["new"]);
        assert_eq!(tags, vec!["new".to_string()]);
        // Inserted inside the frontmatter, before the closing fence.
        let fm_end = raw.find("\n---\n").unwrap();
        assert!(raw[..fm_end].contains("tags: [new]"), "got:\n{raw}");
    }

    #[test]
    fn set_tags_empty_removes_the_field() {
        let initial = "---\nname: foo\ntags: [a, b]\n---\nbody\n";
        let (raw, tags) = write_set_read(initial, &[]);
        assert!(tags.is_empty());
        assert!(!raw.contains("tags:"), "tags field should be gone:\n{raw}");
        assert!(raw.contains("name: foo") && raw.contains("body"));
    }

    #[test]
    fn set_tags_quotes_values_needing_it() {
        let initial = "---\ntags: [a]\n---\n";
        let (raw, tags) = write_set_read(initial, &["hello world", "simple"]);
        assert!(raw.contains("\"hello world\""), "got:\n{raw}");
        assert_eq!(tags, vec!["hello world".to_string(), "simple".to_string()]);
    }

    #[test]
    fn set_tags_errors_without_frontmatter() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("SKILL.md");
        std::fs::write(&p, "# no frontmatter here\n").unwrap();
        assert!(set_tags(&p, &["a".to_string()]).is_err());
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

        let skills = discover(lib.path(), false).unwrap().skills;
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

        let skills = discover(lib.path(), false).unwrap().skills;
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

        let skills = discover(work.path(), false).unwrap().skills;
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

        let skills = discover(work.path(), true).unwrap().skills;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"evil"),
            "with --include-vendored, node_modules is walked: {names:?}"
        );
    }

    #[test]
    fn discover_warns_on_case_insensitive_collision() {
        let work = TempDir::new().unwrap();
        write_skill(work.path(), "alpha", "---\nname: Foo\n---");
        write_skill(work.path(), "bravo", "---\nname: foo\n---");
        let out = discover(work.path(), false).unwrap();
        assert_eq!(out.skills.len(), 2);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("case-insensitive collision")
                    && w.contains("foo")
                    && w.contains("Foo")),
            "expected a case-collision warning, got: {:?}",
            out.warnings
        );
    }

    #[test]
    fn discover_no_collision_warning_for_distinct_names() {
        let work = TempDir::new().unwrap();
        write_skill(work.path(), "a", "---\nname: alpha\n---");
        write_skill(work.path(), "b", "---\nname: bravo\n---");
        let out = discover(work.path(), false).unwrap();
        assert!(
            out.warnings.iter().all(|w| !w.contains("case-insensitive")),
            "no collision warning expected, got: {:?}",
            out.warnings
        );
    }

    #[test]
    fn discover_warns_on_mixed_script_name() {
        let work = TempDir::new().unwrap();
        // Latin `a` followed by Cyrillic `а` (U+0430). Same glyph, different
        // characters — classic homograph attack pattern.
        let mixed = "cl\u{0430}ude"; // "claude" with Cyrillic а
        write_skill(work.path(), "mixed", &format!("---\nname: {mixed}\n---"));
        let out = discover(work.path(), false).unwrap();
        assert_eq!(out.skills.len(), 1);
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("distinct Unicode scripts") || w.contains("mixes")),
            "expected mixed-script warning, got: {:?}",
            out.warnings
        );
    }

    #[test]
    fn discover_no_warning_for_pure_ascii_or_pure_script() {
        let work = TempDir::new().unwrap();
        write_skill(work.path(), "ascii", "---\nname: claude-api\n---");
        // Pure-Cyrillic name: no script mix.
        write_skill(
            work.path(),
            "cyrillic",
            "---\nname: \u{043F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}\n---",
        );
        let out = discover(work.path(), false).unwrap();
        assert!(
            out.warnings
                .iter()
                .all(|w| !w.contains("distinct Unicode scripts")),
            "no homograph warning expected, got: {:?}",
            out.warnings
        );
    }

    #[test]
    fn mixes_distinct_scripts_unit() {
        assert!(!super::mixes_distinct_scripts("claude"));
        assert!(!super::mixes_distinct_scripts(
            "\u{043A}\u{043B}\u{043E}\u{0434}"
        )); // pure Cyrillic
        assert!(!super::mixes_distinct_scripts("foo-123_bar.baz")); // ASCII + Common
        assert!(super::mixes_distinct_scripts("cl\u{0430}ude")); // Latin + Cyrillic
        assert!(super::mixes_distinct_scripts("a\u{4E00}")); // Latin + Han
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

        let skills = discover(work.path(), false).unwrap().skills;
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
    fn read_skill_md_bounded_rejects_oversize() {
        let work = TempDir::new().unwrap();
        let path = work.path().join("BIG.md");
        // 1 MiB + 1 byte. The cap rejects on > MAX_SKILL_MD_BYTES, so we
        // need at least one extra byte over the cap.
        let mut content = vec![b'x'; super::MAX_SKILL_MD_BYTES + 1];
        content[0] = b'-';
        std::fs::write(&path, &content).unwrap();
        let err = super::read_skill_md_bounded(&path).unwrap_err().to_string();
        assert!(
            err.contains("exceeds") || err.contains("unbounded"),
            "expected oversize error, got: {err}"
        );
    }

    #[test]
    fn read_skill_md_bounded_accepts_just_under_cap() {
        let work = TempDir::new().unwrap();
        let path = work.path().join("OK.md");
        std::fs::write(&path, vec![b'x'; super::MAX_SKILL_MD_BYTES]).unwrap();
        assert!(super::read_skill_md_bounded(&path).is_ok());
    }

    #[test]
    fn strips_utf8_bom_before_frontmatter() {
        let raw = "\u{feff}---\nname: foo\n---\n";
        let (name, _, _) = parse_frontmatter(raw);
        assert_eq!(name.as_deref(), Some("foo"));
    }

    #[test]
    fn balanced_double_quotes_stripped() {
        assert_eq!(clean_value("\"foo\""), "foo");
    }

    #[test]
    fn balanced_single_quotes_stripped() {
        assert_eq!(clean_value("'foo'"), "foo");
    }

    #[test]
    fn mismatched_quotes_left_as_is() {
        assert_eq!(clean_value("\"foo'"), "\"foo'");
        assert_eq!(clean_value("'foo\""), "'foo\"");
    }

    #[test]
    fn unquoted_value_passes_through() {
        assert_eq!(clean_value("foo"), "foo");
        assert_eq!(clean_value("  foo  "), "foo");
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

        let skills = discover(lib.path(), false).unwrap().skills;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "foo");
        assert!(
            skills[0].description.is_none(),
            "ANSI-poisoned description should be stripped"
        );
    }
}

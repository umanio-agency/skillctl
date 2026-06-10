use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::path_safety::validate_relative_subpath;
use crate::sanitize::validate_identifier;

const FILENAME: &str = ".skills.toml";

/// Hard cap on the number of `[[installed]]` entries we will load from a
/// `.skills.toml`. A malicious PR could otherwise bury 1M entries that each
/// trigger a `safe_join` + git fetch path, OOM'ing the diff classifier.
/// 256 is comfortably above any realistic library size.
const MAX_INSTALLED_ENTRIES: usize = 256;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub installed: Vec<InstalledSkill>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledSkill {
    pub name: String,
    pub source_path: PathBuf,
    pub source_sha: String,
    pub destination: PathBuf,
    pub installed_at: String,
    /// Provenance — the name of the library this skill was installed from.
    /// Optional for back-compat with manifests written before multi-library
    /// support; absence means it came from the (then sole) default library.
    /// The name is a local alias; `library_url` is the durable key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<String>,
    /// Provenance — the URL of the library this skill was installed from.
    /// Durable across `library` renames/removals; the routing key for which
    /// library `push`/`pull` act against once multi-library routing lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_url: Option<String>,
}

impl InstalledSkill {
    /// Reject entries whose `source_path` or `destination` could escape the
    /// library/project root (absolute paths, `..` traversal).
    ///
    /// `.skills.toml` is committed to projects and exchanged via PR; without
    /// this check a single malicious PR could weaponise `pull`/`push` into
    /// deleting arbitrary directories on a maintainer's machine, or read
    /// outside the library cache on the library side.
    pub fn validate(&self) -> Result<(), AppError> {
        // `name` is single-line and ends up in commit subjects and terminal
        // output — strict identifier check.
        validate_identifier("name in .skills.toml", &self.name)?;

        // `source_sha` is later passed as a positional refspec to `git ls-tree
        // <refspec> -- <path>` (which sits before `--` in the argv). Without
        // validation, a value starting with `-` becomes a git flag, corrupting
        // the diff classifier and the downstream `pull`/`push` destructive
        // choices. Lock it down to hex (sha1: 40 chars, sha256: 64 chars).
        if !is_hex_sha(&self.source_sha) {
            return Err(AppError::Config(format!(
                "invalid source_sha for skill `{}` in .skills.toml: expected 40-64 hex characters, got `{}`",
                self.name, self.source_sha
            )));
        }

        validate_relative_subpath(&self.source_path).map_err(|e| {
            AppError::Config(format!(
                "invalid source_path for skill `{}` in .skills.toml: {e}",
                self.name
            ))
        })?;
        validate_relative_subpath(&self.destination).map_err(|e| {
            AppError::Config(format!(
                "invalid destination for skill `{}` in .skills.toml: {e}",
                self.name
            ))
        })?;

        // Provenance fields travel via PR-merged `.skills.toml` and are echoed
        // in logs/JSON; reject control characters (CRLF trailer forgery, ANSI
        // hijack) on both. `validate_identifier` rejects all control chars
        // while still allowing the URL punctuation (`/ : . @ ?`) a URL needs.
        if let Some(library) = &self.library {
            validate_identifier("library in .skills.toml", library)?;
        }
        if let Some(library_url) = &self.library_url {
            validate_identifier("library_url in .skills.toml", library_url)?;
            // `library_url` is the durable routing key for `push`/`pull`. It
            // must be a parseable repo URL: an unparseable value would be
            // ignored by the URL comparison in `Library::matches_provenance`
            // and could otherwise let a foreign skill fall back to a matching
            // `library` name alias. Reject it at the boundary (fail closed).
            crate::host::parse_remote_url(library_url).map_err(|e| {
                AppError::Config(format!(
                    "invalid library_url for skill `{}` in .skills.toml: {e}",
                    self.name
                ))
            })?;
        }
        Ok(())
    }
}

fn is_hex_sha(s: &str) -> bool {
    let len = s.len();
    (40..=64).contains(&len)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b))
}

pub fn path(project_root: &Path) -> PathBuf {
    project_root.join(FILENAME)
}

pub fn load(project_root: &Path) -> Result<ProjectConfig> {
    let p = path(project_root);
    if !p.exists() {
        return Ok(ProjectConfig::default());
    }
    let raw = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let cfg: ProjectConfig =
        toml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))?;
    if cfg.installed.len() > MAX_INSTALLED_ENTRIES {
        return Err(AppError::Config(format!(
            "{} has {} `[[installed]]` entries (cap is {}); refusing to load",
            p.display(),
            cfg.installed.len(),
            MAX_INSTALLED_ENTRIES
        ))
        .into());
    }
    // Per-entry path + name + sha validation.
    for installed in &cfg.installed {
        installed.validate()?;
    }
    // Duplicate detection: two entries with the same `name` or same
    // `destination` would make every command ambiguous (which one wins?).
    // Better to refuse loading and let the operator de-dup by hand than to
    // silently keep one and drop the other.
    for i in 0..cfg.installed.len() {
        for j in (i + 1)..cfg.installed.len() {
            if cfg.installed[i].name == cfg.installed[j].name {
                return Err(AppError::Config(format!(
                    "{} has duplicate `[[installed]]` entries with name `{}`; remove the older one",
                    p.display(),
                    cfg.installed[i].name
                ))
                .into());
            }
            if cfg.installed[i].destination == cfg.installed[j].destination {
                return Err(AppError::Config(format!(
                    "{} has duplicate `[[installed]]` entries with destination `{}`; remove the older one",
                    p.display(),
                    cfg.installed[i].destination.display()
                ))
                .into());
            }
        }
    }
    Ok(cfg)
}

/// Atomically write `.skills.toml`. We write the new content to a sibling
/// temp file, then `fs::rename` it over the target — a crash mid-write only
/// leaves the temp file on disk (cleaned up on the next failure path); the
/// live `.skills.toml` is never truncated.
pub fn save(project_root: &Path, cfg: &ProjectConfig) -> Result<()> {
    let p = path(project_root);
    let raw = toml::to_string_pretty(cfg).context("serializing project config")?;

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = p.with_file_name(format!(".skills.toml.tmp.{pid}.{nanos}"));

    if let Err(e) = fs::write(&tmp, &raw) {
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    if let Err(e) = fs::rename(&tmp, &p) {
        let _ = fs::remove_file(&tmp);
        return Err(e)
            .with_context(|| format!("atomic rename {} -> {}", tmp.display(), p.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const VALID_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn make_skill(name: &str, source_path: &str, destination: &str) -> InstalledSkill {
        InstalledSkill {
            name: name.to_string(),
            source_path: PathBuf::from(source_path),
            source_sha: VALID_SHA.to_string(),
            destination: PathBuf::from(destination),
            installed_at: "2026-05-20T00:00:00Z".to_string(),
            library: None,
            library_url: None,
        }
    }

    #[test]
    fn validate_accepts_safe_relative_paths() {
        let s = make_skill("foo", "skills/foo", ".claude/skills/foo");
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_absolute_destination() {
        let s = make_skill("evil", "skills/foo", "/home/seb/.ssh");
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("destination"));
        assert!(err.contains("absolute"));
    }

    #[test]
    fn validate_rejects_parent_traversal_destination() {
        let s = make_skill("evil", "skills/foo", "../../../etc");
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("destination"));
        assert!(err.contains(".."));
    }

    #[test]
    fn validate_rejects_absolute_source_path() {
        let s = make_skill("evil", "/home/seb/.aws", ".claude/skills/foo");
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("source_path"));
    }

    #[test]
    fn validate_rejects_parent_traversal_source_path() {
        let s = make_skill("evil", "../../../etc", ".claude/skills/foo");
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("source_path"));
        assert!(err.contains(".."));
    }

    #[test]
    fn load_rejects_malicious_skills_toml_destination() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "evil"
source_path = "skills/evil"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = "/home/seb/.ssh"
installed_at = "2026-05-20T00:00:00Z"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let err = load(work.path()).unwrap_err().to_string();
        assert!(
            err.contains("destination") && err.contains("absolute"),
            "expected a path-safety error, got: {err}"
        );
    }

    #[test]
    fn load_rejects_malicious_skills_toml_parent_traversal() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "evil"
source_path = "../../../etc"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/evil"
installed_at = "2026-05-20T00:00:00Z"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let err = load(work.path()).unwrap_err().to_string();
        assert!(
            err.contains("source_path") && err.contains(".."),
            "expected a path-safety error, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_non_hex_source_sha() {
        let mut s = make_skill("foo", "skills/foo", ".claude/skills/foo");
        s.source_sha = "--name-only".to_string();
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("source_sha"));
        assert!(err.contains("hex"));
    }

    #[test]
    fn validate_rejects_too_short_source_sha() {
        let mut s = make_skill("foo", "skills/foo", ".claude/skills/foo");
        s.source_sha = "deadbeef".to_string();
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_rejects_too_long_source_sha() {
        let mut s = make_skill("foo", "skills/foo", ".claude/skills/foo");
        s.source_sha = "a".repeat(65);
        assert!(s.validate().is_err());
    }

    #[test]
    fn validate_accepts_sha1_and_sha256() {
        let mut s = make_skill("foo", "skills/foo", ".claude/skills/foo");
        s.source_sha = "a".repeat(40); // sha1
        assert!(s.validate().is_ok());
        s.source_sha = "a".repeat(64); // sha256
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_newline_in_name() {
        let s = make_skill(
            "foo\nCo-Authored-By: evil",
            "skills/foo",
            ".claude/skills/foo",
        );
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("name"));
        assert!(err.contains("control character"));
    }

    #[test]
    fn validate_rejects_ansi_in_name() {
        let s = make_skill("\x1b[31mEVIL\x1b[0m", "skills/foo", ".claude/skills/foo");
        assert!(s.validate().is_err());
    }

    #[test]
    fn load_rejects_unknown_top_level_key() {
        let work = TempDir::new().unwrap();
        let raw = r#"
mystery_field = "from the future"

[[installed]]
name = "foo"
source_path = "skills/foo"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/foo"
installed_at = "2026-05-22T00:00:00Z"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        // anyhow's Display only shows the outermost context by default;
        // `{:#}` walks the cause chain so the underlying serde error
        // ("unknown field `mystery_field`") surfaces.
        let err = format!("{:#}", load(work.path()).unwrap_err());
        assert!(
            err.contains("unknown field") || err.contains("mystery_field"),
            "expected an unknown-field error, got: {err}"
        );
    }

    #[test]
    fn load_rejects_unknown_installed_key() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "foo"
source_path = "skills/foo"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/foo"
installed_at = "2026-05-22T00:00:00Z"
sneaky = true
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let err = format!("{:#}", load(work.path()).unwrap_err());
        assert!(
            err.contains("unknown field") || err.contains("sneaky"),
            "expected an unknown-field error, got: {err}"
        );
    }

    #[test]
    fn load_rejects_duplicate_name() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "foo"
source_path = "skills/foo"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/foo"
installed_at = "2026-05-22T00:00:00Z"

[[installed]]
name = "foo"
source_path = "skills/foo2"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/foo-alt"
installed_at = "2026-05-22T00:00:00Z"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let err = load(work.path()).unwrap_err().to_string();
        assert!(
            err.contains("duplicate") && err.contains("foo"),
            "expected duplicate error, got: {err}"
        );
    }

    #[test]
    fn load_rejects_duplicate_destination() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "foo"
source_path = "skills/foo"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/shared"
installed_at = "2026-05-22T00:00:00Z"

[[installed]]
name = "bar"
source_path = "skills/bar"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/shared"
installed_at = "2026-05-22T00:00:00Z"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let err = load(work.path()).unwrap_err().to_string();
        assert!(
            err.contains("duplicate") && err.contains("destination"),
            "expected duplicate destination error, got: {err}"
        );
    }

    #[test]
    fn load_rejects_too_many_entries() {
        let work = TempDir::new().unwrap();
        let mut raw = String::new();
        for i in 0..(MAX_INSTALLED_ENTRIES + 1) {
            raw.push_str(&format!(
                "[[installed]]\nname = \"s{i}\"\nsource_path = \"skills/s{i}\"\nsource_sha = \"0123456789abcdef0123456789abcdef01234567\"\ndestination = \".claude/skills/s{i}\"\ninstalled_at = \"2026-05-22T00:00:00Z\"\n\n"
            ));
        }
        fs::write(work.path().join(".skills.toml"), &raw).unwrap();
        let err = load(work.path()).unwrap_err().to_string();
        assert!(err.contains("cap"), "expected entry-cap error, got: {err}");
    }

    #[test]
    fn load_accepts_well_formed_skills_toml() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "foo"
source_path = "skills/foo"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/foo"
installed_at = "2026-05-20T00:00:00Z"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let cfg = load(work.path()).unwrap();
        assert_eq!(cfg.installed.len(), 1);
        assert_eq!(cfg.installed[0].name, "foo");
        // Pre-multi-library manifests have no provenance fields.
        assert!(cfg.installed[0].library.is_none());
        assert!(cfg.installed[0].library_url.is_none());
    }

    #[test]
    fn load_accepts_provenance_fields() {
        let work = TempDir::new().unwrap();
        let raw = r#"
[[installed]]
name = "foo"
source_path = "skills/foo"
source_sha = "0123456789abcdef0123456789abcdef01234567"
destination = ".claude/skills/foo"
installed_at = "2026-05-20T00:00:00Z"
library = "personal"
library_url = "https://github.com/o/r"
"#;
        fs::write(work.path().join(".skills.toml"), raw).unwrap();
        let cfg = load(work.path()).unwrap();
        assert_eq!(cfg.installed[0].library.as_deref(), Some("personal"));
        assert_eq!(
            cfg.installed[0].library_url.as_deref(),
            Some("https://github.com/o/r")
        );
    }

    #[test]
    fn provenance_roundtrips_through_save() {
        let work = TempDir::new().unwrap();
        let cfg = ProjectConfig {
            installed: vec![InstalledSkill {
                library: Some("personal".to_string()),
                library_url: Some("https://github.com/o/r".to_string()),
                ..make_skill("foo", "skills/foo", ".claude/skills/foo")
            }],
        };
        save(work.path(), &cfg).unwrap();
        let reloaded = load(work.path()).unwrap();
        assert_eq!(reloaded.installed[0].library.as_deref(), Some("personal"));
    }

    #[test]
    fn validate_rejects_unparseable_library_url() {
        // Security: an unparseable `library_url` is the routing-bypass vector,
        // so it must be rejected at load time, not silently ignored.
        let s = InstalledSkill {
            library: Some("personal".to_string()),
            library_url: Some("not-a-url".to_string()),
            ..make_skill("foo", "skills/foo", ".claude/skills/foo")
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("library_url"), "got: {err}");
    }

    #[test]
    fn validate_accepts_scp_library_url() {
        let s = InstalledSkill {
            library: Some("personal".to_string()),
            library_url: Some("git@github.com:o/r.git".to_string()),
            ..make_skill("foo", "skills/foo", ".claude/skills/foo")
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_control_char_in_library() {
        let s = InstalledSkill {
            library: Some("per\nsonal".to_string()),
            ..make_skill("foo", "skills/foo", ".claude/skills/foo")
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("library"), "got: {err}");
    }
}

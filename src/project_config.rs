use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::path_safety::validate_relative_subpath;
use crate::sanitize::validate_identifier;

const FILENAME: &str = ".skills.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProjectConfig {
    #[serde(default)]
    pub installed: Vec<InstalledSkill>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub name: String,
    pub source_path: PathBuf,
    pub source_sha: String,
    pub destination: PathBuf,
    pub installed_at: String,
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
    for installed in &cfg.installed {
        installed.validate()?;
    }
    Ok(cfg)
}

pub fn save(project_root: &Path, cfg: &ProjectConfig) -> Result<()> {
    let p = path(project_root);
    let raw = toml::to_string_pretty(cfg).context("serializing project config")?;
    fs::write(&p, raw).with_context(|| format!("writing {}", p.display()))?;
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
    }
}

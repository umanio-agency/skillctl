use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::sanitize::validate_identifier;

const QUALIFIER: &str = "dev";
const ORGANIZATION: &str = "umanio-agency";
const APPLICATION: &str = "skills-cli";

/// Name assigned to the primary library when migrating a legacy single-library
/// `config.toml`, or when `skillctl init` creates the first library.
pub const PRIMARY_LIBRARY_NAME: &str = "personal";

/// What skillctl is allowed to do with a configured library.
///
/// Only `Read` vs not-`Read` is acted on in the current build; `Write` and
/// `Pr` are persisted so the schema is stable, but their behavioural
/// difference (direct commit vs branch + PR/MR) lands in a later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Consume only — write flows refuse this library.
    #[default]
    Read,
    /// Commit straight to the default branch.
    Write,
    /// Push a branch and open a PR/MR for review.
    Pr,
}

impl Access {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Pr => "pr",
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Serialized as a TOML array of `[[library]]` tables.
    #[serde(default, rename = "library")]
    pub libraries: Vec<Library>,
    /// Optional `[propagate]` section. Omitted from the serialized config while
    /// empty, so a config that never uses propagation stays byte-clean.
    #[serde(default, skip_serializing_if = "PropagateConfig::is_empty")]
    pub propagate: PropagateConfig,
}

/// Settings for `skillctl propagate` (and `push --propagate`). Only carries the
/// default scan roots for now — the directories walked for `.skills.toml`
/// install sites when `--root` is not passed on the command line.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PropagateConfig {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

impl PropagateConfig {
    fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub default: bool,
}

impl Library {
    /// Whether an installed skill's recorded provenance points at this library.
    ///
    /// `push`/`pull` act only on skills that belong to the library they target
    /// (the default in this build), skipping skills installed from another
    /// library until cross-library write-back lands. Matching is by normalized
    /// URL (the durable key — survives a `library` rename), then by name; a
    /// skill carrying no provenance at all is treated as belonging to the
    /// default library (pre-multi-library manifests).
    pub fn matches_provenance(&self, library: Option<&str>, library_url: Option<&str>) -> bool {
        if library.is_none() && library_url.is_none() {
            return self.default;
        }
        // `library_url` is the authoritative routing key. When it is present we
        // decide solely on it and **fail closed**: if either side fails to
        // parse we cannot confidently match, so treat the skill as foreign
        // (return false) rather than falling back to the rename-able `library`
        // name. Otherwise an attacker-supplied `.skills.toml` could set an
        // unparseable `library_url` plus the default library's name to slip a
        // foreign skill past the skip and have push/pull act on the wrong cache.
        if let Some(url) = library_url {
            return match (
                crate::host::parse_remote_url(url),
                crate::host::parse_remote_url(&self.url),
            ) {
                (Ok(want), Ok(have)) => want.normalized == have.normalized,
                _ => false,
            };
        }
        // No URL recorded (legacy / hand-written manifest): the name alias is
        // the only signal available.
        library == Some(self.name.as_str())
    }
}

impl Config {
    /// The library that read/write flows act on by default. Falls back to the
    /// first entry if (somehow) none is flagged default — `validate` rejects
    /// that state on load, so this is belt-and-braces.
    pub fn default_library(&self) -> Option<&Library> {
        self.libraries
            .iter()
            .find(|l| l.default)
            .or_else(|| self.libraries.first())
    }

    pub fn by_name(&self, name: &str) -> Option<&Library> {
        self.libraries.iter().find(|l| l.name == name)
    }

    /// Resolve which library a read command (`add`/`list`) consumes from.
    /// `None` → the default library; `Some(name)` → the named library. The
    /// `--from all` span is not a single library, so the caller handles it
    /// before reaching here.
    pub fn resolve_read(&self, from: Option<&str>) -> Result<&Library, AppError> {
        match from {
            None => self.default_library().ok_or_else(|| {
                AppError::Config(
                    "no library configured — run `skillctl init <url>` first".into(),
                )
            }),
            Some(name) => self.by_name(name).ok_or_else(|| {
                AppError::Config(format!(
                    "no library named `{name}` — run `skillctl library list` to see configured libraries"
                ))
            }),
        }
    }

    /// Find the configured library an installed skill belongs to, given the
    /// provenance recorded in its `.skills.toml` entry. This is the inverse of
    /// `Library::matches_provenance` and the routing primitive `push`/`pull`
    /// use to send each skill back to the right library: it reuses the same
    /// fail-closed matching (a present-but-foreign `library_url` matches no
    /// library; absence of any provenance resolves to the default). Returns
    /// `None` when the recorded provenance points at a library that is no
    /// longer configured.
    pub fn resolve_provenance(
        &self,
        library: Option<&str>,
        library_url: Option<&str>,
    ) -> Option<&Library> {
        self.libraries
            .iter()
            .find(|l| l.matches_provenance(library, library_url))
    }

    /// Resolve a write command's explicitly named target library (`--to`),
    /// `None` → the default library. The target must not be `read` — a
    /// read-only source is refused with a pointer to fixing access. (`pr` is
    /// allowed through here; the caller decides between a direct commit and the
    /// branch + PR/MR flow.)
    pub fn resolve_write(&self, to: Option<&str>) -> Result<&Library, AppError> {
        let lib = match to {
            None => self.default_library().ok_or_else(|| {
                AppError::Config("no library configured — run `skillctl init <url>` first".into())
            })?,
            Some(name) => self.by_name(name).ok_or_else(|| {
                AppError::Config(format!(
                    "no library named `{name}` — run `skillctl library list` to see configured libraries"
                ))
            })?,
        };
        if lib.access == Access::Read {
            return Err(AppError::Config(format!(
                "library `{}` is read-only (access = read); choose a writable library with `--to <name>`, or grant it write access in your config",
                lib.name
            )));
        }
        Ok(lib)
    }

    /// The configured libraries a write command can target directly (`write`
    /// access), default library first. Used to pick a target when `--to` is
    /// omitted: zero ⇒ error, one ⇒ use it, many ⇒ ambiguous (Select / `--to`).
    pub fn write_targets(&self) -> Vec<&Library> {
        let mut v: Vec<&Library> = self
            .libraries
            .iter()
            .filter(|l| l.access == Access::Write)
            .collect();
        v.sort_by_key(|l| !l.default);
        v
    }

    /// Register a library, enforcing name uniqueness. The new library becomes
    /// the sole default when `make_default` is set or it is the first one.
    pub fn add_library(&mut self, mut lib: Library, make_default: bool) -> Result<(), AppError> {
        if self.by_name(&lib.name).is_some() {
            return Err(AppError::Conflict(format!(
                "a library named `{}` already exists",
                lib.name
            )));
        }
        if make_default || self.libraries.is_empty() {
            for l in &mut self.libraries {
                l.default = false;
            }
            lib.default = true;
        } else {
            lib.default = false;
        }
        self.libraries.push(lib);
        Ok(())
    }

    /// Drop a library by name and return it. Refuses to remove the default
    /// while other libraries remain (the operator must pick a new default
    /// first); removing the only library leaves an empty config.
    pub fn remove_library(&mut self, name: &str) -> Result<Library, AppError> {
        let idx = self
            .libraries
            .iter()
            .position(|l| l.name == name)
            .ok_or_else(|| AppError::Config(format!("no library named `{name}`")))?;
        if self.libraries[idx].default && self.libraries.len() > 1 {
            return Err(AppError::Conflict(format!(
                "`{name}` is the default library; set another default with `skillctl library set-default <name>` before removing it"
            )));
        }
        Ok(self.libraries.remove(idx))
    }

    pub fn set_default(&mut self, name: &str) -> Result<(), AppError> {
        if self.by_name(name).is_none() {
            return Err(AppError::Config(format!("no library named `{name}`")));
        }
        for l in &mut self.libraries {
            l.default = l.name == name;
        }
        Ok(())
    }

    /// Enforce the invariants every well-formed config must hold once it has
    /// at least one library: non-empty unique names, and exactly one default.
    fn validate(&self) -> Result<(), AppError> {
        if self.libraries.is_empty() {
            return Ok(());
        }
        for lib in &self.libraries {
            if lib.name.is_empty() {
                return Err(AppError::Config(
                    "config.toml has a library with an empty name".into(),
                ));
            }
            validate_identifier("library name in config.toml", &lib.name)?;
            if lib.url.is_empty() {
                return Err(AppError::Config(format!(
                    "config.toml: library `{}` has an empty url",
                    lib.name
                )));
            }
            // The url is copied into `.skills.toml` provenance and echoed in
            // logs/JSON; reject control chars (CRLF/ANSI) here too, mirroring
            // the gate already applied to `.skills.toml`'s `library_url`.
            validate_identifier("library url in config.toml", &lib.url)?;
        }
        for i in 0..self.libraries.len() {
            for j in (i + 1)..self.libraries.len() {
                if self.libraries[i].name == self.libraries[j].name {
                    return Err(AppError::Config(format!(
                        "config.toml has duplicate libraries named `{}`",
                        self.libraries[i].name
                    )));
                }
            }
        }
        // Reject two libraries that resolve to the same repository (same
        // normalized URL). They would share one cache directory AND one
        // provenance match, so `resolve_provenance` would route a skill by
        // config order rather than intent — letting a `read`/`pr` library that
        // shares a repo with a `write` sibling be written to, bypassing the
        // access gate. (Distinct repos can't collide: the cache slug hashes the
        // normalized URL.) Only parseable URLs are compared; an unparseable URL
        // never matches provenance (fail-closed), so it can't enable this.
        let mut seen: Vec<(String, &str)> = Vec::with_capacity(self.libraries.len());
        for lib in &self.libraries {
            if let Ok(remote) = crate::host::parse_remote_url(&lib.url) {
                if let Some((_, other)) = seen.iter().find(|(n, _)| *n == remote.normalized) {
                    return Err(AppError::Config(format!(
                        "config.toml: libraries `{}` and `{}` point at the same repository (`{}`); configure each repository at most once",
                        other, lib.name, remote.normalized
                    )));
                }
                seen.push((remote.normalized, &lib.name));
            }
        }
        let defaults = self.libraries.iter().filter(|l| l.default).count();
        if defaults != 1 {
            return Err(AppError::Config(format!(
                "config.toml must have exactly one library marked `default = true` (found {defaults})"
            )));
        }
        Ok(())
    }
}

/// Legacy single-library config shape (`[library]` table with just `url`),
/// from before multi-library support. Parsed only as a fallback when the
/// current `[[library]]` array shape fails to deserialize.
#[derive(Debug, Deserialize)]
struct LegacyConfig {
    library: Option<LegacyLibrary>,
}

#[derive(Debug, Deserialize)]
struct LegacyLibrary {
    url: String,
}

impl LegacyConfig {
    fn migrate(self) -> Config {
        match self.library {
            Some(l) => Config {
                libraries: vec![Library {
                    name: PRIMARY_LIBRARY_NAME.to_string(),
                    url: l.url,
                    access: Access::Write,
                    default: true,
                }],
                ..Config::default()
            },
            None => Config::default(),
        }
    }
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION).ok_or_else(|| {
        anyhow!("could not determine standard project directories for this platform")
    })
}

pub fn config_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.toml"))
}

pub fn cache_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

pub fn library_cache_path(url: &str) -> Result<PathBuf> {
    let remote = crate::host::parse_remote_url(url)?;
    Ok(cache_dir()?.join(crate::host::cache_slug(&remote)))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("reading config at {}", path.display()))?;
    parse(&raw).with_context(|| format!("parsing config at {}", path.display()))
}

/// Parse a config string, transparently migrating the legacy single-`[library]`
/// shape into the current `[[library]]` array. The migration is in-memory
/// only — it is persisted on the next config-writing command, never on a pure
/// read.
fn parse(raw: &str) -> Result<Config> {
    // Current shape: an array of `[[library]]` tables.
    match toml::from_str::<Config>(raw) {
        Ok(cfg) => {
            cfg.validate()?;
            Ok(cfg)
        }
        // A legacy file (`[library]` table) fails the array parse above, so
        // reaching here means either a legacy file or a malformed new-format
        // one. Only treat it as legacy when it actually carries a `[library]`
        // table — a file that parses as `LegacyConfig` merely because it has
        // no library section is far more likely a malformed new-format config,
        // and silently returning "no library" would let the next save() drop
        // the operator's libraries. In that case, surface the new-format error.
        Err(new_err) => match toml::from_str::<LegacyConfig>(raw) {
            Ok(legacy) if legacy.library.is_some() => {
                let migrated = legacy.migrate();
                migrated.validate()?;
                Ok(migrated)
            }
            _ => Err(new_err.into()),
        },
    }
}

/// Atomically persist the config. Refuses to write an invariant-violating
/// config (so a buggy mutator can never brick the next `load`), serializes to
/// a sibling temp file, then `fs::rename`s it over the target — a crash
/// mid-write only leaves the temp file; the live `config.toml` is never
/// truncated. (Mirrors `project_config::save`.) Concurrent config writers can
/// still lose an update to each other — a config-level lock can be added if
/// that race ever bites in practice; the atomic swap already removes the
/// torn-write / zero-byte-file data-loss risk, which is the dangerous part.
pub fn save(config: &Config) -> Result<()> {
    config.validate()?;
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(config).context("serializing config")?;

    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = path.with_file_name(format!("config.toml.tmp.{pid}.{nanos}"));

    if let Err(e) = fs::write(&tmp, &raw) {
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e)
            .with_context(|| format!("atomic rename {} -> {}", tmp.display(), path.display()));
    }
    Ok(())
}

/// Strip an embedded `user[:password]@` userinfo from an HTTPS URL so the
/// stored `Library::url` (echoed in `config.toml`, `--json` output, error
/// chains, and CI logs) cannot leak personal access tokens. URLs without
/// userinfo (SSH `git@host:path` form, `ssh://`, or plain HTTPS) are
/// returned unchanged — for `ssh://git@host/...` the `git@` is the SSH login
/// user, not a credential, so we leave SSH alone.
pub fn sanitize_url_for_display(url: &str) -> String {
    if url.starts_with("git@") || url.starts_with("ssh://") {
        return url.to_string();
    }
    for prefix in ["https://", "http://"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let authority = &rest[..authority_end];
            if let Some(at) = authority.find('@') {
                let stripped_authority = &authority[at + 1..];
                let remainder = &rest[authority_end..];
                return format!("{prefix}{stripped_authority}{remainder}");
            }
            return url.to_string();
        }
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_x_access_token() {
        assert_eq!(
            sanitize_url_for_display(
                "https://x-access-token:ghp_abcdefghijklmnop@github.com/foo/bar"
            ),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn sanitize_strips_user_password() {
        assert_eq!(
            sanitize_url_for_display("https://alice:hunter2@github.com/foo/bar.git"),
            "https://github.com/foo/bar.git"
        );
    }

    #[test]
    fn sanitize_leaves_ssh_alone() {
        assert_eq!(
            sanitize_url_for_display("git@github.com:foo/bar.git"),
            "git@github.com:foo/bar.git"
        );
        assert_eq!(
            sanitize_url_for_display("ssh://git@github.com/foo/bar"),
            "ssh://git@github.com/foo/bar"
        );
    }

    #[test]
    fn sanitize_noop_on_clean_https() {
        assert_eq!(
            sanitize_url_for_display("https://github.com/foo/bar"),
            "https://github.com/foo/bar"
        );
    }

    #[test]
    fn sanitize_does_not_strip_at_in_path() {
        assert_eq!(
            sanitize_url_for_display("https://github.com/foo/bar@v1"),
            "https://github.com/foo/bar@v1"
        );
    }

    fn lib(name: &str, default: bool) -> Library {
        Library {
            name: name.to_string(),
            url: format!("https://github.com/o/{name}"),
            access: Access::Read,
            default,
        }
    }

    #[test]
    fn parse_new_array_shape() {
        let raw = r#"
[[library]]
name = "personal"
url = "https://github.com/o/r"
access = "write"
default = true

[[library]]
name = "team"
url = "https://github.com/o/team"
access = "pr"
"#;
        let cfg = parse(raw).unwrap();
        assert_eq!(cfg.libraries.len(), 2);
        assert_eq!(cfg.default_library().unwrap().name, "personal");
        assert_eq!(cfg.by_name("team").unwrap().access, Access::Pr);
        assert!(!cfg.by_name("team").unwrap().default);
    }

    #[test]
    fn parse_migrates_legacy_single_library() {
        let raw = r#"
[library]
url = "https://github.com/o/r"
"#;
        let cfg = parse(raw).unwrap();
        assert_eq!(cfg.libraries.len(), 1);
        let primary = cfg.default_library().unwrap();
        assert_eq!(primary.name, PRIMARY_LIBRARY_NAME);
        assert_eq!(primary.url, "https://github.com/o/r");
        assert_eq!(primary.access, Access::Write);
        assert!(primary.default);
    }

    #[test]
    fn parse_empty_config_is_no_library() {
        assert!(parse("").unwrap().libraries.is_empty());
    }

    #[test]
    fn parse_rejects_duplicate_names() {
        let raw = r#"
[[library]]
name = "dup"
url = "https://github.com/o/a"
default = true

[[library]]
name = "dup"
url = "https://github.com/o/b"
"#;
        let err = parse(raw).unwrap_err().to_string();
        assert!(
            err.contains("duplicate") && err.contains("dup"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_rejects_zero_defaults() {
        let raw = r#"
[[library]]
name = "a"
url = "https://github.com/o/a"

[[library]]
name = "b"
url = "https://github.com/o/b"
"#;
        let err = parse(raw).unwrap_err().to_string();
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn parse_rejects_multiple_defaults() {
        let raw = r#"
[[library]]
name = "a"
url = "https://github.com/o/a"
default = true

[[library]]
name = "b"
url = "https://github.com/o/b"
default = true
"#;
        let err = parse(raw).unwrap_err().to_string();
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn parse_rejects_control_char_in_name() {
        let raw =
            "[[library]]\nname = \"a\\nb\"\nurl = \"https://github.com/o/a\"\ndefault = true\n";
        assert!(parse(raw).is_err());
    }

    #[test]
    fn parse_rejects_malformed_library_key_rather_than_emptying() {
        // `library` present but the wrong type: must surface an error, never
        // silently fall back to an empty (no-library) config that a later
        // save() would then persist, dropping the operator's libraries.
        assert!(parse("library = \"oops\"\n").is_err());
        assert!(parse("library = 42\n").is_err());
    }

    #[test]
    fn validate_rejects_same_repo_under_two_libraries() {
        // The access-gate bypass H1: a `read` source and a `write` sibling
        // pointing at the same repo (different URL spellings) must be rejected,
        // so a skill's access can't be decided by config order.
        let cfg = Config {
            libraries: vec![
                Library {
                    name: "upstream".into(),
                    url: "git@github.com:o/r.git".into(),
                    access: Access::Read,
                    default: true,
                },
                Library {
                    name: "mine".into(),
                    url: "https://github.com/o/r".into(),
                    access: Access::Write,
                    default: false,
                },
            ],
            ..Config::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("same repository") && err.contains("o/r"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_allows_distinct_repos() {
        let cfg = Config {
            libraries: vec![lib("a", true), lib("b", false)],
            ..Config::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_url() {
        let cfg = Config {
            libraries: vec![Library {
                name: "a".into(),
                url: String::new(),
                access: Access::Read,
                default: true,
            }],
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_control_char_in_url() {
        let cfg = Config {
            libraries: vec![Library {
                name: "a".into(),
                url: "https://github.com/o/a\nevil".into(),
                access: Access::Read,
                default: true,
            }],
            ..Config::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn save_refuses_invalid_config_before_touching_disk() {
        // Two defaults is invalid; save() must reject via validate() before it
        // ever computes a path or writes — so this never hits the real FS.
        let cfg = Config {
            libraries: vec![lib("a", true), lib("b", true)],
            ..Config::default()
        };
        assert!(save(&cfg).is_err());
    }

    #[test]
    fn add_library_first_is_forced_default() {
        let mut cfg = Config::default();
        cfg.add_library(lib("solo", false), false).unwrap();
        assert!(cfg.by_name("solo").unwrap().default);
    }

    #[test]
    fn add_library_extra_is_not_default_by_default() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        cfg.add_library(lib("b", false), false).unwrap();
        assert!(cfg.by_name("a").unwrap().default);
        assert!(!cfg.by_name("b").unwrap().default);
    }

    #[test]
    fn add_library_make_default_moves_the_flag() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        cfg.add_library(lib("b", false), true).unwrap();
        assert!(!cfg.by_name("a").unwrap().default);
        assert!(cfg.by_name("b").unwrap().default);
    }

    #[test]
    fn add_library_rejects_duplicate_name() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        assert!(cfg.add_library(lib("a", false), false).is_err());
    }

    #[test]
    fn remove_library_refuses_default_when_others_remain() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        cfg.add_library(lib("b", false), false).unwrap();
        assert!(cfg.remove_library("a").is_err());
        assert!(cfg.remove_library("b").is_ok());
    }

    #[test]
    fn remove_library_allows_removing_the_only_one() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        assert!(cfg.remove_library("a").is_ok());
        assert!(cfg.libraries.is_empty());
    }

    #[test]
    fn remove_library_unknown_name_errors() {
        let mut cfg = Config::default();
        assert!(cfg.remove_library("ghost").is_err());
    }

    #[test]
    fn set_default_moves_the_flag() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        cfg.add_library(lib("b", false), false).unwrap();
        cfg.set_default("b").unwrap();
        assert!(!cfg.by_name("a").unwrap().default);
        assert!(cfg.by_name("b").unwrap().default);
        assert!(cfg.set_default("ghost").is_err());
    }

    #[test]
    fn resolve_read_defaults_to_default_library() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        cfg.add_library(lib("b", false), false).unwrap();
        assert_eq!(cfg.resolve_read(None).unwrap().name, "a");
    }

    #[test]
    fn resolve_read_named_library() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        cfg.add_library(lib("b", false), false).unwrap();
        assert_eq!(cfg.resolve_read(Some("b")).unwrap().name, "b");
    }

    #[test]
    fn resolve_read_unknown_name_errors() {
        let mut cfg = Config::default();
        cfg.add_library(lib("a", true), true).unwrap();
        assert!(cfg.resolve_read(Some("ghost")).is_err());
    }

    #[test]
    fn resolve_read_no_library_errors() {
        let cfg = Config::default();
        assert!(cfg.resolve_read(None).is_err());
    }

    fn lib_access(name: &str, access: Access, default: bool) -> Library {
        Library {
            name: name.to_string(),
            url: format!("https://github.com/o/{name}"),
            access,
            default,
        }
    }

    #[test]
    fn resolve_write_refuses_read_and_allows_write_pr() {
        let mut cfg = Config::default();
        cfg.add_library(lib_access("src", Access::Read, true), true)
            .unwrap();
        cfg.add_library(lib_access("mine", Access::Write, false), false)
            .unwrap();
        cfg.add_library(lib_access("team", Access::Pr, false), false)
            .unwrap();
        // Default (src) is read-only → refused.
        let err = cfg.resolve_write(None).unwrap_err().to_string();
        assert!(err.contains("read-only"), "got: {err}");
        // Named write + pr both resolve.
        assert_eq!(cfg.resolve_write(Some("mine")).unwrap().name, "mine");
        assert_eq!(cfg.resolve_write(Some("team")).unwrap().access, Access::Pr);
        // Unknown name errors.
        assert!(cfg.resolve_write(Some("ghost")).is_err());
    }

    #[test]
    fn write_targets_lists_write_libs_default_first() {
        let mut cfg = Config::default();
        cfg.add_library(lib_access("src", Access::Read, true), true)
            .unwrap();
        cfg.add_library(lib_access("a", Access::Write, false), false)
            .unwrap();
        cfg.add_library(lib_access("b", Access::Write, false), false)
            .unwrap();
        cfg.set_default("b").unwrap();
        let t = cfg.write_targets();
        // read lib excluded; default (b) first.
        assert_eq!(
            t.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
    }

    #[test]
    fn resolve_provenance_routes_to_owning_library() {
        let mut cfg = Config::default();
        cfg.add_library(
            Library {
                name: "personal".into(),
                url: "https://github.com/o/personal".into(),
                access: Access::Write,
                default: true,
            },
            true,
        )
        .unwrap();
        cfg.add_library(
            Library {
                name: "team".into(),
                url: "https://github.com/o/team".into(),
                access: Access::Write,
                default: false,
            },
            false,
        )
        .unwrap();
        // By URL (durable key), across spellings.
        let r = cfg
            .resolve_provenance(Some("whatever"), Some("git@github.com:o/team.git"))
            .unwrap();
        assert_eq!(r.name, "team");
        // No provenance → default library.
        assert_eq!(cfg.resolve_provenance(None, None).unwrap().name, "personal");
        // A URL matching no configured library → None (removed/renamed repo).
        assert!(
            cfg.resolve_provenance(Some("team"), Some("https://github.com/o/gone"))
                .is_none()
        );
    }

    #[test]
    fn matches_provenance_no_provenance_is_default_library() {
        let mut cfg = Config::default();
        cfg.add_library(lib("personal", true), true).unwrap();
        cfg.add_library(lib("team", false), false).unwrap();
        let personal = cfg.by_name("personal").unwrap();
        let team = cfg.by_name("team").unwrap();
        // A pre-multi-library skill (no provenance) belongs to the default.
        assert!(personal.matches_provenance(None, None));
        assert!(!team.matches_provenance(None, None));
    }

    #[test]
    fn matches_provenance_by_url_across_spellings() {
        let team = Library {
            name: "team".into(),
            url: "https://github.com/o/team".into(),
            access: Access::Read,
            default: false,
        };
        // scp spelling of the same repo still matches by normalized URL.
        assert!(team.matches_provenance(Some("renamed"), Some("git@github.com:o/team.git")));
        // A different repo does not match, even if the name happens to.
        assert!(!team.matches_provenance(Some("team"), Some("https://github.com/o/other")));
    }

    #[test]
    fn matches_provenance_fails_closed_on_unparseable_url() {
        // Security: a present-but-unparseable `library_url` must NOT fall back
        // to the name alias — otherwise a foreign skill claiming the default
        // library's name would route against the wrong cache.
        let default = Library {
            name: "personal".into(),
            url: "https://github.com/o/personal".into(),
            access: Access::Write,
            default: true,
        };
        assert!(!default.matches_provenance(Some("personal"), Some("not-a-url")));
        assert!(!default.matches_provenance(Some("personal"), Some("github.com/o/personal")));
    }

    #[test]
    fn matches_provenance_falls_back_to_name_without_url() {
        let team = Library {
            name: "team".into(),
            url: "https://github.com/o/team".into(),
            access: Access::Read,
            default: false,
        };
        assert!(team.matches_provenance(Some("team"), None));
        assert!(!team.matches_provenance(Some("personal"), None));
    }

    #[test]
    fn migrated_config_roundtrips_through_save_shape() {
        // After migration, serializing produces the new [[library]] array,
        // which must parse back identically.
        let migrated = parse("[library]\nurl = \"https://github.com/o/r\"\n").unwrap();
        let raw = toml::to_string_pretty(&migrated).unwrap();
        assert!(
            raw.contains("[[library]]"),
            "expected array shape, got:\n{raw}"
        );
        let reparsed = parse(&raw).unwrap();
        assert_eq!(reparsed.libraries.len(), 1);
        assert_eq!(
            reparsed.default_library().unwrap().name,
            PRIMARY_LIBRARY_NAME
        );
    }

    #[test]
    fn parse_reads_propagate_roots() {
        let raw = r#"
[[library]]
name = "personal"
url = "https://github.com/o/r"
default = true

[propagate]
roots = ["~/code", "/srv/projects"]
"#;
        let cfg = parse(raw).unwrap();
        assert_eq!(
            cfg.propagate.roots,
            vec![PathBuf::from("~/code"), PathBuf::from("/srv/projects")]
        );
    }

    #[test]
    fn parse_without_propagate_has_empty_roots() {
        let cfg = parse("[library]\nurl = \"https://github.com/o/r\"\n").unwrap();
        assert!(cfg.propagate.roots.is_empty());
    }

    #[test]
    fn empty_propagate_is_omitted_from_serialized_config() {
        let cfg = Config {
            libraries: vec![lib("a", true)],
            ..Config::default()
        };
        let raw = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !raw.contains("[propagate]"),
            "empty propagate section must not be serialized, got:\n{raw}"
        );
    }

    #[test]
    fn propagate_roots_roundtrip_through_serialization() {
        let cfg = Config {
            libraries: vec![lib("a", true)],
            propagate: PropagateConfig {
                roots: vec![PathBuf::from("/a"), PathBuf::from("/b")],
            },
        };
        let raw = toml::to_string_pretty(&cfg).unwrap();
        assert!(raw.contains("[propagate]"), "got:\n{raw}");
        let reparsed = parse(&raw).unwrap();
        assert_eq!(
            reparsed.propagate.roots,
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }
}

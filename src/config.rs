use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const QUALIFIER: &str = "dev";
const ORGANIZATION: &str = "umanio-agency";
const APPLICATION: &str = "skills-cli";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub library: Option<Library>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Library {
    pub url: String,
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
    Ok(cache_dir()?.join(slug_for_url(url)?))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("reading config at {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing config at {}", path.display()))
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir {}", parent.display()))?;
    }
    let raw = toml::to_string_pretty(config).context("serializing config")?;
    fs::write(&path, raw).with_context(|| format!("writing config at {}", path.display()))?;
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

/// Derive a stable cache folder name from a GitHub URL: `owner-repo`.
///
/// Accepts only HTTPS (`https://github.com/owner/repo`) or SSH
/// (`git@github.com:owner/repo`) forms. Plain `http://` is rejected so a
/// network attacker on the operator's link cannot downgrade the clone to
/// cleartext — GitHub itself redirects `http://` to `https://`, but a MITM
/// could intercept the initial connection and serve modified content.
fn slug_for_url(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let tail = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if trimmed.starts_with("http://") {
        return Err(anyhow!(
            "refusing to use cleartext HTTP URL: {url} — use the HTTPS form (`https://github.com/owner/repo`) instead"
        ));
    } else {
        return Err(anyhow!(
            "unsupported URL: {url} — expected a GitHub HTTPS or SSH URL"
        ));
    };
    let (owner, repo) = tail
        .split_once('/')
        .ok_or_else(|| anyhow!("malformed GitHub URL: {url} — expected the form owner/repo"))?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(anyhow!(
            "malformed GitHub URL: {url} — expected the form owner/repo"
        ));
    }
    Ok(format!("{owner}-{repo}"))
}

#[cfg(test)]
mod tests {
    use super::{sanitize_url_for_display, slug_for_url};

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

    #[test]
    fn slug_https() {
        assert_eq!(
            slug_for_url("https://github.com/foo/bar").unwrap(),
            "foo-bar"
        );
    }

    #[test]
    fn slug_https_dot_git() {
        assert_eq!(
            slug_for_url("https://github.com/foo/bar.git").unwrap(),
            "foo-bar"
        );
    }

    #[test]
    fn slug_ssh() {
        assert_eq!(
            slug_for_url("git@github.com:foo/bar.git").unwrap(),
            "foo-bar"
        );
    }

    #[test]
    fn slug_rejects_non_github() {
        assert!(slug_for_url("https://gitlab.com/foo/bar").is_err());
    }

    #[test]
    fn slug_rejects_cleartext_http() {
        let err = slug_for_url("http://github.com/foo/bar")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cleartext") || err.contains("HTTPS"),
            "expected HTTPS-required error, got: {err}"
        );
    }

    #[test]
    fn slug_rejects_malformed() {
        assert!(slug_for_url("https://github.com/foo").is_err());
    }
}

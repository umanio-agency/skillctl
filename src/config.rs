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

/// Derive a stable cache folder name from a GitHub URL: `owner-repo`.
fn slug_for_url(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let tail = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else {
        return Err(anyhow!(
            "unsupported URL: {url} — expected a GitHub HTTPS or SSH URL"
        ));
    };
    let (owner, repo) = tail.split_once('/').ok_or_else(|| {
        anyhow!("malformed GitHub URL: {url} — expected the form owner/repo")
    })?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(anyhow!(
            "malformed GitHub URL: {url} — expected the form owner/repo"
        ));
    }
    Ok(format!("{owner}-{repo}"))
}

#[cfg(test)]
mod tests {
    use super::slug_for_url;

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
    fn slug_rejects_malformed() {
        assert!(slug_for_url("https://github.com/foo").is_err());
    }
}

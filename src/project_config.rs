use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

pub fn path(project_root: &Path) -> PathBuf {
    project_root.join(FILENAME)
}

pub fn load(project_root: &Path) -> Result<ProjectConfig> {
    let p = path(project_root);
    if !p.exists() {
        return Ok(ProjectConfig::default());
    }
    let raw = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing {}", p.display()))
}

pub fn save(project_root: &Path, cfg: &ProjectConfig) -> Result<()> {
    let p = path(project_root);
    let raw = toml::to_string_pretty(cfg).context("serializing project config")?;
    fs::write(&p, raw).with_context(|| format!("writing {}", p.display()))?;
    Ok(())
}

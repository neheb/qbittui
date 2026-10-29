//! Optional TOML config file. Command-line flags and environment variables override it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Shell command whose output is the password, e.g. `pass show qbittorrent`.
    pub password_command: Option<String>,
    pub interval: Option<f64>,
    pub torrent_dir: Option<String>,
    pub mouse: Option<bool>,
}

/// `$XDG_CONFIG_HOME/qbittui/config.toml`, falling back to `~/.config/qbittui/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("qbittui").join("config.toml"))
}

/// Load `path`. A missing file is only an error when the user named it explicitly.
pub fn load(path: &Path, explicit: bool) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn run_password_command(cmd: &str) -> Result<String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("running password_command `{cmd}`"))?;
    if !out.status.success() {
        bail!("password_command `{cmd}` failed with {}", out.status);
    }
    let text = String::from_utf8(out.stdout).context("password_command output is not UTF-8")?;
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

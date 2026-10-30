//! Optional TOML config file. Command-line flags and environment variables override it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// PEM file with a certificate to trust (e.g. a self-signed WebUI certificate).
    pub ca_cert: Option<String>,
    /// Accept any HTTPS certificate. Exposes the password to anyone who can intercept the connection.
    pub insecure: Option<bool>,
}

/// `$XDG_CONFIG_HOME/qbittui/config.toml`, else `%APPDATA%\qbittui\config.toml` on Windows,
/// else `~/.config/qbittui/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = var("XDG_CONFIG_HOME")
        .or_else(|| if cfg!(windows) { var("APPDATA") } else { None })
        .or_else(|| std::env::home_dir().map(|h| h.join(".config")))?;
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

/// `cmd` run by the platform's shell: `sh -c` or `cmd /C`.
fn shell(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        // cmd parses its own command line; Rust's argument quoting would mangle it.
        c.arg("/C").raw_arg(cmd);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

pub fn run_password_command(cmd: &str) -> Result<String> {
    let out = shell(cmd)
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("running password_command `{cmd}`"))?;
    if !out.status.success() {
        bail!("password_command `{cmd}` failed with {}", out.status);
    }
    let text = String::from_utf8(out.stdout).context("password_command output is not UTF-8")?;
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

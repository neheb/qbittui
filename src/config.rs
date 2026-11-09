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
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn load_str(dir: &TempDir, text: &str) -> Result<Config> {
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        load(&path, true)
    }

    #[test]
    fn a_missing_file_is_only_an_error_when_named() {
        let dir = TempDir::new("config-missing");
        let path = dir.path().join("config.toml");
        assert!(load(&path, false).unwrap().url.is_none());
        let err = load(&path, true).unwrap_err();
        assert!(err.to_string().starts_with("reading"), "{err}");
    }

    #[test]
    fn every_key_parses() {
        let dir = TempDir::new("config-full");
        let c = load_str(
            &dir,
            r#"
                url = "http://nas:8080"
                username = "admin"
                password = "secret"
                password_command = "pass show qbittorrent"
                interval = 0.5
                torrent_dir = "~/Downloads/"
                mouse = false
                ca_cert = "~/webui.pem"
                insecure = true
            "#,
        )
        .unwrap();
        assert_eq!(c.url.as_deref(), Some("http://nas:8080"));
        assert_eq!(c.username.as_deref(), Some("admin"));
        assert_eq!(c.password.as_deref(), Some("secret"));
        assert_eq!(c.password_command.as_deref(), Some("pass show qbittorrent"));
        assert_eq!(c.interval, Some(0.5));
        assert_eq!(c.torrent_dir.as_deref(), Some("~/Downloads/"));
        assert_eq!(c.mouse, Some(false));
        assert_eq!(c.ca_cert.as_deref(), Some("~/webui.pem"));
        assert_eq!(c.insecure, Some(true));
        assert!(
            load_str(&dir, "").unwrap().url.is_none(),
            "every key is optional"
        );
    }

    #[test]
    fn typos_and_wrong_types_are_errors() {
        let dir = TempDir::new("config-bad");
        let err = load_str(&dir, "usrname = \"admin\"").unwrap_err();
        assert!(
            format!("{err:#}").contains("unknown field `usrname`"),
            "{err:#}"
        );
        let err = load_str(&dir, "interval = \"fast\"").unwrap_err();
        assert!(format!("{err:#}").contains("interval"), "{err:#}");
    }

    #[test]
    fn password_command_output_is_the_password() {
        assert_eq!(run_password_command("echo hunter2").unwrap(), "hunter2");
        let err = run_password_command("exit 3").unwrap_err();
        assert!(err.to_string().contains("failed"), "{err}");
    }
}

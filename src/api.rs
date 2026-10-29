//! Minimal client for the qBittorrent WebUI API (v2).

use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::blocking::{Client as Http, RequestBuilder, Response, multipart};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Torrent {
    pub hash: String,
    pub name: String,
    pub size: i64,
    pub progress: f64,
    pub dlspeed: i64,
    pub upspeed: i64,
    pub num_seeds: i64,
    pub num_complete: i64,
    pub num_leechs: i64,
    pub num_incomplete: i64,
    pub ratio: f64,
    pub eta: i64,
    pub state: String,
    pub category: String,
    pub tags: String,
    pub added_on: i64,
    pub save_path: String,
    pub tracker: String,
    pub downloaded: i64,
    pub uploaded: i64,
    pub amount_left: i64,
}

impl Torrent {
    pub fn is_paused(&self) -> bool {
        self.state.starts_with("paused") || self.state.starts_with("stopped")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Transfer {
    pub dl_info_speed: i64,
    pub up_info_speed: i64,
    pub dl_info_data: i64,
    pub up_info_data: i64,
    pub dht_nodes: i64,
    pub connection_status: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CategoryInfo {
    pub name: String,
    /// Empty, relative (to the default save path) or absolute.
    #[serde(rename = "savePath")]
    pub save_path: String,
}

/// The few qBittorrent preferences the add dialog needs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub save_path: String,
    /// New torrents use Automatic Torrent Management, which saves into the category's path.
    pub auto_tmm_enabled: bool,
}

/// A torrent to add: magnet link, URL or local .torrent file, plus where it goes.
pub struct NewTorrent {
    pub source: String,
    pub category: Option<String>,
    pub save_path: String,
}

/// How to verify the server's HTTPS certificate.
#[derive(Default)]
pub struct Tls {
    /// Extra PEM certificate(s) to trust, e.g. qBittorrent's self-signed WebUI certificate.
    pub ca_cert: Option<std::path::PathBuf>,
    /// Skip certificate verification entirely.
    pub insecure: bool,
}

pub struct Client {
    http: Http,
    base: String,
    username: String,
    password: String,
    /// WebUI API >= 2.11 (qBittorrent 5) renamed pause/resume to stop/start.
    stop_start: bool,
}

impl Client {
    pub fn new(base: &str, username: &str, password: &str, tls: &Tls) -> Result<Self> {
        let mut builder = Http::builder().cookie_store(true).timeout(Duration::from_secs(10));
        if tls.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        } else if let Some(path) = &tls.ca_cert {
            builder = builder.tls_backend_preconfigured(crate::tls::config_trusting(path)?);
        }
        let http = builder.build().context("setting up the HTTP client")?;
        Ok(Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
            stop_start: false,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v2/{}", self.base, path)
    }

    pub fn login(&mut self) -> Result<()> {
        let resp = self
            .http
            .post(self.url("auth/login"))
            .header("Referer", &self.base)
            .form(&[("username", &self.username), ("password", &self.password)])
            .send()
            .with_context(|| format!("cannot reach {}", self.base))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!("login failed: HTTP {status} {}", body.trim());
        }
        if body.trim() == "Fails." {
            bail!("login failed: bad username or password");
        }

        let version = self.request(|c| c.http.get(c.url("app/webapiVersion")))?.text()?;
        self.stop_start = version_at_least(version.trim(), (2, 11));
        Ok(())
    }

    /// Send a request, logging in again once if the session has expired.
    fn request(&mut self, build: impl Fn(&Self) -> RequestBuilder) -> Result<Response> {
        let mut resp = build(self).send()?;
        if resp.status() == StatusCode::FORBIDDEN {
            self.login()?;
            resp = build(self).send()?;
        }
        if !resp.status().is_success() {
            bail!("HTTP {}", resp.status());
        }
        Ok(resp)
    }

    fn post_hashes(&mut self, path: &str, hashes: &[String], extra: &[(&str, &str)]) -> Result<()> {
        let joined = hashes.join("|");
        self.request(|c| {
            let mut form = vec![("hashes", joined.as_str())];
            form.extend_from_slice(extra);
            c.http.post(c.url(path)).form(&form)
        })?;
        Ok(())
    }

    pub fn torrents(&mut self) -> Result<Vec<Torrent>> {
        Ok(self.request(|c| c.http.get(c.url("torrents/info")))?.json()?)
    }

    /// Categories sorted by name. The API returns an object keyed by name.
    pub fn categories(&mut self) -> Result<Vec<CategoryInfo>> {
        let map: std::collections::BTreeMap<String, CategoryInfo> =
            self.request(|c| c.http.get(c.url("torrents/categories")))?.json()?;
        Ok(map.into_values().collect())
    }

    pub fn preferences(&mut self) -> Result<Prefs> {
        Ok(self.request(|c| c.http.get(c.url("app/preferences")))?.json()?)
    }

    pub fn transfer(&mut self) -> Result<Transfer> {
        Ok(self.request(|c| c.http.get(c.url("transfer/info")))?.json()?)
    }

    pub fn pause(&mut self, hashes: &[String]) -> Result<()> {
        let path = if self.stop_start { "torrents/stop" } else { "torrents/pause" };
        self.post_hashes(path, hashes, &[])
    }

    pub fn resume(&mut self, hashes: &[String]) -> Result<()> {
        let path = if self.stop_start { "torrents/start" } else { "torrents/resume" };
        self.post_hashes(path, hashes, &[])
    }

    pub fn delete(&mut self, hashes: &[String], delete_files: bool) -> Result<()> {
        let flag = if delete_files { "true" } else { "false" };
        self.post_hashes("torrents/delete", hashes, &[("deleteFiles", flag)])
    }

    /// An empty `category` removes the torrents from their category.
    pub fn set_category(&mut self, hashes: &[String], category: &str) -> Result<()> {
        self.post_hashes("torrents/setCategory", hashes, &[("category", category)])
    }

    pub fn create_category(&mut self, name: &str) -> Result<()> {
        self.request(|c| c.http.post(c.url("torrents/createCategory")).form(&[("category", name), ("savePath", "")]))?;
        Ok(())
    }

    pub fn tags(&mut self) -> Result<Vec<String>> {
        let mut tags: Vec<String> = self.request(|c| c.http.get(c.url("torrents/tags")))?.json()?;
        tags.sort_by_key(|t| t.to_lowercase());
        Ok(tags)
    }

    /// qBittorrent creates the tag if it doesn't exist yet.
    pub fn add_tag(&mut self, hashes: &[String], tag: &str) -> Result<()> {
        self.post_hashes("torrents/addTags", hashes, &[("tags", tag)])
    }

    pub fn remove_tag(&mut self, hashes: &[String], tag: &str) -> Result<()> {
        self.post_hashes("torrents/removeTags", hashes, &[("tags", tag)])
    }

    pub fn recheck(&mut self, hashes: &[String]) -> Result<()> {
        self.post_hashes("torrents/recheck", hashes, &[])
    }

    pub fn reannounce(&mut self, hashes: &[String]) -> Result<()> {
        self.post_hashes("torrents/reannounce", hashes, &[])
    }

    pub fn add(&mut self, t: &NewTorrent) -> Result<()> {
        let path = std::path::Path::new(&t.source);
        let file = if path.is_file() {
            let bytes = std::fs::read(path).with_context(|| format!("reading {}", t.source))?;
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            Some((bytes, name))
        } else {
            None
        };
        self.request(|c| {
            let mut form = match &file {
                Some((bytes, name)) => {
                    let part = multipart::Part::bytes(bytes.clone())
                        .file_name(name.clone())
                        .mime_str("application/x-bittorrent")
                        .expect("valid mime");
                    multipart::Form::new().part("torrents", part)
                }
                None => multipart::Form::new().text("urls", t.source.clone()),
            };
            if let Some(cat) = &t.category {
                form = form.text("category", cat.clone());
            }
            if !t.save_path.is_empty() {
                form = form.text("savepath", t.save_path.clone());
            }
            c.http.post(c.url("torrents/add")).multipart(form)
        })?;
        Ok(())
    }
}

fn version_at_least(version: &str, (major, minor): (u32, u32)) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let v = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    v >= (major, minor)
}

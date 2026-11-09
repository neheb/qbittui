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

/// One file inside a torrent, from `torrents/files`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TorrentFile {
    pub name: String,
    pub size: i64,
    pub progress: f64,
    /// 0 = do not download, 1 = normal, 6 = high, 7 = maximal.
    pub priority: i64,
}

/// A torrent to add: magnet link, URL or local .torrent file, plus where it goes.
#[cfg_attr(test, derive(Debug, PartialEq))]
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
        let mut builder = Http::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(10));
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

        let version = self
            .request(|c| c.http.get(c.url("app/webapiVersion")))?
            .text()?;
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
        Ok(self
            .request(|c| c.http.get(c.url("torrents/info")))?
            .json()?)
    }

    /// Categories sorted by name. The API returns an object keyed by name.
    pub fn categories(&mut self) -> Result<Vec<CategoryInfo>> {
        let map: std::collections::BTreeMap<String, CategoryInfo> = self
            .request(|c| c.http.get(c.url("torrents/categories")))?
            .json()?;
        Ok(map.into_values().collect())
    }

    pub fn files(&mut self, hash: &str) -> Result<Vec<TorrentFile>> {
        Ok(self
            .request(|c| c.http.get(c.url("torrents/files")).query(&[("hash", hash)]))?
            .json()?)
    }

    /// `ids` are file indices as listed by `files`; priority 0 skips the files.
    pub fn file_prio(&mut self, hash: &str, ids: &[usize], priority: i64) -> Result<()> {
        let ids = ids
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join("|");
        let priority = priority.to_string();
        self.request(|c| {
            c.http.post(c.url("torrents/filePrio")).form(&[
                ("hash", hash),
                ("id", &ids),
                ("priority", &priority),
            ])
        })?;
        Ok(())
    }

    pub fn preferences(&mut self) -> Result<Prefs> {
        Ok(self
            .request(|c| c.http.get(c.url("app/preferences")))?
            .json()?)
    }

    pub fn transfer(&mut self) -> Result<Transfer> {
        Ok(self
            .request(|c| c.http.get(c.url("transfer/info")))?
            .json()?)
    }

    pub fn pause(&mut self, hashes: &[String]) -> Result<()> {
        let path = if self.stop_start {
            "torrents/stop"
        } else {
            "torrents/pause"
        };
        self.post_hashes(path, hashes, &[])
    }

    pub fn resume(&mut self, hashes: &[String]) -> Result<()> {
        let path = if self.stop_start {
            "torrents/start"
        } else {
            "torrents/resume"
        };
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
        self.request(|c| {
            c.http
                .post(c.url("torrents/createCategory"))
                .form(&[("category", name), ("savePath", "")])
        })?;
        Ok(())
    }

    pub fn tags(&mut self) -> Result<Vec<String>> {
        let mut tags: Vec<String> = self
            .request(|c| c.http.get(c.url("torrents/tags")))?
            .json()?;
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
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            Some((bytes, name))
        } else {
            None
        };
        self.request(|c| {
            let mut form = match &file {
                Some((bytes, name)) => {
                    #[expect(clippy::expect_used, reason = "constant MIME type always parses")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Seen, serve};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Receiver;

    pub(crate) fn client(base: &str, tls: &Tls) -> Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // A trailing slash on the URL must not double up in request paths.
        Client::new(&format!("{base}/"), "admin", "s3cret&x", tls).unwrap()
    }

    /// A WebUI that accepts any login and reports `version` as its API version.
    fn webui(version: &'static str) -> (String, Receiver<Seen>) {
        serve(None, move |_, path| match path {
            "/api/v2/auth/login" => (200, "Ok."),
            "/api/v2/app/webapiVersion" => (200, version),
            _ => (200, ""),
        })
    }

    fn requests(rx: &Receiver<Seen>) -> Vec<String> {
        rx.try_iter()
            .map(|(method, path, body)| format!("{method} {path} {body}").trim_end().to_string())
            .collect()
    }

    #[test]
    fn qbittorrent_5_uses_stop_and_start() {
        let (base, rx) = webui("2.11.4");
        let mut c = client(&base, &Tls::default());
        c.login().unwrap();
        c.pause(&["a".into(), "b".into()]).unwrap();
        c.resume(&["a".into()]).unwrap();
        assert_eq!(
            requests(&rx),
            [
                "POST /api/v2/auth/login username=admin&password=s3cret%26x",
                "GET /api/v2/app/webapiVersion",
                "POST /api/v2/torrents/stop hashes=a%7Cb",
                "POST /api/v2/torrents/start hashes=a",
            ]
        );
    }

    #[test]
    fn qbittorrent_4_uses_pause_and_resume() {
        let (base, rx) = webui("2.8.19");
        let mut c = client(&base, &Tls::default());
        c.login().unwrap();
        c.pause(&["a".into()]).unwrap();
        c.resume(&["a".into()]).unwrap();
        let reqs = requests(&rx);
        assert_eq!(
            reqs[2..],
            [
                "POST /api/v2/torrents/pause hashes=a",
                "POST /api/v2/torrents/resume hashes=a"
            ]
        );
    }

    #[test]
    fn torrent_actions_send_their_parameters() {
        let (base, rx) = webui("2.11.4");
        let mut c = client(&base, &Tls::default());
        let h = ["a".to_string()];
        c.delete(&h, true).unwrap();
        c.set_category(&h, "TV").unwrap();
        c.create_category("New Cat").unwrap();
        c.add_tag(&h, "hd").unwrap();
        c.remove_tag(&h, "hd").unwrap();
        c.file_prio("a", &[0, 2], 6).unwrap();
        c.recheck(&h).unwrap();
        c.reannounce(&h).unwrap();
        assert_eq!(
            requests(&rx),
            [
                "POST /api/v2/torrents/delete hashes=a&deleteFiles=true",
                "POST /api/v2/torrents/setCategory hashes=a&category=TV",
                "POST /api/v2/torrents/createCategory category=New+Cat&savePath=",
                "POST /api/v2/torrents/addTags hashes=a&tags=hd",
                "POST /api/v2/torrents/removeTags hashes=a&tags=hd",
                "POST /api/v2/torrents/filePrio hash=a&id=0%7C2&priority=6",
                "POST /api/v2/torrents/recheck hashes=a",
                "POST /api/v2/torrents/reannounce hashes=a",
            ]
        );
    }

    #[test]
    fn adding_a_magnet_sends_urls_and_options() {
        let (base, rx) = webui("2.11.4");
        let mut c = client(&base, &Tls::default());
        c.add(&NewTorrent {
            source: "magnet:?xt=urn:btih:abc".into(),
            category: Some("TV".into()),
            save_path: "/srv/tv".into(),
        })
        .unwrap();
        let (method, path, body) = rx.try_recv().unwrap();
        assert_eq!(
            (method.as_str(), path.as_str()),
            ("POST", "/api/v2/torrents/add")
        );
        for part in [
            "name=\"urls\"\r\n\r\nmagnet:?xt=urn:btih:abc\r\n",
            "name=\"category\"\r\n\r\nTV\r\n",
            "name=\"savepath\"\r\n\r\n/srv/tv\r\n",
        ] {
            assert!(body.contains(part), "{part:?} missing from {body}");
        }
    }

    #[test]
    fn adding_a_file_uploads_it() {
        let dir = crate::testutil::TempDir::new("upload");
        let path = dir.path().join("x.torrent");
        std::fs::write(&path, "d8:announce0:e").unwrap();
        let (base, rx) = webui("2.11.4");
        let mut c = client(&base, &Tls::default());
        c.add(&NewTorrent {
            source: path.display().to_string(),
            category: None,
            save_path: String::new(),
        })
        .unwrap();
        let (_, _, body) = rx.try_recv().unwrap();
        assert!(body.contains("name=\"torrents\"; filename=\"x.torrent\""));
        assert!(body.contains("Content-Type: application/x-bittorrent\r\n\r\nd8:announce0:e"));
        assert!(!body.contains("category") && !body.contains("savepath"));
    }

    #[test]
    fn bad_credentials_are_reported() {
        let (base, _rx) = serve(None, |_, _| (200, "Fails."));
        let err = client(&base, &Tls::default()).login().unwrap_err();
        assert_eq!(err.to_string(), "login failed: bad username or password");

        let (base, _rx) = serve(None, |_, _| (403, "banned"));
        let err = client(&base, &Tls::default()).login().unwrap_err();
        assert_eq!(err.to_string(), "login failed: HTTP 403 Forbidden banned");
    }

    #[test]
    fn an_expired_session_logs_in_again() {
        let calls = AtomicUsize::new(0);
        let (base, rx) = serve(None, move |_, path| match path {
            "/api/v2/torrents/info" if calls.fetch_add(1, Ordering::Relaxed) == 0 => (403, ""),
            "/api/v2/torrents/info" => (
                200,
                r#"[{"hash":"h1","name":"One","state":"stoppedDL","size":5,"future_field":[1]}]"#,
            ),
            "/api/v2/auth/login" => (200, "Ok."),
            "/api/v2/app/webapiVersion" => (200, "2.11.4"),
            _ => (404, ""),
        });
        let mut c = client(&base, &Tls::default());
        let torrents = c.torrents().unwrap();
        assert_eq!(torrents.len(), 1);
        assert_eq!((torrents[0].name.as_str(), torrents[0].size), ("One", 5));
        assert!(torrents[0].is_paused());
        assert_eq!(torrents[0].ratio, 0.0, "missing fields default");
        let paths: Vec<String> = rx.try_iter().map(|(_, path, _)| path).collect();
        assert_eq!(
            paths,
            [
                "/api/v2/torrents/info",
                "/api/v2/auth/login",
                "/api/v2/app/webapiVersion",
                "/api/v2/torrents/info"
            ]
        );
    }

    #[test]
    fn other_http_errors_are_reported() {
        let (base, _rx) = serve(None, |_, _| (500, ""));
        let err = client(&base, &Tls::default()).transfer().unwrap_err();
        assert_eq!(err.to_string(), "HTTP 500 Internal Server Error");
    }

    #[test]
    fn categories_and_tags_come_back_sorted() {
        let (base, _rx) = serve(None, |_, path| match path {
            "/api/v2/torrents/categories" => (
                200,
                r#"{"tv":{"name":"tv","savePath":"/tv"},"Movies":{"name":"Movies","savePath":""}}"#,
            ),
            "/api/v2/torrents/tags" => (200, r#"["b","A","c"]"#),
            _ => (404, ""),
        });
        let mut c = client(&base, &Tls::default());
        let cats = c.categories().unwrap();
        let cats: Vec<_> = cats
            .iter()
            .map(|c| (c.name.as_str(), c.save_path.as_str()))
            .collect();
        assert_eq!(cats, [("Movies", ""), ("tv", "/tv")]);
        assert_eq!(c.tags().unwrap(), ["A", "b", "c"]);
    }

    #[test]
    fn version_comparison() {
        assert!(version_at_least("2.11.4", (2, 11)));
        assert!(version_at_least("3.0", (2, 11)));
        assert!(!version_at_least("2.10.9", (2, 11)));
        assert!(!version_at_least("2", (2, 11)));
        assert!(!version_at_least("", (2, 11)));
    }
}

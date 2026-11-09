//! Helpers shared by the unit tests.

use std::path::{Path, PathBuf};

/// A fresh directory under the system temp dir, removed again on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("qbittui-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Create an empty file (and its parent directories) at `rel`.
    pub fn touch(&self, rel: &str) -> PathBuf {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        path
    }

    /// The directory as a string ending in the platform's separator, like the add form shows it.
    pub fn prefix(&self) -> String {
        format!("{}{}", self.0.display(), std::path::MAIN_SEPARATOR)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A request the fake WebUI received: method, path with query, and body.
pub type Seen = (String, String, String);

/// A fake qBittorrent WebUI on a local port, answering each request with `respond(method, path)`
/// as (status, body), over HTTPS when `tls` is given. Returns its base URL and every request seen.
pub fn serve(
    tls: Option<std::sync::Arc<rustls::ServerConfig>>,
    respond: impl Fn(&str, &str) -> (u16, &'static str) + Send + 'static,
) -> (String, std::sync::mpsc::Receiver<Seen>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let scheme = if tls.is_some() { "https" } else { "http" };
    let base = format!("{scheme}://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            // A failed TLS handshake sees no request; keep serving.
            match &tls {
                Some(cfg) => {
                    let conn = rustls::ServerConnection::new(cfg.clone()).unwrap();
                    answer(rustls::StreamOwned::new(conn, stream), &respond, &tx);
                }
                None => {
                    answer(stream, &respond, &tx);
                }
            }
        }
    });
    (base, rx)
}

/// Read one HTTP/1.1 request from `stream`, report it on `seen`, then answer it and close.
/// Reporting first means a test sees the request by the time the client has its response.
fn answer<S: std::io::Read + std::io::Write>(
    mut stream: S,
    respond: &impl Fn(&str, &str) -> (u16, &'static str),
    seen: &std::sync::mpsc::Sender<Seen>,
) -> Option<()> {
    use std::io::{BufRead, Read};
    let mut reader = std::io::BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut len = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            len = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    drop(reader);
    let (status, reply) = respond(&method, &path);
    let body = String::from_utf8_lossy(&body).into_owned();
    // Tests that don't look at requests drop the receiver; answer anyway.
    let _ = seen.send((method, path, body));
    write!(
        stream,
        "HTTP/1.1 {status} Status\r\nContent-Length: {}\r\nSet-Cookie: SID=test; path=/\r\n\
         Connection: close\r\n\r\n{reply}",
        reply.len()
    )
    .ok()?;
    stream.flush().ok()
}

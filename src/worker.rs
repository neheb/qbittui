//! Background thread that owns the HTTP client so the UI never blocks on the network.

use crate::api::{CategoryInfo, Client, NewTorrent, Prefs, Torrent, Transfer};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

pub enum Cmd {
    Refresh,
    /// Re-read preferences (default save path may have changed).
    Prefs,
    Pause(Vec<String>),
    Resume(Vec<String>),
    Delete(Vec<String>, bool),
    Recheck(Vec<String>),
    Reannounce(Vec<String>),
    Add(NewTorrent),
    /// Hashes, category (empty = none), and whether it must be created first.
    SetCategory(Vec<String>, String, bool),
    /// Hashes, tag, and whether to add (true) or remove it.
    Tag(Vec<String>, String, bool),
}

pub enum Msg {
    /// Torrents, transfer stats, categories, tag names.
    Data(Vec<Torrent>, Transfer, Vec<CategoryInfo>, Vec<String>),
    Prefs(Prefs),
    Info(String),
    Error(String),
}

pub fn run(mut client: Client, interval: Duration, rx: Receiver<Cmd>, tx: Sender<Msg>) {
    let mut logged_in = false;
    loop {
        let cmd = match rx.recv_timeout(interval) {
            Ok(cmd) => Some(cmd),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };

        if !logged_in {
            match client.login() {
                Ok(()) => {
                    logged_in = true;
                    if let Ok(p) = client.preferences() {
                        let _ = tx.send(Msg::Prefs(p));
                    }
                }
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("{e:#}")));
                    continue;
                }
            }
        }

        if let Some(cmd) = cmd {
            let result = match cmd {
                Cmd::Refresh => Ok(None),
                Cmd::Prefs => client.preferences().map(|p| {
                    let _ = tx.send(Msg::Prefs(p));
                    None
                }),
                Cmd::Pause(h) => client.pause(&h).map(|_| Some(format!("Paused {}", n(&h)))),
                Cmd::Resume(h) => client.resume(&h).map(|_| Some(format!("Resumed {}", n(&h)))),
                Cmd::Delete(h, files) => client.delete(&h, files).map(|_| {
                    let what = if files { " (with data)" } else { "" };
                    Some(format!("Deleted {}{what}", n(&h)))
                }),
                Cmd::Recheck(h) => client.recheck(&h).map(|_| Some(format!("Rechecking {}", n(&h)))),
                Cmd::Reannounce(h) => {
                    client.reannounce(&h).map(|_| Some(format!("Reannounced {}", n(&h))))
                }
                Cmd::SetCategory(h, cat, create) => (|| {
                    if create {
                        client.create_category(&cat)?;
                    }
                    client.set_category(&h, &cat)?;
                    Ok(Some(if cat.is_empty() {
                        format!("Removed category from {}", n(&h))
                    } else {
                        format!("Moved {} to {cat}", n(&h))
                    }))
                })(),
                Cmd::Tag(h, tag, true) => client.add_tag(&h, &tag).map(|_| Some(format!("Tagged {} with {tag}", n(&h)))),
                Cmd::Tag(h, tag, false) => {
                    client.remove_tag(&h, &tag).map(|_| Some(format!("Removed tag {tag} from {}", n(&h))))
                }
                Cmd::Add(t) => client.add(&t).map(|_| Some("Torrent added".to_string())),
            };
            match result {
                Ok(Some(info)) => {
                    let _ = tx.send(Msg::Info(info));
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(Msg::Error(format!("{e:#}")));
                }
            }
        }

        let mut fetch = || -> anyhow::Result<_> {
            Ok((client.torrents()?, client.transfer()?, client.categories()?, client.tags()?))
        };
        match fetch() {
            Ok((torrents, transfer, categories, tags)) => {
                if tx.send(Msg::Data(torrents, transfer, categories, tags)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Msg::Error(format!("{e:#}")));
            }
        }
    }
}

fn n(hashes: &[String]) -> String {
    match hashes.len() {
        1 => "1 torrent".into(),
        k => format!("{k} torrents"),
    }
}

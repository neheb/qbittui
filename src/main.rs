mod api;
mod app;
mod config;
mod ui;
mod worker;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyEventKind,
};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// A terminal UI for qBittorrent's WebUI.
///
/// Settings come from flags, then environment variables, then the config file
/// (~/.config/qbittui/config.toml), then built-in defaults.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// WebUI address [default: http://localhost:8080]
    #[arg(short, long, env = "QBT_URL")]
    url: Option<String>,
    /// WebUI username [default: admin]
    #[arg(short = 'U', long, env = "QBT_USERNAME")]
    username: Option<String>,
    /// WebUI password
    #[arg(short = 'P', long, env = "QBT_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Refresh interval in seconds [default: 2]
    #[arg(short, long)]
    interval: Option<f64>,
    /// Directory the add dialog starts in when picking a .torrent file [default: ~/Downloads/]
    #[arg(short, long, env = "QBT_TORRENT_DIR")]
    torrent_dir: Option<String>,
    /// Disable mouse support (keeps the terminal's own text selection)
    #[arg(long)]
    no_mouse: bool,
    /// Config file path
    #[arg(short, long, env = "QBITTUI_CONFIG")]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    // reqwest is built without a bundled crypto provider; use ring (it cross-compiles everywhere).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cfg = match (&args.config, config::default_path()) {
        (Some(path), _) => config::load(path, true)?,
        (None, Some(path)) => config::load(&path, false)?,
        (None, None) => config::Config::default(),
    };

    let url = args.url.or(cfg.url).unwrap_or_else(|| "http://localhost:8080".into());
    let username = args.username.or(cfg.username).unwrap_or_else(|| "admin".into());
    let password = match (args.password.or(cfg.password), cfg.password_command) {
        (Some(p), _) => p,
        (None, Some(cmd)) => config::run_password_command(&cmd)?,
        (None, None) => String::new(),
    };
    let interval = args.interval.or(cfg.interval).unwrap_or(2.0);
    let torrent_dir = args.torrent_dir.or(cfg.torrent_dir).unwrap_or_else(|| "~/Downloads/".into());
    let mouse = !args.no_mouse && cfg.mouse.unwrap_or(true);

    let client = api::Client::new(&url, &username, &password)?;
    let interval = Duration::from_secs_f64(interval.max(0.25));

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (msg_tx, msg_rx) = mpsc::channel();
    std::thread::spawn(move || worker::run(client, interval, cmd_rx, msg_tx));
    cmd_tx.send(worker::Cmd::Refresh)?;

    let mut app = app::App::new(url, torrent_dir, cmd_tx);
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)?;
    if mouse {
        crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    }
    let result = (|| -> Result<()> {
        while !app.quit {
            while let Ok(msg) = msg_rx.try_recv() {
                app.handle_msg(msg);
            }
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
                    Event::Mouse(ev) => app.handle_mouse(ev),
                    Event::Paste(text) => app.handle_paste(&text),
                    _ => {}
                }
            }
        }
        Ok(())
    })();
    if mouse {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    }
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

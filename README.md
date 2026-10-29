# qbittui

A terminal UI for qBittorrent, talking to its WebUI API (works with qBittorrent 4.x and 5.x).

## Install

Static Linux binaries for amd64, arm64, armv7 and armv6 are attached to each
[GitHub release](https://github.com/neheb/qbittui/releases). Or build from source with `cargo install --path .`.

## Usage

Enable the WebUI in qBittorrent (Tools → Options → Web UI), then:

```sh
qbittui --url http://localhost:8080 -U admin -P secret
```

Or via environment: `QBT_URL`, `QBT_USERNAME`, `QBT_PASSWORD`. `--interval` sets the refresh rate (seconds, default 2).

## Config file

Settings can also live in `~/.config/qbittui/config.toml` (respects `$XDG_CONFIG_HOME`; use `--config` or
`QBITTUI_CONFIG` for another path). Flags and environment variables take precedence over it. Every key is optional:

```toml
url = "http://localhost:8080"
username = "admin"
password = "secret"                          # or, to keep it out of the file:
password_command = "pass show qbittorrent"   # stdout of this shell command is the password
interval = 2                                 # refresh, seconds
torrent_dir = "~/Downloads/"                 # where the add dialog starts
mouse = true
```

Unknown keys are reported as errors, so typos don't go unnoticed.

## Adding torrents

`a` opens a form with three fields:

- **Source**: starts in `~/Downloads/` (`--torrent-dir` / `QBT_TORRENT_DIR` to change). Tab completes
  directories and `.torrent` files like a shell: the first Tab fills the common prefix, further Tabs cycle
  through matches, → accepts the current match. Pasting a magnet link or URL replaces the prefilled directory.
- **Category**: ←/→ to cycle; preselected from the sidebar.
- **Save path**: shows, greyed out, where qBittorrent will put the torrent: its default save path, or the
  category's path when Automatic Torrent Management is on. That path isn't sent, so qBittorrent's own logic
  applies. Type a path to override it (`~` is expanded on this machine), or press → to copy the shown path in
  and edit it.

## Keys

| Key | Action |
| --- | --- |
| `j`/`k` | next / previous category |
| `J`/`K` | next / previous tag |
| ↑/↓, PgUp/PgDn, Home/End | move in the torrent list |
| ←/→, `tab`, `1`-`6` | state filter: all / downloading / seeding / paused / active / errored |
| `space` | mark torrent (actions apply to all marked) |
| `p` | pause / resume |
| `d` | delete (`y` keep data, `D` delete data) |
| `C` | set category of the torrent(s); type to filter, or type a new name to create it |
| `T` | tag / untag the torrent(s): a checklist of tags, toggles apply immediately; type a new name to create one |
| `c` / `R` | force recheck / reannounce |
| `a` | add a torrent (see above) |
| `/` | search by name |
| `s` / `S` | cycle sort column / reverse |
| `r` | refresh now |
| `?` | help |
| `q` | quit |

## Mouse

Click a category, tag, filter tab, or torrent to select it; right-click a torrent to mark it; the wheel scrolls
whichever pane it is over. Mouse capture disables the terminal's own text selection (most terminals bypass it
with Shift+drag); run with `--no-mouse` to turn it off.

use crate::api::{CategoryInfo, NewTorrent, Prefs, Torrent, TorrentFile, Transfer};
use crate::worker::{Cmd, Msg};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::{ListState, TableState};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Downloading,
    Seeding,
    Paused,
    Active,
    Errored,
}

impl Filter {
    pub const ALL: [Filter; 6] = [
        Filter::All,
        Filter::Downloading,
        Filter::Seeding,
        Filter::Paused,
        Filter::Active,
        Filter::Errored,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Downloading => "Downloading",
            Filter::Seeding => "Seeding",
            Filter::Paused => "Paused",
            Filter::Active => "Active",
            Filter::Errored => "Errored",
        }
    }

    pub fn matches(self, t: &Torrent) -> bool {
        let s = t.state.as_str();
        match self {
            Filter::All => true,
            Filter::Downloading => matches!(
                s,
                "downloading"
                    | "stalledDL"
                    | "queuedDL"
                    | "forcedDL"
                    | "metaDL"
                    | "forcedMetaDL"
                    | "checkingDL"
            ),
            Filter::Seeding => matches!(
                s,
                "uploading" | "stalledUP" | "queuedUP" | "forcedUP" | "checkingUP"
            ),
            Filter::Paused => t.is_paused(),
            Filter::Active => t.dlspeed > 0 || t.upspeed > 0,
            Filter::Errored => matches!(s, "error" | "missingFiles"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Added,
    Name,
    Size,
    Progress,
    Down,
    Up,
    Ratio,
    Eta,
}

impl Sort {
    const ORDER: [Sort; 8] = [
        Sort::Added,
        Sort::Name,
        Sort::Size,
        Sort::Progress,
        Sort::Down,
        Sort::Up,
        Sort::Ratio,
        Sort::Eta,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Added => "added",
            Sort::Name => "name",
            Sort::Size => "size",
            Sort::Progress => "progress",
            Sort::Down => "down",
            Sort::Up => "up",
            Sort::Ratio => "ratio",
            Sort::Eta => "eta",
        }
    }

    fn next(self) -> Sort {
        let i = Self::ORDER.iter().position(|&s| s == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }
}

/// Sidebar entry: everything, torrents with no category, or one named category.
#[derive(Clone, PartialEq, Eq)]
pub enum Category {
    All,
    None,
    Named(String),
}

impl Category {
    pub fn matches(&self, t: &Torrent) -> bool {
        match self {
            Category::All => true,
            Category::None => t.category.is_empty(),
            Category::Named(name) => &t.category == name,
        }
    }
}

/// Tags sidebar entry: everything, torrents with no tags, or torrents carrying one tag.
#[derive(Clone, PartialEq, Eq)]
pub enum TagFilter {
    All,
    None,
    Named(String),
}

impl TagFilter {
    pub fn matches(&self, t: &Torrent) -> bool {
        match self {
            TagFilter::All => true,
            TagFilter::None => torrent_tags(t).next().is_none(),
            TagFilter::Named(name) => torrent_tags(t).any(|x| x == name),
        }
    }
}

/// Screen regions from the last draw, used to map mouse events.
#[derive(Default)]
pub struct HitAreas {
    pub sidebar: Rect,
    pub tags: Rect,
    pub table: Rect,
    /// One rect per state filter tab.
    pub tabs: Vec<Rect>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddField {
    Source,
    Category,
    SavePath,
}

pub struct AddForm {
    pub source: String,
    pub category: Option<String>,
    /// Empty means qBittorrent decides (its default, or the category's path).
    pub save_path: String,
    pub field: AddField,
    /// Directory the source field was prefilled with; a paste replaces it rather than appending.
    prefill: String,
    /// Pending Tab completions for the source field and which one is showing.
    pub completions: Vec<String>,
    pub completion_idx: Option<usize>,
}

impl AddForm {
    fn text_field(&mut self) -> Option<&mut String> {
        self.completions.clear();
        self.completion_idx = None;
        match self.field {
            AddField::Source => Some(&mut self.source),
            AddField::SavePath => Some(&mut self.save_path),
            AddField::Category => None,
        }
    }

    /// Shell-style completion of a local path in the source field.
    /// The first Tab extends to the longest common prefix; further Tabs cycle through matches.
    fn complete_source(&mut self) {
        if let Some(i) = self.completion_idx {
            let next = (i + 1) % self.completions.len();
            self.completion_idx = Some(next);
            self.source = self.completions[next].clone();
            return;
        }
        let matches = path_completions(&self.source);
        match matches.len() {
            0 => {}
            1 => {
                self.source = matches[0].clone();
                self.completions.clear();
            }
            _ => {
                let lcp = common_prefix(&matches);
                if lcp.len() > self.source.len() {
                    self.source = lcp;
                } else {
                    self.completion_idx = Some(0);
                    self.source = matches[0].clone();
                }
                self.completions = matches;
            }
        }
    }
}

fn looks_like_path(s: &str) -> bool {
    s.starts_with(['.', '~']) || std::path::Path::new(s).has_root()
}

/// Directories and .torrent files matching `input`, written the way the user typed them (`~` kept).
fn path_completions(input: &str) -> Vec<String> {
    let (dir, prefix) = match input.rfind(std::path::is_separator) {
        Some(i) => input.split_at(i + 1),
        None => ("", input),
    };
    let read_dir = if dir.is_empty() {
        ".".to_string()
    } else {
        expand_tilde(dir)
    };
    let Ok(entries) = std::fs::read_dir(&read_dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                return None;
            }
            // Follow symlinks so linked directories complete as directories.
            let is_dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
            if is_dir {
                Some(format!("{dir}{name}{}", std::path::MAIN_SEPARATOR))
            } else if name.to_lowercase().ends_with(".torrent") {
                Some(format!("{dir}{name}"))
            } else {
                None
            }
        })
        .collect();
    out.sort();
    out
}

fn common_prefix(items: &[String]) -> String {
    let mut prefix = items[0].as_str();
    for s in &items[1..] {
        let len = prefix
            .char_indices()
            .zip(s.chars())
            .find(|((_, a), b)| a != b)
            .map_or(prefix.len().min(s.len()), |((i, _), _)| i);
        prefix = &prefix[..len];
    }
    prefix.to_string()
}

#[derive(Clone, PartialEq, Eq)]
pub enum PickOption {
    None,
    Existing(String),
    Create(String),
}

/// Category picker for moving torrents; typing filters, an unknown name offers to create it.
pub struct Picker {
    pub targets: Vec<String>,
    pub filter: String,
    pub idx: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Check {
    All,
    Some,
    None,
}

/// Tag checklist for the target torrents. Toggles apply immediately; the picker tracks the
/// result locally so it doesn't have to wait for the next refresh.
pub struct TagPicker {
    pub targets: Vec<String>,
    pub filter: String,
    pub idx: usize,
    tags: Vec<String>,
    state: HashMap<String, Check>,
}

impl TagPicker {
    /// Matching tags, plus a "create" entry (as `Err`) when the filter names a new tag.
    pub fn options(&self) -> Vec<Result<String, String>> {
        let needle = self.filter.trim().to_lowercase();
        let mut out: Vec<Result<String, String>> = self
            .tags
            .iter()
            .filter(|t| t.to_lowercase().contains(&needle))
            .cloned()
            .map(Ok)
            .collect();
        if !needle.is_empty() && !self.tags.iter().any(|t| t.to_lowercase() == needle) {
            out.push(Err(self.filter.trim().to_string()));
        }
        out
    }

    pub fn check(&self, tag: &str) -> Check {
        self.state.get(tag).copied().unwrap_or(Check::None)
    }
}

/// File list of one torrent, refreshed by the worker while open.
pub struct FilesView {
    pub hash: String,
    pub name: String,
    /// None until the first response arrives. A file's position is its index for the API.
    pub files: Option<Vec<TorrentFile>>,
    /// Folder paths (without trailing '/') the user has folded.
    pub collapsed: HashSet<String>,
    pub table: TableState,
}

/// A visible line of the file tree. `files` holds the indices of every file a folder contains.
pub enum FileRow {
    Folder {
        path: String,
        name: String,
        depth: usize,
        open: bool,
        files: Vec<usize>,
    },
    File {
        idx: usize,
        name: String,
        depth: usize,
    },
}

impl FileRow {
    pub fn depth(&self) -> usize {
        match self {
            FileRow::Folder { depth, .. } | FileRow::File { depth, .. } => *depth,
        }
    }

    pub fn files(&self) -> Vec<usize> {
        match self {
            FileRow::Folder { files, .. } => files.clone(),
            FileRow::File { idx, .. } => vec![*idx],
        }
    }
}

/// qBittorrent's file priorities, lowest first.
pub const FILE_PRIORITIES: [i64; 4] = [0, 1, 6, 7];

impl FilesView {
    /// The file tree flattened for display: folders first, then files, each sorted by name.
    pub fn rows(&self) -> Vec<FileRow> {
        let mut out = Vec::new();
        if let Some(files) = &self.files {
            let items = files
                .iter()
                .enumerate()
                .map(|(i, f)| (i, f.name.as_str()))
                .collect();
            self.walk(items, "", 0, &mut out);
        }
        out
    }

    fn walk(&self, items: Vec<(usize, &str)>, prefix: &str, depth: usize, out: &mut Vec<FileRow>) {
        let mut folders: std::collections::BTreeMap<&str, Vec<(usize, &str)>> = Default::default();
        let mut files = Vec::new();
        for (i, rest) in items {
            match rest.split_once('/') {
                Some((dir, tail)) => folders.entry(dir).or_default().push((i, tail)),
                None => files.push((i, rest)),
            }
        }
        for (name, children) in folders {
            let path = format!("{prefix}{name}");
            let open = !self.collapsed.contains(&path);
            let ids = children.iter().map(|&(i, _)| i).collect();
            out.push(FileRow::Folder {
                path: path.clone(),
                name: name.to_string(),
                depth,
                open,
                files: ids,
            });
            if open {
                self.walk(children, &format!("{path}/"), depth + 1, out);
            }
        }
        files.sort_by_key(|&(_, name)| name);
        out.extend(files.into_iter().map(|(idx, name)| FileRow::File {
            idx,
            name: name.to_string(),
            depth,
        }));
    }

    /// The shared priority of `ids`, or None if they differ.
    pub fn priority(&self, ids: &[usize]) -> Option<i64> {
        let files = self.files.as_ref()?;
        let first = files.get(*ids.first()?)?.priority;
        ids.iter()
            .all(|&i| files[i].priority == first)
            .then_some(first)
    }
}

/// Tags on a torrent; the API joins them with ", ".
pub fn torrent_tags(t: &Torrent) -> impl Iterator<Item = &str> {
    t.tags.split(',').map(str::trim).filter(|s| !s.is_empty())
}

pub enum Mode {
    Normal,
    Search,
    Add(AddForm),
    Recategorize(Picker),
    Tag(TagPicker),
    ConfirmDelete,
    Files(FilesView),
    Help,
}

pub struct App {
    pub url: String,
    pub torrents: Vec<Torrent>,
    /// Indices into `torrents`, filtered and sorted for display.
    pub view: Vec<usize>,
    pub table: TableState,
    selected_hash: Option<String>,
    pub marked: HashSet<String>,
    pub transfer: Transfer,
    pub connected: bool,
    pub filter: Filter,
    /// Sidebar entries: All, Uncategorized, then each category by name.
    pub categories: Vec<Category>,
    pub tags: Vec<String>,
    /// Save path configured per category (empty, relative or absolute).
    category_paths: HashMap<String, String>,
    prefs: Option<Prefs>,
    /// Directory the add form's source field starts in.
    torrent_dir: String,
    pub category: Category,
    pub sidebar: ListState,
    /// Tags sidebar entries: All, Untagged, then each tag by name.
    pub tag_filters: Vec<TagFilter>,
    pub tag: TagFilter,
    pub tag_list: ListState,
    pub hit: HitAreas,
    pub sort: Sort,
    pub reverse: bool,
    pub search: String,
    pub mode: Mode,
    pub status: Option<(String, bool, Instant)>,
    pub page_size: usize,
    pub quit: bool,
    tx: Sender<Cmd>,
}

impl App {
    pub fn new(url: String, torrent_dir: String, tx: Sender<Cmd>) -> Self {
        Self {
            url,
            torrents: Vec::new(),
            view: Vec::new(),
            table: TableState::default(),
            selected_hash: None,
            marked: HashSet::new(),
            transfer: Transfer::default(),
            connected: false,
            filter: Filter::All,
            categories: vec![Category::All, Category::None],
            tags: Vec::new(),
            category_paths: HashMap::new(),
            prefs: None,
            torrent_dir,
            category: Category::All,
            sidebar: ListState::default(),
            tag_filters: vec![TagFilter::All, TagFilter::None],
            tag: TagFilter::All,
            tag_list: ListState::default(),
            hit: HitAreas::default(),
            sort: Sort::Added,
            reverse: true,
            search: String::new(),
            mode: Mode::Normal,
            status: Some(("Connecting…".into(), false, Instant::now())),
            page_size: 10,
            quit: false,
            tx,
        }
    }

    pub fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Prefs(p) => self.prefs = Some(p),
            Msg::Data(torrents, transfer, categories, tags) => {
                self.category_paths = categories
                    .iter()
                    .map(|c| (c.name.clone(), c.save_path.clone()))
                    .collect();
                self.tags = tags;
                if !self.connected {
                    self.status = None;
                }
                self.connected = true;
                self.torrents = torrents;
                self.transfer = transfer;
                self.categories = [Category::All, Category::None]
                    .into_iter()
                    .chain(
                        categories
                            .into_iter()
                            .map(|c: CategoryInfo| Category::Named(c.name)),
                    )
                    .collect();
                if !self.categories.contains(&self.category) {
                    self.category = Category::All;
                }
                self.tag_filters = [TagFilter::All, TagFilter::None]
                    .into_iter()
                    .chain(self.tags.iter().cloned().map(TagFilter::Named))
                    .collect();
                if !self.tag_filters.contains(&self.tag) {
                    self.tag = TagFilter::All;
                }
                let live: HashSet<&str> = self.torrents.iter().map(|t| t.hash.as_str()).collect();
                self.marked.retain(|h| live.contains(h.as_str()));
                self.rebuild_view();
            }
            Msg::Files(hash, files) => {
                if let Mode::Files(v) = &mut self.mode
                    && v.hash == hash
                {
                    v.files = Some(files);
                    let len = v.rows().len();
                    let i = v.table.selected().unwrap_or(0).min(len.saturating_sub(1));
                    v.table.select((len > 0).then_some(i));
                }
            }
            Msg::Info(s) => self.set_status(s, false),
            Msg::Error(s) => {
                self.connected = false;
                self.set_status(s, true);
            }
        }
    }

    fn set_status(&mut self, msg: String, error: bool) {
        self.status = Some((msg, error, Instant::now()));
    }

    pub fn selected(&self) -> Option<&Torrent> {
        self.table
            .selected()
            .and_then(|i| self.view.get(i))
            .map(|&i| &self.torrents[i])
    }

    fn rebuild_view(&mut self) {
        let needle = self.search.to_lowercase();
        let mut view: Vec<usize> = self
            .torrents
            .iter()
            .enumerate()
            .filter(|(_, t)| self.filter.matches(t))
            .filter(|(_, t)| self.category.matches(t))
            .filter(|(_, t)| self.tag.matches(t))
            .filter(|(_, t)| needle.is_empty() || t.name.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();

        let ts = &self.torrents;
        view.sort_by(|&a, &b| {
            let (a, b) = (&ts[a], &ts[b]);
            let ord = match self.sort {
                Sort::Added => a.added_on.cmp(&b.added_on),
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Sort::Size => a.size.cmp(&b.size),
                Sort::Progress => a.progress.total_cmp(&b.progress),
                Sort::Down => a.dlspeed.cmp(&b.dlspeed),
                Sort::Up => a.upspeed.cmp(&b.upspeed),
                Sort::Ratio => a.ratio.total_cmp(&b.ratio),
                Sort::Eta => a.eta.cmp(&b.eta),
            };
            let ord = ord.then_with(|| a.name.cmp(&b.name));
            if self.reverse { ord.reverse() } else { ord }
        });
        self.view = view;

        // Keep the cursor on the same torrent across refreshes and re-sorts.
        let idx = self
            .selected_hash
            .as_ref()
            .and_then(|h| self.view.iter().position(|&i| &self.torrents[i].hash == h));
        match (idx, self.view.len()) {
            (_, 0) => self.table.select(None),
            (Some(i), _) => self.table.select(Some(i)),
            (None, len) => {
                let i = self.table.selected().unwrap_or(0).min(len - 1);
                self.select(i);
            }
        }
    }

    fn select(&mut self, i: usize) {
        if self.view.is_empty() {
            self.table.select(None);
            self.selected_hash = None;
            return;
        }
        let i = i.min(self.view.len() - 1);
        self.table.select(Some(i));
        self.selected_hash = Some(self.torrents[self.view[i]].hash.clone());
    }

    fn move_by(&mut self, delta: isize) {
        let cur = self.table.selected().unwrap_or(0) as isize;
        self.select((cur + delta).max(0) as usize);
    }

    pub fn category_index(&self) -> usize {
        self.categories
            .iter()
            .position(|c| c == &self.category)
            .unwrap_or(0)
    }

    fn move_category(&mut self, delta: isize) {
        let last = self.categories.len() as isize - 1;
        let i = (self.category_index() as isize + delta).clamp(0, last.max(0)) as usize;
        self.category = self.categories[i].clone();
        self.rebuild_view();
    }

    pub fn tag_index(&self) -> usize {
        self.tag_filters
            .iter()
            .position(|t| t == &self.tag)
            .unwrap_or(0)
    }

    fn move_tag(&mut self, delta: isize) {
        let last = self.tag_filters.len() as isize - 1;
        let i = (self.tag_index() as isize + delta).clamp(0, last.max(0)) as usize;
        self.tag = self.tag_filters[i].clone();
        self.rebuild_view();
    }

    fn cycle_filter(&mut self, delta: isize) {
        let n = Filter::ALL.len() as isize;
        let i = Filter::ALL
            .iter()
            .position(|&f| f == self.filter)
            .unwrap_or(0) as isize;
        self.filter = Filter::ALL[(i + delta).rem_euclid(n) as usize];
        self.rebuild_view();
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent) {
        match self.mode {
            Mode::Normal => {}
            Mode::Search => self.mode = Mode::Normal,
            Mode::Help => {
                if let MouseEventKind::Down(_) = ev.kind {
                    self.mode = Mode::Normal;
                }
                return;
            }
            Mode::Files(_) => {
                if let MouseEventKind::ScrollDown | MouseEventKind::ScrollUp = ev.kind {
                    self.files_move(if ev.kind == MouseEventKind::ScrollDown {
                        3
                    } else {
                        -3
                    });
                }
                return;
            }
            Mode::Add(_) | Mode::Recategorize(_) | Mode::Tag(_) | Mode::ConfirmDelete => return,
        }

        let pos = Position::new(ev.column, ev.row);
        let in_sidebar = self.hit.sidebar.contains(pos);
        let in_tags = self.hit.tags.contains(pos);
        let in_table = self.hit.table.contains(pos);
        match ev.kind {
            MouseEventKind::ScrollDown if in_sidebar => self.move_category(1),
            MouseEventKind::ScrollUp if in_sidebar => self.move_category(-1),
            MouseEventKind::ScrollDown if in_tags => self.move_tag(1),
            MouseEventKind::ScrollUp if in_tags => self.move_tag(-1),
            MouseEventKind::ScrollDown if in_table => self.move_by(3),
            MouseEventKind::ScrollUp if in_table => self.move_by(-3),
            MouseEventKind::Down(button) => {
                if let Some(i) = self.hit.tabs.iter().position(|r| r.contains(pos)) {
                    self.filter = Filter::ALL[i];
                    self.rebuild_view();
                } else if in_sidebar {
                    // Inside the border, offset by however far the list has scrolled.
                    let row = (pos.y as usize).checked_sub(self.hit.sidebar.y as usize + 1);
                    if let Some(c) =
                        row.and_then(|r| self.categories.get(r + self.sidebar.offset()))
                    {
                        self.category = c.clone();
                        self.rebuild_view();
                    }
                } else if in_tags {
                    let row = (pos.y as usize).checked_sub(self.hit.tags.y as usize + 1);
                    if let Some(t) =
                        row.and_then(|r| self.tag_filters.get(r + self.tag_list.offset()))
                    {
                        self.tag = t.clone();
                        self.rebuild_view();
                    }
                } else if in_table {
                    // Skip the top border and the header row.
                    let row = (pos.y as usize).checked_sub(self.hit.table.y as usize + 2);
                    let idx = row
                        .map(|r| r + self.table.offset())
                        .filter(|&i| i < self.view.len());
                    if let Some(i) = idx {
                        self.select(i);
                        if button == MouseButton::Right {
                            let h = self.torrents[self.view[i]].hash.clone();
                            if !self.marked.remove(&h) {
                                self.marked.insert(h);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Hashes an action applies to: the marked set, or the cursor row.
    fn targets(&self) -> Vec<String> {
        if !self.marked.is_empty() {
            return self.marked.iter().cloned().collect();
        }
        self.selected()
            .map(|t| vec![t.hash.clone()])
            .unwrap_or_default()
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Help => self.mode = Mode::Normal,
            Mode::Search => match key.code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.mode = Mode::Normal;
                    self.rebuild_view();
                }
                KeyCode::Enter => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    self.search.pop();
                    self.rebuild_view();
                }
                KeyCode::Char(c) => {
                    self.search.push(c);
                    self.rebuild_view();
                }
                _ => {}
            },
            Mode::Add(_) => self.add_key(key),
            Mode::Recategorize(_) => self.picker_key(key),
            Mode::Tag(_) => self.tag_key(key),
            Mode::Files(_) => self.files_key(key),
            Mode::ConfirmDelete => {
                let targets = self.targets();
                match key.code {
                    KeyCode::Char('y') => self.send(Cmd::Delete(targets, false)),
                    KeyCode::Char('D') => self.send(Cmd::Delete(targets, true)),
                    _ => {}
                }
                self.marked.clear();
                self.mode = Mode::Normal;
            }
        }
    }

    /// Where qBittorrent will save a new torrent if no path is given, and why, mirroring its
    /// web UI: the category's path under Automatic Torrent Management, else the default path.
    pub fn effective_save_path(&self, category: Option<&str>) -> Option<(String, &'static str)> {
        let prefs = self.prefs.as_ref()?;
        // The server may run on Windows: keep its separator and recognize drive and UNC paths.
        let sep = if prefs.save_path.contains('\\') {
            '\\'
        } else {
            '/'
        };
        let default = prefs.save_path.trim_end_matches(['/', '\\']);
        match category {
            Some(cat) if prefs.auto_tmm_enabled => {
                let own = self
                    .category_paths
                    .get(cat)
                    .map(String::as_str)
                    .unwrap_or("");
                let path = if own.starts_with(['/', '\\']) || own.as_bytes().get(1) == Some(&b':') {
                    own.to_string()
                } else if own.is_empty() {
                    format!("{default}{sep}{cat}")
                } else {
                    format!("{default}{sep}{own}")
                };
                Some((path, "category"))
            }
            _ => Some((prefs.save_path.clone(), "qBittorrent default")),
        }
    }

    fn open_add_form(&mut self) {
        self.send(Cmd::Prefs);
        let category = match &self.category {
            Category::Named(n) => Some(n.clone()),
            Category::All | Category::None => None,
        };
        let mut dir = self.torrent_dir.clone();
        if !dir.is_empty() && !dir.ends_with(std::path::is_separator) {
            dir.push(std::path::MAIN_SEPARATOR);
        }
        self.mode = Mode::Add(AddForm {
            source: dir.clone(),
            category,
            save_path: String::new(),
            field: AddField::Source,
            prefill: dir,
            completions: Vec::new(),
            completion_idx: None,
        });
    }

    pub fn handle_paste(&mut self, text: &str) {
        let text = text.trim_end_matches(['\r', '\n']);
        match &mut self.mode {
            Mode::Add(form) => {
                // Pasting a magnet link or URL over the untouched prefilled directory replaces it.
                let replace = form.field == AddField::Source && form.source == form.prefill;
                if let Some(buf) = form.text_field() {
                    if replace {
                        buf.clear();
                    }
                    buf.push_str(text);
                }
            }
            Mode::Search => {
                self.search.push_str(text);
                self.rebuild_view();
            }
            Mode::Recategorize(p) => {
                p.filter.push_str(text);
                p.idx = 0;
            }
            Mode::Tag(p) => {
                p.filter.push_str(text);
                p.idx = 0;
            }
            Mode::Normal | Mode::ConfirmDelete | Mode::Files(_) | Mode::Help => {}
        }
    }

    fn add_key(&mut self, key: KeyEvent) {
        let Mode::Add(mut form) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let modified = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                let source = form.source.trim();
                if source.is_empty() {
                    form.field = AddField::Source;
                    self.set_status("Enter a magnet link, URL or .torrent path".into(), true);
                } else {
                    if looks_like_path(source)
                        && !std::path::Path::new(&expand_tilde(source)).is_file()
                    {
                        form.field = AddField::Source;
                        self.set_status(format!("Not a file: {source}"), true);
                        self.mode = Mode::Add(form);
                        return;
                    }
                    self.send(Cmd::Add(NewTorrent {
                        source: expand_tilde(source),
                        category: form.category,
                        save_path: expand_tilde(form.save_path.trim()),
                    }));
                    return;
                }
            }
            KeyCode::Tab if form.field == AddField::Source && looks_like_path(&form.source) => {
                form.complete_source();
            }
            // Accept the completion being cycled through, so the next Tab descends into it.
            // Copy the path qBittorrent would use into the field, to edit it.
            KeyCode::Right if form.field == AddField::SavePath && form.save_path.is_empty() => {
                if let Some((path, _)) = self.effective_save_path(form.category.as_deref()) {
                    form.save_path = path;
                }
            }
            KeyCode::Right if form.field == AddField::Source => {
                form.completions.clear();
                form.completion_idx = None;
            }
            KeyCode::Tab | KeyCode::Down => {
                form.field = match form.field {
                    AddField::Source => AddField::Category,
                    AddField::Category => AddField::SavePath,
                    AddField::SavePath => AddField::Source,
                };
            }
            KeyCode::BackTab | KeyCode::Up => {
                form.field = match form.field {
                    AddField::Source => AddField::SavePath,
                    AddField::Category => AddField::Source,
                    AddField::SavePath => AddField::Category,
                };
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('j' | 'k' | ' ')
                if form.field == AddField::Category =>
            {
                // Cycle through "no category" followed by each named category.
                let mut choices: Vec<Option<String>> = vec![None];
                choices.extend(self.categories.iter().filter_map(|c| match c {
                    Category::Named(n) => Some(Some(n.clone())),
                    Category::All | Category::None => None,
                }));
                let i = choices
                    .iter()
                    .position(|c| *c == form.category)
                    .unwrap_or(0) as isize;
                let back = matches!(key.code, KeyCode::Left | KeyCode::Char('k'));
                let next = (i + if back { -1 } else { 1 }).rem_euclid(choices.len() as isize);
                form.category = choices[next as usize].clone();
            }
            KeyCode::Char('u') if modified => {
                if let Some(buf) = form.text_field() {
                    buf.clear();
                }
            }
            KeyCode::Backspace => {
                if let Some(buf) = form.text_field() {
                    buf.pop();
                }
            }
            KeyCode::Char(c) if !modified => {
                if let Some(buf) = form.text_field() {
                    buf.push(c);
                }
            }
            _ => {}
        }
        self.mode = Mode::Add(form);
    }

    pub fn picker_options(&self, filter: &str) -> Vec<PickOption> {
        let needle = filter.trim().to_lowercase();
        let mut out = Vec::new();
        if needle.is_empty() {
            out.push(PickOption::None);
        }
        let mut exact = false;
        for c in &self.categories {
            if let Category::Named(n) = c
                && n.to_lowercase().contains(&needle)
            {
                exact |= n.to_lowercase() == needle;
                out.push(PickOption::Existing(n.clone()));
            }
        }
        if !needle.is_empty() && !exact {
            out.push(PickOption::Create(filter.trim().to_string()));
        }
        out
    }

    fn open_picker(&mut self) {
        let targets = self.targets();
        let Some(first) = targets.first() else { return };
        // Start on the category the (first) target is already in.
        let current = self
            .torrents
            .iter()
            .find(|t| &t.hash == first)
            .map(|t| t.category.clone())
            .unwrap_or_default();
        let idx = self
            .picker_options("")
            .iter()
            .position(|o| match o {
                PickOption::None => current.is_empty(),
                PickOption::Existing(n) => *n == current,
                PickOption::Create(_) => false,
            })
            .unwrap_or(0);
        self.mode = Mode::Recategorize(Picker {
            targets,
            filter: String::new(),
            idx,
        });
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Mode::Recategorize(mut p) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = self.picker_options(&p.filter).len();
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                let cmd = match self.picker_options(&p.filter).get(p.idx) {
                    Some(PickOption::None) => Cmd::SetCategory(p.targets, String::new(), false),
                    Some(PickOption::Existing(n)) => Cmd::SetCategory(p.targets, n.clone(), false),
                    Some(PickOption::Create(n)) => Cmd::SetCategory(p.targets, n.clone(), true),
                    None => return,
                };
                self.send(cmd);
                return;
            }
            KeyCode::Down | KeyCode::Tab => p.idx = (p.idx + 1) % len.max(1),
            KeyCode::Char('n') if ctrl => p.idx = (p.idx + 1) % len.max(1),
            KeyCode::Up | KeyCode::BackTab => p.idx = (p.idx + len.max(1) - 1) % len.max(1),
            KeyCode::Char('p') if ctrl => p.idx = (p.idx + len.max(1) - 1) % len.max(1),
            KeyCode::Backspace => {
                p.filter.pop();
                p.idx = 0;
            }
            KeyCode::Char('u') if ctrl => {
                p.filter.clear();
                p.idx = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                p.filter.push(c);
                p.idx = 0;
            }
            _ => {}
        }
        self.mode = Mode::Recategorize(p);
    }

    fn open_tag_picker(&mut self) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let mut counts: HashMap<String, usize> = HashMap::new();
        for t in self.torrents.iter().filter(|t| targets.contains(&t.hash)) {
            for tag in torrent_tags(t) {
                *counts.entry(tag.to_string()).or_default() += 1;
            }
        }
        let mut tags = self.tags.clone();
        tags.extend(counts.keys().filter(|k| !self.tags.contains(k)).cloned());
        tags.sort_by_key(|t| t.to_lowercase());
        let state = counts
            .into_iter()
            .map(|(tag, n)| {
                (
                    tag,
                    if n == targets.len() {
                        Check::All
                    } else {
                        Check::Some
                    },
                )
            })
            .collect();
        self.mode = Mode::Tag(TagPicker {
            targets,
            filter: String::new(),
            idx: 0,
            tags,
            state,
        });
    }

    fn tag_key(&mut self, key: KeyEvent) {
        let Mode::Tag(mut p) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = p.options().len().max(1);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter | KeyCode::Char(' ') => match p.options().get(p.idx).cloned() {
                Some(Ok(tag)) => {
                    // Partially or un-tagged → tag all; fully tagged → untag all.
                    let add = p.check(&tag) != Check::All;
                    self.send(Cmd::Tag(p.targets.clone(), tag.clone(), add));
                    p.state
                        .insert(tag, if add { Check::All } else { Check::None });
                }
                Some(Err(new)) => {
                    self.send(Cmd::Tag(p.targets.clone(), new.clone(), true));
                    p.tags.push(new.clone());
                    p.tags.sort_by_key(|t| t.to_lowercase());
                    p.state.insert(new.clone(), Check::All);
                    p.filter.clear();
                    p.idx = p
                        .options()
                        .iter()
                        .position(|o| o.as_ref() == Ok(&new))
                        .unwrap_or(0);
                }
                None => {}
            },
            KeyCode::Down | KeyCode::Tab => p.idx = (p.idx + 1) % len,
            KeyCode::Char('n') if ctrl => p.idx = (p.idx + 1) % len,
            KeyCode::Up | KeyCode::BackTab => p.idx = (p.idx + len - 1) % len,
            KeyCode::Char('p') if ctrl => p.idx = (p.idx + len - 1) % len,
            KeyCode::Backspace => {
                p.filter.pop();
                p.idx = 0;
            }
            KeyCode::Char('u') if ctrl => {
                p.filter.clear();
                p.idx = 0;
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                p.filter.push(c);
                p.idx = 0;
            }
            _ => {}
        }
        self.mode = Mode::Tag(p);
    }

    fn open_files(&mut self) {
        let Some(t) = self.selected() else { return };
        let (hash, name) = (t.hash.clone(), t.name.clone());
        self.send(Cmd::Files(Some(hash.clone())));
        self.mode = Mode::Files(FilesView {
            hash,
            name,
            files: None,
            collapsed: HashSet::new(),
            table: TableState::default(),
        });
    }

    fn files_move(&mut self, delta: isize) {
        let Mode::Files(v) = &mut self.mode else {
            return;
        };
        let len = v.rows().len();
        if len == 0 {
            return;
        }
        let cur = v.table.selected().unwrap_or(0) as isize;
        v.table
            .select(Some((cur + delta).clamp(0, len as isize - 1) as usize));
    }

    /// Apply a priority locally right away and send it to qBittorrent.
    fn set_file_priority(&mut self, ids: Vec<usize>, priority: i64) {
        let Mode::Files(v) = &mut self.mode else {
            return;
        };
        let Some(files) = &mut v.files else { return };
        for &i in &ids {
            files[i].priority = priority;
        }
        let hash = v.hash.clone();
        self.send(Cmd::FilePrio(hash, ids, priority));
    }

    fn files_key(&mut self, key: KeyEvent) {
        let Mode::Files(v) = &mut self.mode else {
            return;
        };
        let rows = v.rows();
        let sel = v.table.selected().and_then(|i| rows.get(i).map(|r| (i, r)));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_files(),
            KeyCode::Right | KeyCode::Enter if matches!(sel, Some((_, FileRow::Folder { .. }))) => {
                let Some((_, FileRow::Folder { path, open, .. })) = sel else {
                    return;
                };
                if *open && key.code == KeyCode::Enter {
                    v.collapsed.insert(path.clone());
                } else {
                    v.collapsed.remove(path);
                }
            }
            // Fold the folder under the cursor, else step out to the parent folder, else close.
            KeyCode::Left => match sel {
                Some((
                    _,
                    FileRow::Folder {
                        path, open: true, ..
                    },
                )) => {
                    v.collapsed.insert(path.clone());
                }
                Some((i, row)) if row.depth() > 0 => {
                    let parent = rows[..i].iter().rposition(|r| r.depth() < row.depth());
                    v.table.select(parent);
                }
                _ => self.close_files(),
            },
            KeyCode::Char(' ') => {
                let Some((_, row)) = sel else { return };
                let ids = row.files();
                let skipped = v.priority(&ids) == Some(0);
                self.set_file_priority(ids, if skipped { 1 } else { 0 });
            }
            KeyCode::Char(c @ ('+' | '=' | '-')) => {
                let Some((_, row)) = sel else { return };
                let ids = row.files();
                let Some(files) = v.files.as_ref() else {
                    return;
                };
                let current = ids.iter().map(|&i| files[i].priority).max().unwrap_or(1);
                let level = FILE_PRIORITIES
                    .iter()
                    .position(|&p| p >= current)
                    .unwrap_or(1);
                let level = if c == '-' {
                    level.saturating_sub(1)
                } else {
                    (level + 1).min(FILE_PRIORITIES.len() - 1)
                };
                self.set_file_priority(ids, FILE_PRIORITIES[level]);
            }
            KeyCode::Down | KeyCode::Char('j') => self.files_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.files_move(-1),
            KeyCode::PageDown => self.files_move(self.page_size as isize),
            KeyCode::PageUp => self.files_move(-(self.page_size as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.files_move(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.files_move(isize::MAX / 2),
            _ => {}
        }
    }

    fn close_files(&mut self) {
        self.send(Cmd::Files(None));
        self.mode = Mode::Normal;
    }

    fn normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if !self.marked.is_empty() {
                    self.marked.clear();
                } else if !self.search.is_empty() {
                    self.search.clear();
                    self.rebuild_view();
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            // Vim keys drive the category sidebar; arrows and paging keys drive the torrent list.
            KeyCode::Char('j') => self.move_category(1),
            KeyCode::Char('k') => self.move_category(-1),
            KeyCode::Char('J') => self.move_tag(1),
            KeyCode::Char('K') => self.move_tag(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::PageDown => self.move_by(self.page_size as isize),
            KeyCode::PageUp => self.move_by(-(self.page_size as isize)),
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(usize::MAX),
            KeyCode::BackTab => self.cycle_filter(-1),
            KeyCode::Tab => self.cycle_filter(1),
            KeyCode::Right => self.open_files(),
            KeyCode::Char(c @ '1'..='6') => {
                self.filter = Filter::ALL[c as usize - '1' as usize];
                self.rebuild_view();
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.rebuild_view();
            }
            KeyCode::Char('S') => {
                self.reverse = !self.reverse;
                self.rebuild_view();
            }
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('a') => self.open_add_form(),
            KeyCode::Char(' ') => {
                if let Some(h) = self.selected().map(|t| t.hash.clone()) {
                    if !self.marked.remove(&h) {
                        self.marked.insert(h);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Char('p') => {
                let targets = self.targets();
                if targets.is_empty() {
                    return;
                }
                let all_paused = targets.iter().all(|h| {
                    self.torrents
                        .iter()
                        .find(|t| &t.hash == h)
                        .is_some_and(|t| t.is_paused())
                });
                self.send(if all_paused {
                    Cmd::Resume(targets)
                } else {
                    Cmd::Pause(targets)
                });
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if !self.targets().is_empty() {
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('C') => self.open_picker(),
            KeyCode::Char('T') => self.open_tag_picker(),
            KeyCode::Char('c') => {
                let t = self.targets();
                if !t.is_empty() {
                    self.send(Cmd::Recheck(t));
                }
            }
            KeyCode::Char('R') => {
                let t = self.targets();
                if !t.is_empty() {
                    self.send(Cmd::Reannounce(t));
                }
            }
            KeyCode::Char('r') => self.send(Cmd::Refresh),
            _ => {}
        }
    }
}

/// Expand a leading `~`, alone or before a path separator, to the home directory.
pub fn expand_tilde(s: &str) -> String {
    let Some(rest) = s.strip_prefix('~') else {
        return s.to_string();
    };
    match (rest.chars().next(), std::env::home_dir()) {
        (None, Some(home)) => home.display().to_string(),
        (Some(c), Some(home)) if std::path::is_separator(c) => format!("{}{rest}", home.display()),
        _ => s.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::path::MAIN_SEPARATOR as SEP;
    use std::sync::mpsc::{self, Receiver};

    pub(crate) fn torrent(hash: &str, name: &str, state: &str) -> Torrent {
        Torrent {
            hash: hash.into(),
            name: name.into(),
            state: state.into(),
            ..Default::default()
        }
    }

    fn category(name: &str, save_path: &str) -> CategoryInfo {
        CategoryInfo {
            name: name.into(),
            save_path: save_path.into(),
        }
    }

    pub(crate) fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Type each character of `keys` as a key press.
    pub(crate) fn press(app: &mut App, keys: &str) {
        for c in keys.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    /// An app after one refresh with `torrents`, and the receiving end of its commands.
    fn app_with(
        torrents: Vec<Torrent>,
        categories: Vec<CategoryInfo>,
        tags: &[&str],
    ) -> (App, Receiver<Cmd>) {
        let (tx, rx) = mpsc::channel();
        let mut app = App::new("http://localhost:8080".into(), String::new(), tx);
        let tags = tags.iter().map(|t| t.to_string()).collect();
        app.handle_msg(Msg::Data(torrents, Transfer::default(), categories, tags));
        (app, rx)
    }

    /// Three torrents with every column filled in. Newest first: aaa, ccc, bbb.
    pub(crate) fn sample() -> (App, Receiver<Cmd>) {
        let mut a = torrent("aaa", "Alpha", "downloading");
        a.added_on = 3;
        a.size = 3 << 30;
        a.progress = 0.42;
        a.dlspeed = 2_500_000;
        a.eta = 3_700;
        a.num_seeds = 4;
        a.num_leechs = 2;
        a.category = "Movies".into();
        a.tags = "hd, new".into();
        a.save_path = "/data/Movies".into();
        let mut b = torrent("bbb", "Bravo", "pausedUP");
        b.added_on = 1;
        b.size = 700 << 20;
        b.progress = 1.0;
        b.ratio = 1.5;
        b.tags = "hd".into();
        let mut c = torrent(
            "ccc",
            "Charlie, a torrent whose name is far too long to fit in any reasonable column",
            "stalledUP",
        );
        c.added_on = 2;
        c.progress = 1.0;
        c.upspeed = 1_000;
        c.category = "TV".into();
        app_with(
            vec![a, b, c],
            vec![category("Movies", ""), category("TV", "/tv")],
            &["hd", "new"],
        )
    }

    fn hashes(app: &App) -> Vec<&str> {
        app.view
            .iter()
            .map(|&i| app.torrents[i].hash.as_str())
            .collect()
    }

    fn selected(app: &App) -> Option<&str> {
        app.selected().map(|t| t.hash.as_str())
    }

    fn select_hash(app: &mut App, hash: &str) {
        let i = hashes(app).iter().position(|&h| h == hash).unwrap();
        app.select(i);
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Commands sent so far, with hash lists sorted since a marked set has no order.
    fn sent(rx: &Receiver<Cmd>) -> Vec<Cmd> {
        rx.try_iter()
            .map(|mut cmd| {
                match &mut cmd {
                    Cmd::Pause(h)
                    | Cmd::Resume(h)
                    | Cmd::Delete(h, _)
                    | Cmd::Recheck(h)
                    | Cmd::Reannounce(h)
                    | Cmd::SetCategory(h, ..)
                    | Cmd::Tag(h, ..) => h.sort(),
                    Cmd::Refresh | Cmd::Prefs | Cmd::Add(_) | Cmd::Files(_) | Cmd::FilePrio(..) => {
                    }
                }
                cmd
            })
            .collect()
    }

    #[test]
    fn newest_first_with_cursor_on_top() {
        let (app, _rx) = sample();
        assert_eq!(hashes(&app), ["aaa", "ccc", "bbb"]);
        assert_eq!(selected(&app), Some("aaa"));
        assert!(app.connected);
    }

    #[test]
    fn sorting_keeps_the_cursor_on_the_same_torrent() {
        let (mut app, _rx) = sample();
        app.handle_key(key(KeyCode::Down));
        assert_eq!(selected(&app), Some("ccc"));

        press(&mut app, "s");
        assert!(app.sort == Sort::Name);
        assert_eq!(hashes(&app), ["ccc", "bbb", "aaa"]);
        assert_eq!(selected(&app), Some("ccc"));
        assert_eq!(app.table.selected(), Some(0));

        press(&mut app, "S");
        assert_eq!(hashes(&app), ["aaa", "bbb", "ccc"]);
        assert_eq!(app.table.selected(), Some(2));
    }

    #[test]
    fn sort_order_cycles_back_to_added() {
        assert!(Sort::ORDER.iter().all(|s| !s.label().is_empty()));
        let mut sort = Sort::Added;
        for _ in 0..Sort::ORDER.len() {
            sort = sort.next();
        }
        assert!(sort == Sort::Added);
    }

    #[test]
    fn refresh_keeps_the_cursor_and_drops_stale_marks() {
        let (mut app, _rx) = sample();
        select_hash(&mut app, "ccc");
        press(&mut app, " ");
        assert!(app.marked.contains("ccc"));
        assert_eq!(
            selected(&app),
            Some("bbb"),
            "space moves down after marking"
        );

        let mut torrents = app.torrents.clone();
        torrents.retain(|t| t.hash != "ccc");
        torrents.reverse();
        app.handle_msg(Msg::Data(
            torrents,
            Transfer::default(),
            Vec::new(),
            Vec::new(),
        ));
        assert_eq!(selected(&app), Some("bbb"));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn number_keys_and_tab_pick_the_state_filter() {
        let (mut app, _rx) = sample();
        press(&mut app, "2");
        assert_eq!(hashes(&app), ["aaa"]);
        press(&mut app, "3");
        assert_eq!(hashes(&app), ["ccc"]);
        press(&mut app, "4");
        assert_eq!(hashes(&app), ["bbb"]);
        press(&mut app, "5");
        assert_eq!(hashes(&app), ["aaa", "ccc"]);
        press(&mut app, "6");
        assert!(hashes(&app).is_empty());
        assert_eq!(selected(&app), None);

        app.handle_key(key(KeyCode::Tab));
        assert!(app.filter == Filter::All, "Tab wraps from the last filter");
        app.handle_key(key(KeyCode::BackTab));
        assert!(app.filter == Filter::Errored);
    }

    #[test]
    fn paused_and_errored_states() {
        let paused = ["pausedDL", "pausedUP", "stoppedDL", "stoppedUP"];
        assert!(paused.iter().all(|s| torrent("h", "n", s).is_paused()));
        assert!(!torrent("h", "n", "stalledUP").is_paused());
        assert!(Filter::Errored.matches(&torrent("h", "n", "missingFiles")));
        assert!(!Filter::Seeding.matches(&torrent("h", "n", "stalledDL")));
    }

    #[test]
    fn vim_keys_walk_the_category_and_tag_sidebars() {
        let (mut app, _rx) = sample();
        press(&mut app, "j");
        assert!(app.category == Category::None);
        assert_eq!(hashes(&app), ["bbb"]);
        press(&mut app, "j");
        assert_eq!(hashes(&app), ["aaa"]);
        press(&mut app, "jjj");
        assert!(
            app.category == Category::Named("TV".into()),
            "stops at the last"
        );
        assert_eq!(hashes(&app), ["ccc"]);
        press(&mut app, "kkkkk");
        assert!(app.category == Category::All);

        press(&mut app, "J");
        assert_eq!(hashes(&app), ["ccc"], "untagged");
        press(&mut app, "J");
        assert_eq!(hashes(&app), ["aaa", "bbb"], "tagged hd");
        press(&mut app, "J");
        assert_eq!(hashes(&app), ["aaa"], "tagged new");
    }

    #[test]
    fn a_category_that_disappears_resets_the_sidebar() {
        let (mut app, _rx) = sample();
        press(&mut app, "jjj");
        assert!(app.category == Category::Named("TV".into()));
        let torrents = app.torrents.clone();
        let categories = vec![category("Movies", "")];
        app.handle_msg(Msg::Data(
            torrents,
            Transfer::default(),
            categories,
            Vec::new(),
        ));
        assert!(app.category == Category::All);
        assert_eq!(hashes(&app).len(), 3);
    }

    #[test]
    fn tags_are_split_on_commas() {
        let mut t = torrent("h", "n", "");
        t.tags = "hd, new,  x ,".into();
        assert_eq!(torrent_tags(&t).collect::<Vec<_>>(), ["hd", "new", "x"]);
        t.tags.clear();
        assert_eq!(torrent_tags(&t).count(), 0);
    }

    #[test]
    fn search_filters_by_name_ignoring_case() {
        let (mut app, _rx) = sample();
        press(&mut app, "/BRq");
        assert!(!app.quit, "q is text while searching");
        assert!(hashes(&app).is_empty());
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(hashes(&app), ["bbb"]);
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(hashes(&app), ["bbb"], "the search stays after Enter");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(hashes(&app).len(), 3);

        press(&mut app, "/al");
        app.handle_paste("pha\n");
        assert_eq!(hashes(&app), ["aaa"]);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.search.is_empty());
    }

    #[test]
    fn p_resumes_when_every_target_is_paused_and_pauses_otherwise() {
        let (mut app, rx) = sample();
        select_hash(&mut app, "bbb");
        press(&mut app, "p");
        select_hash(&mut app, "aaa");
        press(&mut app, "p");
        assert_eq!(
            sent(&rx),
            [
                Cmd::Resume(strings(&["bbb"])),
                Cmd::Pause(strings(&["aaa"]))
            ]
        );

        // Marked torrents are the targets, wherever the cursor is.
        press(&mut app, " ");
        select_hash(&mut app, "bbb");
        press(&mut app, " p");
        assert_eq!(sent(&rx), [Cmd::Pause(strings(&["aaa", "bbb"]))]);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn delete_asks_first() {
        let (mut app, rx) = sample();
        press(&mut app, "d");
        assert!(matches!(app.mode, Mode::ConfirmDelete));
        press(&mut app, "n");
        assert!(matches!(app.mode, Mode::Normal));
        app.handle_key(key(KeyCode::Delete));
        press(&mut app, "y");
        press(&mut app, "dD");
        assert_eq!(
            sent(&rx),
            [
                Cmd::Delete(strings(&["aaa"]), false),
                Cmd::Delete(strings(&["aaa"]), true)
            ]
        );
    }

    #[test]
    fn one_key_commands() {
        let (mut app, rx) = sample();
        press(&mut app, "cRr");
        assert_eq!(
            sent(&rx),
            [
                Cmd::Recheck(strings(&["aaa"])),
                Cmd::Reannounce(strings(&["aaa"])),
                Cmd::Refresh
            ]
        );
        press(&mut app, "?");
        assert!(matches!(app.mode, Mode::Help));
        press(&mut app, "q");
        assert!(matches!(app.mode, Mode::Normal), "any key closes help");
        assert!(!app.quit);
        press(&mut app, "q");
        assert!(app.quit);
    }

    #[test]
    fn nothing_selected_means_nothing_sent() {
        let (mut app, rx) = app_with(Vec::new(), Vec::new(), &[]);
        press(&mut app, "pcRdCT ");
        app.handle_key(key(KeyCode::Right));
        assert!(matches!(app.mode, Mode::Normal));
        assert!(sent(&rx).is_empty());
    }

    #[test]
    fn ctrl_c_quits_from_any_dialog() {
        let (mut app, _rx) = sample();
        press(&mut app, "a");
        assert!(matches!(app.mode, Mode::Add(_)));
        app.handle_key(ctrl('c'));
        assert!(app.quit);
    }

    #[test]
    fn errors_disconnect_until_the_next_refresh() {
        let (mut app, _rx) = sample();
        app.handle_msg(Msg::Error("boom".into()));
        assert!(!app.connected);
        assert!(matches!(&app.status, Some((s, true, _)) if s == "boom"));
        let torrents = app.torrents.clone();
        app.handle_msg(Msg::Data(
            torrents,
            Transfer::default(),
            Vec::new(),
            Vec::new(),
        ));
        assert!(app.connected);
        assert!(app.status.is_none());
    }

    #[test]
    fn mouse_clicks_select_mark_and_filter() {
        let (mut app, _rx) = sample();
        app.hit.table = Rect::new(20, 1, 100, 20);
        app.hit.tabs = vec![Rect::new(20, 0, 5, 1), Rect::new(26, 0, 5, 1)];
        let click = |button, column, row| MouseEvent {
            kind: MouseEventKind::Down(button),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Row 3 is the second torrent: below the table's border and header.
        app.handle_mouse(click(MouseButton::Left, 30, 4));
        assert_eq!(selected(&app), Some("ccc"));
        app.handle_mouse(click(MouseButton::Right, 30, 5));
        assert_eq!(selected(&app), Some("bbb"));
        assert!(app.marked.contains("bbb"));
        app.handle_mouse(click(MouseButton::Left, 30, 19));
        assert_eq!(
            selected(&app),
            Some("bbb"),
            "clicks below the last row do nothing"
        );
        app.handle_mouse(click(MouseButton::Left, 27, 0));
        assert!(app.filter == Filter::Downloading);
    }

    #[test]
    fn category_picker_options() {
        let (app, _rx) = sample();
        let existing = |n: &str| PickOption::Existing(n.into());
        assert!(app.picker_options("") == [PickOption::None, existing("Movies"), existing("TV")]);
        assert!(app.picker_options("mo") == [existing("Movies"), PickOption::Create("mo".into())]);
        assert!(app.picker_options("MOVIES") == [existing("Movies")]);
        assert!(app.picker_options(" tv ") == [existing("TV")]);
    }

    #[test]
    fn category_picker_moves_torrents() {
        let (mut app, rx) = sample();
        press(&mut app, "C");
        let Mode::Recategorize(p) = &app.mode else {
            panic!("picker not open")
        };
        assert_eq!(p.idx, 1, "starts on the torrent's own category");
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "C");
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "CAnime");
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "C");
        app.handle_key(key(KeyCode::Esc));
        let aaa = || strings(&["aaa"]);
        assert_eq!(
            sent(&rx),
            [
                Cmd::SetCategory(aaa(), "Movies".into(), false),
                Cmd::SetCategory(aaa(), String::new(), false),
                Cmd::SetCategory(aaa(), "Anime".into(), true),
            ]
        );
        assert!(matches!(app.mode, Mode::Normal));
    }

    #[test]
    fn tag_picker_toggles_tags_on_all_targets() {
        let (mut app, rx) = sample();
        press(&mut app, " ");
        select_hash(&mut app, "bbb");
        press(&mut app, " T");
        let Mode::Tag(p) = &app.mode else {
            panic!("tag picker not open")
        };
        assert!(p.check("hd") == Check::All);
        assert!(p.check("new") == Check::Some);
        assert!(p.check("other") == Check::None);

        let both = || strings(&["aaa", "bbb"]);
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, " ");
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "4k");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            sent(&rx),
            [
                Cmd::Tag(both(), "hd".into(), false),
                Cmd::Tag(both(), "hd".into(), true),
                Cmd::Tag(both(), "new".into(), true),
                Cmd::Tag(both(), "4k".into(), true),
            ]
        );
        let Mode::Tag(p) = &app.mode else {
            panic!("tag picker closed")
        };
        assert!(p.filter.is_empty());
        assert_eq!(p.options()[p.idx], Ok("4k".to_string()));
        assert!(p.check("4k") == Check::All);
    }

    #[test]
    fn effective_save_path_follows_qbittorrent() {
        let categories = vec![
            category("Movies", ""),
            category("Rel", "sub/dir"),
            category("Abs", "/srv/abs"),
            category("Win", "E:\\win"),
        ];
        let (mut app, _rx) = app_with(Vec::new(), categories, &[]);
        assert_eq!(
            app.effective_save_path(Some("Movies")),
            None,
            "no preferences yet"
        );

        let prefs = |save_path: &str, auto_tmm_enabled| {
            Msg::Prefs(Prefs {
                save_path: save_path.into(),
                auto_tmm_enabled,
            })
        };
        let path = |app: &App, cat| app.effective_save_path(cat);
        app.handle_msg(prefs("/data/", false));
        let default = Some(("/data/".to_string(), "qBittorrent default"));
        assert_eq!(path(&app, Some("Movies")), default);

        app.handle_msg(prefs("/data/", true));
        let cat = |p: &str| Some((p.to_string(), "category"));
        assert_eq!(path(&app, None), default);
        assert_eq!(path(&app, Some("Movies")), cat("/data/Movies"));
        assert_eq!(path(&app, Some("Rel")), cat("/data/sub/dir"));
        assert_eq!(path(&app, Some("Abs")), cat("/srv/abs"));

        app.handle_msg(prefs("D:\\dl\\", true));
        assert_eq!(path(&app, Some("Movies")), cat("D:\\dl\\Movies"));
        assert_eq!(path(&app, Some("Win")), cat("E:\\win"));
    }

    #[test]
    fn add_form_sends_a_magnet_link() {
        let (mut app, rx) = sample();
        press(&mut app, "jj");
        press(&mut app, "a");
        let Mode::Add(form) = &app.mode else {
            panic!("add form not open")
        };
        assert_eq!(
            form.category.as_deref(),
            Some("Movies"),
            "taken from the sidebar"
        );
        press(&mut app, "magnet:?xt=urn:btih:jk");
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Right));
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(
            sent(&rx),
            [
                Cmd::Prefs,
                Cmd::Add(NewTorrent {
                    source: "magnet:?xt=urn:btih:jk".into(),
                    category: Some("TV".into()),
                    save_path: String::new(),
                })
            ]
        );
    }

    #[test]
    fn add_form_category_cycles_through_none() {
        let (mut app, _rx) = sample();
        press(&mut app, "a");
        app.handle_key(key(KeyCode::Tab));
        let category = |app: &App| match &app.mode {
            Mode::Add(form) => form.category.clone(),
            _ => panic!("add form closed"),
        };
        press(&mut app, "j");
        assert_eq!(category(&app).as_deref(), Some("Movies"));
        app.handle_key(key(KeyCode::Left));
        press(&mut app, "k");
        assert_eq!(category(&app).as_deref(), Some("TV"), "wraps around");
    }

    #[test]
    fn add_form_needs_a_source_and_an_existing_file() {
        let (mut app, rx) = sample();
        press(&mut app, "a");
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        let Mode::Add(form) = &app.mode else {
            panic!("add form closed")
        };
        assert!(
            form.field == AddField::Source,
            "sent back to the empty field"
        );
        assert!(matches!(&app.status, Some((_, true, _))));

        press(&mut app, "./no-such-file.torrent");
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, Mode::Add(_)));
        assert!(matches!(&app.status, Some((s, true, _)) if s.starts_with("Not a file")));
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, Mode::Normal));
        assert_eq!(sent(&rx), [Cmd::Prefs]);
    }

    #[test]
    fn add_form_sends_a_local_file_and_save_path() {
        let dir = TempDir::new("add-file");
        let file = dir.touch("x.torrent").display().to_string();
        let (mut app, rx) = sample();
        app.handle_msg(Msg::Prefs(Prefs {
            save_path: "/data".into(),
            auto_tmm_enabled: false,
        }));
        press(&mut app, "a");
        app.handle_paste(&file);
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Right));
        press(&mut app, "/x");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            sent(&rx),
            [
                Cmd::Prefs,
                Cmd::Add(NewTorrent {
                    source: file,
                    category: None,
                    save_path: "/data/x".into(),
                })
            ]
        );
    }

    #[test]
    fn pasting_replaces_the_prefilled_directory() {
        let (mut app, _rx) = sample();
        app.torrent_dir = "~/Downloads".into();
        press(&mut app, "a");
        let source = |app: &App| match &app.mode {
            Mode::Add(form) => form.source.clone(),
            _ => panic!("add form closed"),
        };
        assert_eq!(source(&app), format!("~/Downloads{SEP}"));
        app.handle_paste("magnet:?xt=1\r\n");
        assert_eq!(source(&app), "magnet:?xt=1");
        app.handle_paste("2");
        assert_eq!(source(&app), "magnet:?xt=12", "later pastes append");
        app.handle_key(ctrl('u'));
        assert_eq!(source(&app), "");
    }

    /// A directory with folders, .torrent files and things completion should skip.
    fn completion_dir(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        for f in [
            "movies/a.mkv",
            "music/b.mp3",
            "abc1.torrent",
            "abc2.torrent",
            "Show.TORRENT",
            "notes.txt",
            ".hidden.torrent",
        ] {
            dir.touch(f);
        }
        dir
    }

    fn form(source: &str) -> AddForm {
        AddForm {
            source: source.into(),
            category: None,
            save_path: String::new(),
            field: AddField::Source,
            prefill: String::new(),
            completions: Vec::new(),
            completion_idx: None,
        }
    }

    #[test]
    fn path_completions_list_folders_and_torrents() {
        let dir = completion_dir("completions");
        let p = dir.prefix();
        let names = |input: &str| -> Vec<String> {
            path_completions(input)
                .into_iter()
                .map(|c| c.strip_prefix(&p).unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(&p),
            [
                "Show.TORRENT".to_string(),
                "abc1.torrent".into(),
                "abc2.torrent".into(),
                format!("movies{SEP}"),
                format!("music{SEP}"),
            ]
        );
        assert_eq!(names(&format!("{p}.")), [".hidden.torrent"]);
        assert_eq!(names(&format!("{p}mov")), [format!("movies{SEP}")]);
        assert!(names(&format!("{p}zzz")).is_empty());
        assert!(path_completions(&format!("{p}missing{SEP}")).is_empty());
    }

    #[test]
    fn tab_extends_to_the_common_prefix_then_cycles() {
        let dir = completion_dir("tab");
        let p = dir.prefix();

        let mut f = form(&format!("{p}mov"));
        f.complete_source();
        assert_eq!(
            f.source,
            format!("{p}movies{SEP}"),
            "a single match is taken"
        );
        assert!(f.completions.is_empty());

        let mut f = form(&format!("{p}a"));
        f.complete_source();
        assert_eq!(
            f.source,
            format!("{p}abc"),
            "first Tab fills the common prefix"
        );
        assert_eq!(f.completion_idx, None);
        f.complete_source();
        assert_eq!(f.source, format!("{p}abc1.torrent"));
        f.complete_source();
        assert_eq!(f.source, format!("{p}abc2.torrent"));
        f.complete_source();
        assert_eq!(f.source, format!("{p}abc1.torrent"), "cycling wraps");
    }

    #[test]
    fn tab_in_the_add_form_completes_from_the_torrent_dir() {
        let dir = completion_dir("tab-keys");
        let (mut app, _rx) = sample();
        app.torrent_dir = dir.path().display().to_string();
        press(&mut app, "a");
        press(&mut app, "mu");
        app.handle_key(key(KeyCode::Tab));
        let Mode::Add(form) = &app.mode else {
            panic!("add form closed")
        };
        assert_eq!(form.source, format!("{}music{SEP}", dir.prefix()));
        assert!(
            form.field == AddField::Source,
            "Tab completes instead of moving on"
        );
    }

    #[test]
    fn common_prefix_respects_char_boundaries() {
        assert_eq!(common_prefix(&strings(&["héllo", "hélas"])), "hél");
        assert_eq!(common_prefix(&strings(&["abc", "ab"])), "ab");
        assert_eq!(common_prefix(&strings(&["x"])), "x");
        assert_eq!(common_prefix(&strings(&["é", "è"])), "");
    }

    #[test]
    fn paths_versus_links() {
        for s in ["~/a.torrent", "./a.torrent", "../a.torrent"] {
            assert!(looks_like_path(s), "{s}");
        }
        assert!(looks_like_path(&std::env::temp_dir().display().to_string()));
        for s in [
            "magnet:?xt=urn:btih:abc",
            "https://example.com/a.torrent",
            "",
        ] {
            assert!(!looks_like_path(s), "{s}");
        }
    }

    #[test]
    fn tilde_expands_only_alone_or_before_a_separator() {
        assert_eq!(expand_tilde("~other/x"), "~other/x");
        assert_eq!(expand_tilde("/abs/~"), "/abs/~");
        if let Some(home) = std::env::home_dir() {
            let home = home.display().to_string();
            assert_eq!(expand_tilde("~"), home);
            assert_eq!(expand_tilde(&format!("~{SEP}x")), format!("{home}{SEP}x"));
        }
    }

    fn file(name: &str, priority: i64) -> TorrentFile {
        TorrentFile {
            name: name.into(),
            priority,
            ..Default::default()
        }
    }

    fn files() -> Vec<TorrentFile> {
        vec![
            file("Show/ep2.mkv", 1),
            file("Show/ep1.mkv", 1),
            file("Show/Subs/en.srt", 1),
            file("readme.txt", 1),
        ]
    }

    /// The visible tree as "depth name", folders with their files and open state.
    fn tree(v: &FilesView) -> Vec<String> {
        v.rows()
            .iter()
            .map(|r| match r {
                FileRow::Folder {
                    name, open, files, ..
                } => {
                    let sign = if *open { '-' } else { '+' };
                    format!("{} {sign}{name} {files:?}", r.depth())
                }
                FileRow::File { idx, name, .. } => format!("{} {name} #{idx}", r.depth()),
            })
            .collect()
    }

    #[test]
    fn file_tree_puts_folders_first_and_folds() {
        let mut v = FilesView {
            hash: "h".into(),
            name: "Show".into(),
            files: Some(files()),
            collapsed: HashSet::new(),
            table: TableState::default(),
        };
        assert_eq!(
            tree(&v),
            [
                "0 -Show [0, 1, 2]",
                "1 -Subs [2]",
                "2 en.srt #2",
                "1 ep1.mkv #1",
                "1 ep2.mkv #0",
                "0 readme.txt #3",
            ]
        );
        v.collapsed.insert("Show/Subs".into());
        assert_eq!(tree(&v)[1..3], ["1 +Subs [2]", "1 ep1.mkv #1"]);
        v.collapsed.insert("Show".into());
        assert_eq!(tree(&v), ["0 +Show [0, 1, 2]", "0 readme.txt #3"]);

        assert_eq!(v.priority(&[0, 1, 2]), Some(1));
        v.files.as_mut().unwrap()[1].priority = 0;
        assert_eq!(v.priority(&[0, 1, 2]), None);
        assert_eq!(v.priority(&[]), None);
    }

    #[test]
    fn files_view_sets_priorities() {
        let (mut app, rx) = sample();
        app.handle_key(key(KeyCode::Right));
        press(&mut app, " ");
        app.handle_msg(Msg::Files("other".into(), files()));
        app.handle_msg(Msg::Files("aaa".into(), files()));
        let h = || "aaa".to_string();

        // On the Show folder: skip it all, then download it again.
        press(&mut app, "  ");
        // On ep1.mkv: raise to high, maximum, and no further; then lower.
        press(&mut app, "jjj");
        press(&mut app, "+=+-");
        // Back on the folder, whose highest priority is now high.
        app.handle_key(key(KeyCode::Home));
        press(&mut app, "+");
        assert_eq!(
            sent(&rx),
            [
                Cmd::Files(Some(h())),
                Cmd::FilePrio(h(), vec![0, 1, 2], 0),
                Cmd::FilePrio(h(), vec![0, 1, 2], 1),
                Cmd::FilePrio(h(), vec![1], 6),
                Cmd::FilePrio(h(), vec![1], 7),
                Cmd::FilePrio(h(), vec![1], 7),
                Cmd::FilePrio(h(), vec![1], 6),
                Cmd::FilePrio(h(), vec![0, 1, 2], 7),
            ]
        );
        let Mode::Files(v) = &app.mode else {
            panic!("files view closed")
        };
        assert_eq!(
            v.priority(&[0, 1, 2]),
            Some(7),
            "applied locally right away"
        );
    }

    #[test]
    fn files_view_navigation() {
        let (mut app, rx) = sample();
        app.handle_key(key(KeyCode::Right));
        app.handle_msg(Msg::Files("aaa".into(), files()));
        let rows = |app: &App| match &app.mode {
            Mode::Files(v) => (v.rows().len(), v.table.selected()),
            _ => panic!("files view closed"),
        };
        assert_eq!(rows(&app), (6, Some(0)));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(rows(&app), (2, Some(0)), "Enter folds an open folder");
        app.handle_key(key(KeyCode::Right));
        assert_eq!(rows(&app), (6, Some(0)));
        app.handle_key(key(KeyCode::Left));
        assert_eq!(rows(&app), (2, Some(0)), "Left folds too");
        app.handle_key(key(KeyCode::Enter));
        press(&mut app, "jjjj");
        assert_eq!(rows(&app), (6, Some(4)));
        app.handle_key(key(KeyCode::Left));
        assert_eq!(
            rows(&app),
            (6, Some(0)),
            "Left on a file goes to its folder"
        );

        // Fewer files on refresh keep the cursor in range.
        app.handle_key(key(KeyCode::End));
        assert_eq!(rows(&app), (6, Some(5)));
        app.handle_msg(Msg::Files("aaa".into(), vec![file("only.iso", 1)]));
        assert_eq!(rows(&app), (1, Some(0)));

        app.handle_key(key(KeyCode::Left));
        assert!(
            matches!(app.mode, Mode::Normal),
            "Left at the top level closes"
        );
        assert_eq!(
            sent(&rx),
            [Cmd::Files(Some("aaa".into())), Cmd::Files(None)]
        );
    }
}

use crate::app::{AddField, AddForm, App, Category, Check, Filter, Mode, PickOption, Picker, TagFilter, TagPicker};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Tabs, Wrap,
};
use std::time::Duration;

const ACCENT: Color = Color::Cyan;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
    let sidebar_w = if body.width >= 90 { 24 } else { 0 };
    let [sidebar, main] = Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Fill(1)]).areas(body);
    let [tabs, table, details] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(8)]).areas(main);

    draw_header(f, app, header);
    app.hit.sidebar = Rect::default();
    app.hit.tags = Rect::default();
    if sidebar_w > 0 {
        draw_sidebar(f, app, sidebar);
    }
    draw_tabs(f, app, tabs);
    draw_table(f, app, table);
    draw_details(f, app, details);
    draw_footer(f, app, footer);

    match &app.mode {
        Mode::Help => draw_help(f),
        Mode::Add(form) => {
            let hint = app.effective_save_path(form.category.as_deref());
            draw_add_form(f, form, hint);
        }
        Mode::Recategorize(p) => draw_picker(f, app, p),
        Mode::Tag(p) => draw_tag_picker(f, p),
        Mode::ConfirmDelete => draw_confirm(f, app),
        _ => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.transfer;
    let (dot, dot_style) = if app.connected {
        ("●", Style::new().fg(Color::Green))
    } else {
        ("●", Style::new().fg(Color::Red))
    };
    let left = Line::from(vec![
        Span::styled(" qbittui ", Style::new().bg(ACCENT).fg(Color::Black).bold()),
        Span::raw(" "),
        Span::styled(dot, dot_style),
        Span::raw(format!(" {} ", app.url)).dim(),
    ]);
    let right = Line::from(vec![
        Span::styled(format!("↓ {}", speed(t.dl_info_speed)), Style::new().fg(Color::Green)),
        Span::raw(format!(" ({})  ", bytes(t.dl_info_data))).dim(),
        Span::styled(format!("↑ {}", speed(t.up_info_speed)), Style::new().fg(Color::Magenta)),
        Span::raw(format!(" ({})  ", bytes(t.up_info_data))).dim(),
        Span::raw(format!("DHT {} ", t.dht_nodes)).dim(),
    ])
    .right_aligned();
    f.render_widget(left, area);
    f.render_widget(right, area);
}

fn pane() -> Block<'static> {
    Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::new().dim())
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    // Split the height by entry count (+2 for borders), keeping at least a few rows for each list.
    let cat_h = app.categories.len() as u16 + 2;
    let tag_h = app.tag_filters.len() as u16 + 2;
    let cat_h = if cat_h + tag_h <= area.height {
        cat_h
    } else {
        let share = (area.height as u32 * cat_h as u32 / (cat_h + tag_h) as u32) as u16;
        share.clamp(5.min(area.height), area.height.saturating_sub(5))
    };
    let [cats, tags] = Layout::vertical([Constraint::Length(cat_h), Constraint::Fill(1)]).areas(area);
    app.hit.sidebar = cats;
    app.hit.tags = tags;

    let entries: Vec<(String, Style, usize)> = app
        .categories
        .iter()
        .map(|c| {
            let (label, style) = match c {
                Category::All => ("All".to_string(), Style::new().bold()),
                Category::None => ("Uncategorized".to_string(), Style::new().italic()),
                Category::Named(n) => (n.clone(), Style::new()),
            };
            (label, style, app.torrents.iter().filter(|t| c.matches(t)).count())
        })
        .collect();
    app.sidebar.select(Some(app.category_index()));
    draw_side_list(f, cats, " Categories ", entries, &mut app.sidebar);

    let entries: Vec<(String, Style, usize)> = app
        .tag_filters
        .iter()
        .map(|t| {
            let (label, style) = match t {
                TagFilter::All => ("All".to_string(), Style::new().bold()),
                TagFilter::None => ("Untagged".to_string(), Style::new().italic()),
                TagFilter::Named(n) => (n.clone(), Style::new()),
            };
            (label, style, app.torrents.iter().filter(|x| t.matches(x)).count())
        })
        .collect();
    app.tag_list.select(Some(app.tag_index()));
    draw_side_list(f, tags, " Tags ", entries, &mut app.tag_list);
}

/// A bordered sidebar list of `(label, style, count)` rows with the counts right-aligned.
fn draw_side_list(f: &mut Frame, area: Rect, title: &str, entries: Vec<(String, Style, usize)>, state: &mut ListState) {
    let block = pane().title(Line::from(title.to_string()).bold());
    let inner_w = block.inner(area).width as usize;
    let items: Vec<ListItem> = entries
        .into_iter()
        .map(|(label, style, count)| {
            let count = count.to_string();
            // Leave room for the highlight symbol, the count and a space between them.
            let room = inner_w.saturating_sub(count.len() + 3);
            let label = truncate(&label, room);
            let pad = room.saturating_sub(label.chars().count());
            ListItem::new(Line::from(vec![
                Span::styled(label, style),
                Span::raw(" ".repeat(pad + 1)),
                Span::raw(count).dim(),
            ]))
        })
        .collect();
    let highlight = Style::new().bg(Color::DarkGray).fg(ACCENT).bold();
    let list = List::new(items).block(block).highlight_symbol("▌").highlight_style(highlight);
    f.render_stateful_widget(list, area, state);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let titles: Vec<Line> = Filter::ALL
        .iter()
        .enumerate()
        .map(|(i, &flt)| {
            let count = app.torrents.iter().filter(|t| flt.matches(t)).count();
            Line::from(format!("{} {} {}", i + 1, flt.label(), count))
        })
        .collect();

    // Mirror Tabs' layout (" title " separated by a 1-cell divider) for mouse hit-testing.
    let mut x = area.x;
    app.hit.tabs = titles
        .iter()
        .map(|t| {
            let w = t.width() as u16 + 2;
            let r = Rect { x, width: w, ..area }.intersection(area);
            x = x.saturating_add(w + 1);
            r
        })
        .collect();

    let idx = Filter::ALL.iter().position(|&x| x == app.filter).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(idx)
        .style(Style::new().dim())
        .highlight_style(Style::new().fg(ACCENT).bold().not_dim())
        .divider("│");
    f.render_widget(tabs, area);
}

/// Table columns: header, width (0 = fill), and drop order when space is tight (0 = always kept).
const COLUMNS: [(&str, u16, u8); 11] = [
    ("", 1, 0),
    ("Name", 0, 0),
    ("Size", 10, 6),
    ("Progress", 15, 0),
    ("State", 11, 0),
    ("↓", 11, 5),
    ("↑", 11, 4),
    ("Seeds", 9, 2),
    ("Peers", 9, 1),
    ("Ratio", 6, 3),
    ("ETA", 8, 7),
];

/// Width the Name column should keep before other columns are dropped.
const MIN_NAME_WIDTH: u16 = 30;

fn visible_columns(width: u16) -> Vec<usize> {
    let mut cols: Vec<usize> = (0..COLUMNS.len()).collect();
    let used = |cols: &[usize]| cols.iter().map(|&i| COLUMNS[i].1 + 1).sum::<u16>();
    let mut droppable: Vec<usize> = cols.iter().copied().filter(|&i| COLUMNS[i].2 > 0).collect();
    droppable.sort_by_key(|&i| COLUMNS[i].2);
    for d in droppable {
        if used(&cols) + MIN_NAME_WIDTH <= width {
            break;
        }
        cols.retain(|&i| i != d);
    }
    cols
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let cols = visible_columns(area.width.saturating_sub(2));
    let pick = |cells: Vec<Cell<'static>>| -> Vec<Cell<'static>> {
        cells.into_iter().enumerate().filter(|(i, _)| cols.contains(i)).map(|(_, c)| c).collect()
    };

    let header = Row::new(pick(COLUMNS.iter().map(|c| Cell::from(c.0)).collect()))
        .style(Style::new().fg(ACCENT).bold());

    let rows = app.view.iter().map(|&i| {
        let t = &app.torrents[i];
        let mark = if app.marked.contains(&t.hash) { "▌" } else { " " };
        let (state_label, state_color) = state_info(&t.state);
        Row::new(pick(vec![
            Cell::from(mark).fg(Color::Yellow),
            Cell::from(t.name.clone()),
            Cell::from(bytes(t.size)),
            Cell::from(progress_bar(t.progress, 9)).fg(if t.progress >= 1.0 { Color::Green } else { Color::Blue }),
            Cell::from(state_label).fg(state_color),
            Cell::from(speed_or_blank(t.dlspeed)).fg(Color::Green),
            Cell::from(speed_or_blank(t.upspeed)).fg(Color::Magenta),
            Cell::from(format!("{}({})", t.num_seeds, t.num_complete)),
            Cell::from(format!("{}({})", t.num_leechs, t.num_incomplete)),
            Cell::from(format!("{:.2}", t.ratio)),
            Cell::from(eta(t.eta)),
        ]))
        .style(if app.marked.contains(&t.hash) { Style::new().fg(Color::Yellow) } else { Style::new() })
    });

    let widths: Vec<Constraint> = cols
        .iter()
        .map(|&i| match COLUMNS[i].1 {
            0 => Constraint::Fill(1),
            w => Constraint::Length(w),
        })
        .collect();

    let sort_dir = if app.reverse { "↓" } else { "↑" };
    let mut title = vec![Span::raw(format!(" {} torrents ", app.view.len()))];
    match &app.category {
        Category::All => {}
        Category::None => title.push(Span::styled("· uncategorized ", Style::new().fg(ACCENT))),
        Category::Named(n) => title.push(Span::styled(format!("· {n} "), Style::new().fg(ACCENT))),
    }
    match &app.tag {
        TagFilter::All => {}
        TagFilter::None => title.push(Span::styled("· untagged ", Style::new().fg(ACCENT))),
        TagFilter::Named(n) => title.push(Span::styled(format!("· #{n} "), Style::new().fg(ACCENT))),
    }
    title.push(Span::raw(format!("· sort: {} {sort_dir} ", app.sort.label())).dim());
    if !app.search.is_empty() || matches!(app.mode, Mode::Search) {
        title.push(Span::styled(format!("· /{} ", app.search), Style::new().fg(Color::Yellow)));
    }
    if !app.marked.is_empty() {
        title.push(Span::styled(format!("· {} marked ", app.marked.len()), Style::new().fg(Color::Yellow)));
    }

    let block = pane().title(Line::from(title));

    app.page_size = area.height.saturating_sub(3).max(1) as usize;
    app.hit.table = area;

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    if app.view.is_empty() {
        f.render_widget(table, area);
        let msg = if app.connected { "No torrents" } else { "Not connected" };
        let inner = Rect { y: area.y + area.height / 2, height: 1, ..area };
        f.render_widget(Line::from(msg).dim().centered(), inner);
    } else {
        f.render_stateful_widget(table, area, &mut app.table);
    }
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let Some(t) = app.selected() else {
        f.render_widget(pane(), area);
        let mid = Rect { y: area.y + area.height / 2, height: 1, ..area };
        f.render_widget(Line::from("No torrent selected").dim().centered(), mid);
        return;
    };
    let kv = |k: &'static str, v: String| {
        Line::from(vec![Span::styled(format!("{k:>10} "), Style::new().fg(ACCENT)), Span::raw(v)])
    };
    let added = if t.added_on > 0 { fmt_age(t.added_on) } else { "-".into() };
    let left = vec![
        kv("Hash", t.hash.clone()),
        kv("Save path", t.save_path.clone()),
        kv("Tracker", if t.tracker.is_empty() { "-".into() } else { t.tracker.clone() }),
        kv("Category", if t.category.is_empty() { "-".into() } else { t.category.clone() }),
        kv("Tags", if t.tags.is_empty() { "-".into() } else { t.tags.clone() }),
        kv("Added", added),
    ];
    let right = vec![
        kv("Progress", format!("{:.1}%", t.progress * 100.0)),
        kv("Downloaded", bytes(t.downloaded)),
        kv("Uploaded", bytes(t.uploaded)),
        kv("Left", bytes(t.amount_left)),
        kv("Ratio", format!("{:.3}", t.ratio)),
        kv("State", t.state.clone()),
    ];

    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dim())
        .title(Line::from(format!(" {} ", t.name)).bold());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [l, r] = Layout::horizontal([Constraint::Fill(2), Constraint::Fill(1)]).areas(inner);
    f.render_widget(Paragraph::new(left), l);
    f.render_widget(Paragraph::new(right), r);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Mode::Search = app.mode {
        let line = Line::from(vec![
            Span::styled(" / ", Style::new().fg(Color::Black).bg(Color::Yellow)),
            Span::raw(format!(" {}", app.search)),
            Span::styled("█", Style::new().fg(Color::Yellow)),
            Span::raw("   enter: keep  esc: clear").dim(),
        ]);
        f.render_widget(line, area);
        return;
    }

    if let Some((msg, err, at)) = &app.status
        && (*err || at.elapsed() < Duration::from_secs(4))
    {
        let style = if *err { Style::new().fg(Color::Red) } else { Style::new().fg(Color::Green) };
        f.render_widget(Line::styled(format!(" {msg}"), style), area);
        return;
    }

    let keys = [
        ("p", "pause/resume"),
        ("a", "add"),
        ("d", "delete"),
        ("space", "mark"),
        ("/", "search"),
        ("s", "sort"),
        ("j/k", "categories"),
        ("J/K", "tags"),
        ("C", "set category"),
        ("T", "set tags"),
        ("?", "help"),
        ("q", "quit"),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (k, v) in keys {
        spans.push(Span::styled(k, Style::new().fg(ACCENT).bold()));
        spans.push(Span::raw(format!(" {v}  ")).dim());
    }
    f.render_widget(Line::from(spans), area);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn popup(title: &str) -> Block<'_> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(format!(" {title} ")).bold())
}

fn draw_add_form(f: &mut Frame, form: &AddForm, save_hint: Option<(String, &str)>) {
    let area = centered(f.area(), 90, 8);
    f.render_widget(Clear, area);
    let block = popup("Add torrent");
    let inner = block.inner(area);
    f.render_widget(block, area);

    const LABEL_W: usize = 12;
    // Inner width minus the 1-cell side padding and the label column.
    let value_w = (inner.width as usize).saturating_sub(LABEL_W + 2);
    let label = |name: &str, field: AddField| {
        if form.field == field {
            Span::styled(format!("▸ {name:<w$}", w = LABEL_W - 2), Style::new().fg(ACCENT).bold())
        } else {
            Span::raw(format!("  {name:<w$}", w = LABEL_W - 2)).dim()
        }
    };
    // Text fields show their tail so the cursor end stays visible.
    let text = |value: &str, field: AddField, placeholder: &str| -> Vec<Span<'static>> {
        let active = form.field == field;
        let mut spans = Vec::new();
        if value.is_empty() {
            if active {
                spans.push(Span::styled("█", Style::new().fg(ACCENT)));
            }
            spans.push(Span::raw(placeholder.to_string()).dim());
            return spans;
        } else {
            let n = value.chars().count();
            let shown = if n + 1 > value_w {
                let tail: String = value.chars().skip(n + 2 - value_w).collect();
                format!("…{tail}")
            } else {
                value.to_string()
            };
            spans.push(Span::raw(shown));
        }
        if active {
            spans.push(Span::styled("█", Style::new().fg(ACCENT)));
        }
        spans
    };

    let save_placeholder = match save_hint {
        Some((path, source)) => format!("{path}  ({source})"),
        None => "(qBittorrent default)".to_string(),
    };
    let cat_name = form.category.as_deref().unwrap_or("(none)");
    let cat_style = if form.category.is_some() { Style::new() } else { Style::new().dim() };
    let category = if form.field == AddField::Category {
        vec![
            Span::styled("‹ ", Style::new().fg(ACCENT)),
            Span::styled(cat_name.to_string(), cat_style.bold()),
            Span::styled(" ›", Style::new().fg(ACCENT)),
        ]
    } else {
        vec![Span::styled(cat_name.to_string(), cat_style)]
    };

    let line = |l: Span<'static>, mut v: Vec<Span<'static>>| {
        v.insert(0, l);
        Line::from(v)
    };
    let lines = vec![
        line(label("Source", AddField::Source), text(&form.source, AddField::Source, "magnet link, URL or .torrent file")),
        line(label("Category", AddField::Category), category),
        line(label("Save to", AddField::SavePath), text(&form.save_path, AddField::SavePath, &save_placeholder)),
        completion_line(form, value_w),
        {
            // Keys specific to the active field first, then the common ones.
            let field_keys: &[(&str, &str)] = match form.field {
                AddField::Source => &[("tab", "complete"), ("→", "accept")],
                AddField::Category => &[("←/→", "change")],
                AddField::SavePath => &[("→", "edit shown path")],
            };
            let common = [("↑↓", "field"), ("^u", "clear"), ("enter", "add"), ("esc", "cancel")];
            let mut spans = Vec::new();
            for (k, v) in field_keys.iter().chain(common.iter()) {
                spans.push(Span::styled(*k, Style::new().fg(ACCENT)));
                spans.push(Span::raw(format!(" {v}  ")).dim());
            }
            Line::from(spans)
        }
        .centered(),
    ];
    let pad = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), y: inner.y + 1, height: inner.height.saturating_sub(1) };
    f.render_widget(Paragraph::new(lines), pad);
}

/// Candidate file names under the source field, with the one being cycled through highlighted.
fn completion_line(form: &AddForm, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(12))];
    let mut used = 0;
    for (i, c) in form.completions.iter().enumerate() {
        let name = c.trim_end_matches('/').rsplit('/').next().unwrap_or(c);
        let name = if c.ends_with('/') { format!("{name}/") } else { name.to_string() };
        if used + name.chars().count() + 2 > width {
            spans.push(Span::raw(format!("+{}", form.completions.len() - i)).dim());
            break;
        }
        used += name.chars().count() + 2;
        let style = if form.completion_idx == Some(i) { Style::new().fg(ACCENT).bold() } else { Style::new().dim() };
        spans.push(Span::styled(name, style));
        spans.push(Span::raw("  "));
    }
    Line::from(spans)
}

fn draw_picker(f: &mut Frame, app: &App, p: &Picker) {
    let options = app.picker_options(&p.filter);
    let rows = options.len().clamp(1, 12) as u16;
    let area = centered(f.area(), 50, rows + 6);
    f.render_widget(Clear, area);
    let n = p.targets.len();
    let title = format!("Set category ({n} torrent{})", if n == 1 { "" } else { "s" });
    let block = popup(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [input, list, hint] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let line = Line::from(vec![
        Span::styled(" filter ", Style::new().fg(ACCENT)),
        Span::raw(p.filter.clone()),
        Span::styled("█", Style::new().fg(ACCENT)),
    ]);
    f.render_widget(line, input);

    let items: Vec<ListItem> = options
        .iter()
        .map(|o| match o {
            PickOption::None => ListItem::new(Line::from("(none)").italic()),
            PickOption::Existing(name) => ListItem::new(name.clone()),
            PickOption::Create(name) => {
                ListItem::new(Line::from(vec![Span::raw("+ create ").fg(Color::Green), Span::raw(format!("\u{201c}{name}\u{201d}"))]))
            }
        })
        .collect();
    let list_w = List::new(items)
        .highlight_symbol("▌")
        .highlight_style(Style::new().bg(Color::DarkGray).fg(ACCENT).bold());
    let mut state = ListState::default().with_selected(Some(p.idx));
    f.render_stateful_widget(list_w, list, &mut state);

    f.render_widget(Line::from("↑/↓ choose · enter apply · esc cancel").dim().centered(), hint);
}

fn draw_tag_picker(f: &mut Frame, p: &TagPicker) {
    let options = p.options();
    let rows = options.len().clamp(1, 12) as u16;
    let area = centered(f.area(), 50, rows + 6);
    f.render_widget(Clear, area);
    let n = p.targets.len();
    let title = format!("Tags ({n} torrent{})", if n == 1 { "" } else { "s" });
    let block = popup(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [input, list, hint] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let line = Line::from(vec![
        Span::styled(" filter ", Style::new().fg(ACCENT)),
        Span::raw(p.filter.clone()),
        Span::styled("█", Style::new().fg(ACCENT)),
    ]);
    f.render_widget(line, input);

    let items: Vec<ListItem> = if options.is_empty() {
        vec![ListItem::new(Line::from("no tags yet: type a name to create one").dim())]
    } else {
        options
            .iter()
            .map(|o| match o {
                Ok(tag) => {
                    let (mark, style) = match p.check(tag) {
                        Check::All => ("[x] ", Style::new().fg(Color::Green)),
                        Check::Some => ("[-] ", Style::new().fg(Color::Yellow)),
                        Check::None => ("[ ] ", Style::new().dim()),
                    };
                    ListItem::new(Line::from(vec![Span::styled(mark, style), Span::raw(tag.clone())]))
                }
                Err(name) => ListItem::new(Line::from(vec![
                    Span::raw("+ create ").fg(Color::Green),
                    Span::raw(format!("\u{201c}{name}\u{201d}")),
                ])),
            })
            .collect()
    };
    let list_w = List::new(items)
        .highlight_symbol("▌")
        .highlight_style(Style::new().bg(Color::DarkGray).fg(ACCENT).bold());
    let selected = (!options.is_empty()).then_some(p.idx);
    let mut state = ListState::default().with_selected(selected);
    f.render_stateful_widget(list_w, list, &mut state);

    f.render_widget(Line::from("↑/↓ choose · enter/space toggle · esc done").dim().centered(), hint);
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let names: Vec<String> = if app.marked.is_empty() {
        app.selected().map(|t| vec![t.name.clone()]).unwrap_or_default()
    } else {
        app.torrents.iter().filter(|t| app.marked.contains(&t.hash)).map(|t| t.name.clone()).collect()
    };
    let shown = names.len().min(5);
    let area = centered(f.area(), 70, shown as u16 + 6);
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = names.iter().take(5).map(|n| Line::from(format!("  {n}"))).collect();
    if names.len() > 5 {
        lines.push(Line::from(format!("  …and {} more", names.len() - 5)).dim());
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("y", Style::new().fg(Color::Yellow).bold()),
        Span::raw(" remove torrent   "),
        Span::styled("D", Style::new().fg(Color::Red).bold()),
        Span::raw(" remove + delete data   "),
        Span::styled("any", Style::new().bold()),
        Span::raw(" cancel"),
    ]));
    let title = format!("Delete {} torrent{}?", names.len(), if names.len() == 1 { "" } else { "s" });
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(popup(&title)), area);
}

fn draw_help(f: &mut Frame) {
    let rows = [
        ("j/k", "next / previous category"),
        ("J/K", "next / previous tag"),
        ("↑/↓", "move in torrent list"),
        ("PgUp/PgDn Home/End", "page / jump in list"),
        ("←/→ tab 1-6", "switch state filter"),
        ("space", "mark / unmark torrent"),
        ("esc", "clear marks / search"),
        ("p", "pause / resume"),
        ("d", "delete (asks to confirm)"),
        ("C", "set category (type a new name to create)"),
        ("T", "tag / untag (type a new name to create)"),
        ("c", "force recheck"),
        ("R", "reannounce to trackers"),
        ("a", "add torrent (source, category, path)"),
        ("/", "search by name"),
        ("s / S", "cycle sort / reverse"),
        ("r", "refresh now"),
        ("q", "quit"),
    ];
    let area = centered(f.area(), 62, rows.len() as u16 + 5);
    f.render_widget(Clear, area);
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| Line::from(vec![Span::styled(format!("{k:>19}  "), Style::new().fg(ACCENT).bold()), Span::raw(*v)]))
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::from("mouse: click select · right-click mark · wheel scroll").dim().centered());
    lines.push(Line::from("press any key to close").dim().centered());
    f.render_widget(Paragraph::new(lines).block(popup("Keys")), area);
}

fn state_info(state: &str) -> (&'static str, Color) {
    match state {
        "downloading" => ("Downloading", Color::Green),
        "forcedDL" => ("[F] Down", Color::Green),
        "metaDL" | "forcedMetaDL" => ("Metadata", Color::Blue),
        "stalledDL" => ("Stalled", Color::Yellow),
        "queuedDL" | "queuedUP" => ("Queued", Color::Gray),
        "uploading" => ("Seeding", Color::Magenta),
        "forcedUP" => ("[F] Seed", Color::Magenta),
        "stalledUP" => ("Seeding", Color::DarkGray),
        "pausedDL" | "stoppedDL" => ("Paused", Color::Gray),
        "pausedUP" | "stoppedUP" => ("Completed", Color::Gray),
        "checkingDL" | "checkingUP" | "checkingResumeData" => ("Checking", Color::Cyan),
        "moving" => ("Moving", Color::Cyan),
        "allocating" => ("Allocating", Color::Cyan),
        "error" => ("Error", Color::Red),
        "missingFiles" => ("Missing", Color::Red),
        _ => ("Unknown", Color::DarkGray),
    }
}

fn progress_bar(p: f64, width: usize) -> String {
    let p = p.clamp(0.0, 1.0);
    let filled = (p * width as f64).round() as usize;
    format!("{}{} {:>3.0}%", "█".repeat(filled), "░".repeat(width - filled), p * 100.0)
}

pub fn bytes(n: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n.max(0) as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.1} {}", UNITS[u]) }
}

fn speed(n: i64) -> String {
    format!("{}/s", bytes(n))
}

fn speed_or_blank(n: i64) -> String {
    if n > 0 { speed(n) } else { String::new() }
}

fn eta(secs: i64) -> String {
    if !(0..8_640_000).contains(&secs) {
        return "∞".into();
    }
    if secs == 0 {
        return String::new();
    }
    fmt_duration(secs)
}

fn fmt_duration(secs: i64) -> String {
    let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

fn fmt_age(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format!("{} ago", fmt_duration((now - ts).max(0)))
}

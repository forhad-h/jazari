use crate::app::{App, FooterNote, HistoryFilter, View};
use crate::update::Notice;
use crate::usage::{Pane, Usage};
use crate::model::{ContextInfo, DAY_FMT, EventKind, Level, fmt_tokens, MAIN_AGENT, Session, SortKey, Status, TimelineEntry, TokenUsage, Via};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use crate::badges::{Badges, Level as BadgeLevel};
use crate::share_view::{Mark, Screen, ShareView};
use ratatui::symbols::Marker;
use ratatui::widgets::canvas::{Canvas, Line as CanvasLine};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Sparkline, Table,
    TableState, Wrap,
};

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
const ACCENT: Color = Color::Cyan;
/// Secondary text (hints, units, details): a fixed mid gray, lighter than the terminal's own
/// dark gray, which many themes draw too faint to read. It stays above 4.5:1 on the background and
/// on the selected row, even on a warm "reading mode" display.
const MUTED: Color = Color::Indexed(247);
/// Panel borders: darker than text, so the frame recedes behind what it holds.
const FRAME: Style = Style::new().fg(Color::Indexed(240));
/// Titles draw over the border, so they set their own color instead of taking the frame's.
const TITLE: Style = Style::new().fg(Color::Reset);

const HIGHLIGHT: Style = Style::new().bg(Color::Indexed(236)).add_modifier(Modifier::BOLD);

/// A table with the dashboard's standard spacing and selection highlight.
fn styled_table<'a>(rows: impl IntoIterator<Item = Row<'a>>, widths: &[Constraint], block: Block<'a>) -> Table<'a> {
    Table::new(rows, widths.to_vec()).column_spacing(1).row_highlight_style(HIGHLIGHT).block(block)
}

fn right_cell(text: impl Into<String>) -> Cell<'static> {
    Cell::from(Line::from(text.into()).alignment(Alignment::Right))
}

fn via_span(via: Via) -> Span<'static> {
    match via {
        Via::Tool => dim("tool"),
        Via::Command => Span::styled("/cmd", Style::new().fg(ACCENT)),
    }
}

const RUN_WIDTHS: [Constraint; 9] = [
    Constraint::Length(3),
    Constraint::Length(12),
    Constraint::Length(18),
    Constraint::Fill(1),
    Constraint::Length(4),
    Constraint::Length(5),
    Constraint::Length(7),
    Constraint::Length(8),
    Constraint::Length(10),
];
const RUN_HEADER: [&str; 9] = ["  #", "TIME", "AGENT", "SKILL", "VIA", "TURNS", "OUT", "TOKENS", ""];

/// One row of a skill-run table (live history and the usage drill-down share the layout).
#[allow(clippy::too_many_arguments)]
fn run_row(i: usize, at: Option<&str>, agent: &str, skill: &str, via: Via, turns: u32, output: u64, total: u64, max: u64) -> Row<'static> {
    let agent_style = if agent == MAIN_AGENT { Style::new() } else { Style::new().fg(ACCENT) };
    Row::new(vec![
        Cell::from(dim(format!("{:>3}", i + 1))),
        Cell::from(Span::raw(fmt_local(at))),
        Cell::from(Span::styled(agent.to_string(), agent_style)),
        Cell::from(Span::styled(skill.to_string(), Style::new().fg(Color::Magenta).bold())),
        Cell::from(via_span(via)),
        right_cell(turns.to_string()),
        right_cell(fmt_tokens(output)),
        right_cell(format!("≈{}", fmt_tokens(total))),
        Cell::from(Span::styled(bar(weight(total as i64, max as i64), 10), Style::new().fg(Color::Magenta))),
    ])
}

fn runs_table(rows: Vec<Row<'static>>, block: Block<'static>) -> Table<'static> {
    styled_table(rows, &RUN_WIDTHS, block).header(Row::new(RUN_HEADER).style(Style::new().fg(MUTED)))
}

fn status_style(s: Status) -> (Style, &'static str) {
    match s {
        Status::Approve => (Style::new().fg(Color::Yellow).bold(), "APPROVE"),
        Status::Question => (Style::new().fg(Color::Magenta).bold(), "QUESTION"),
        Status::Error => (Style::new().fg(Color::Red).bold(), "ERROR"),
        Status::Active => (Style::new().fg(Color::Green), "ACTIVE"),
        Status::Idle => (Style::new().fg(Color::Blue), "IDLE"),
        Status::Ended => (Style::new().fg(MUTED), "ENDED"),
    }
}

fn status_glyph(s: Status, tick: u64) -> &'static str {
    match s {
        Status::Approve => "●",
        Status::Question => "?",
        Status::Error => "✗",
        Status::Active => SPINNER[(tick / 2 % 4) as usize],
        Status::Idle => "○",
        Status::Ended => "·",
    }
}

fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// Local wall-clock time for an ISO-8601 transcript timestamp.
fn fmt_local(iso: Option<&str>) -> String {
    let Some(t) = iso.and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) else { return "—".into() };
    let t = t.with_timezone(&chrono::Local);
    if t.date_naive() == chrono::Local::now().date_naive() {
        t.format("%H:%M:%S").to_string()
    } else {
        t.format("%b %d %H:%M").to_string()
    }
}

fn short_path(p: &str) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(&*h.to_string_lossy()).map(str::to_string)) {
        Some(rest) => format!("~{rest}"),
        None => p.to_string(),
    }
}

/// Green while there is room, yellow past 60%, red past 85%.
fn ratio_color(r: f64) -> Color {
    match Level::of(r) {
        Level::Critical => Color::Red,
        Level::Warn => Color::Yellow,
        Level::Ok => Color::Green,
    }
}

fn bar(ratio: f64, width: usize) -> String {
    let filled = ((ratio.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    "▰".repeat(filled) + &"▱".repeat(width - filled)
}

/// `▰▰▰▱▱▱ 42% · 412k left before auto-compact` with the whole gauge colored by fill.
fn context_spans(ctx: &ContextInfo, width: usize, long: bool) -> Vec<Span<'static>> {
    let r = ctx.used_ratio();
    let style = Style::new().fg(ratio_color(r));
    let approx = if ctx.estimated { "≈" } else { "" };
    let mut spans = vec![
        Span::styled(bar(r, width), style),
        Span::styled(format!(" {approx}{:.0}%", r * 100.0), style.bold()),
    ];
    if long {
        spans.push(Span::raw(format!("   {} left", fmt_tokens(ctx.left()))));
        spans.push(Span::styled(
            format!(" before auto-compact at {} · window {}", fmt_tokens(ctx.limit), fmt_tokens(ctx.window)),
            Style::new().fg(MUTED),
        ));
    }
    spans
}

pub fn draw(f: &mut Frame, app: &App) {
    let visible = app.visible();
    let note_height = if app.copy_note.is_some() { 4 } else { 0 };
    let status = status_row(app);
    let [header, body, copy_note, status_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(note_height),
        Constraint::Length(u16::from(status.is_some())),
        Constraint::Length(1),
    ])
    .areas(f.area());
    draw_header(f, header, app, &visible);
    if let Some(build) = status {
        // Reset times go first when the row doesn't fit.
        let line = Some(build(true)).filter(|l| l.width() <= status_area.width as usize).unwrap_or_else(|| build(false));
        f.render_widget(Paragraph::new(line), status_area);
    }

    let keys: Vec<(&str, String)> = match (app.view, app.selected_session()) {
        (View::Usage, _) if app.usage.drill.is_some() => {
            draw_drill(f, body, &app.usage);
            vec![("↑↓", "scroll".into()), ("esc", "back".into()), ("q", "quit".into())]
        }
        (View::Usage, _) => {
            draw_usage(f, body, &app.usage);
            vec![
                ("tab", "pane".into()),
                ("↑↓", "move".into()),
                ("enter", "open session".into()),
                ("p/P", format!("period: {}", app.usage.period_label())),
                ("r", "resync".into()),
                ("esc", "back".into()),
                ("q", "quit".into()),
            ]
        }
        (View::Clean, _) if app.clean.is_some() => {
            let clean = app.clean.as_ref().unwrap();
            draw_clean(f, body, clean, app.system.as_ref(), app.tick);
            clean.keys().into_iter().map(|(k, label)| (k, label.to_string())).chain([("q", "quit".to_string())]).collect()
        }
        (View::History, Some(s)) => {
            draw_history(f, body, app, s);
            vec![
                ("↑↓", "scroll".into()),
                ("f", format!("filter: {}", app.history_filter.label())),
                ("g/G", "top/bottom".into()),
                ("tab", "subagents".into()),
                ("y", "copy name+id".into()),
                ("b", "board".into()),
                ("esc", "back".into()),
                ("q", "quit".into()),
            ]
        }
        (View::Agents, Some(s)) => {
            draw_agents(f, body, app, s);
            vec![
                ("↑↓", "scroll".into()),
                ("g/G", "top/bottom".into()),
                ("tab", "details".into()),
                ("y", "copy name+id".into()),
                ("b", "board".into()),
                ("esc", "back".into()),
                ("q", "quit".into()),
            ]
        }
        (View::Board, _) => {
            draw_board(f, body, app);
            vec![
                ("↑↓", "scroll".into()),
                ("g/G", "top/bottom".into()),
                ("c", "launch command".into()),
                ("esc", "back".into()),
                ("q", "quit".into()),
            ]
        }
        _ => {
            draw_list(f, body, app, &visible);
            let ended = if app.show_ended { "hide ended" } else { "show ended" };
            vec![
                ("↑↓", "select".into()),
                ("enter", "details".into()),
                ("tab", "subagents".into()),
                ("y", "copy name+id".into()),
                ("u", "usage".into()),
                ("t/x", "message/stop".into()),
                ("D", "disk cleanup".into()),
                ("p", "share".into()),
                ("P", if app.pin.is_some() { "unpin" } else { "pin on top" }.into()),
                ("b", "board".into()),
                ("c", "launch cmd".into()),
                ("n", "new prompt".into()),
                ("a", ended.into()),
                ("m", format!("alerts: {}", if app.alerts_on { "on" } else { "off" })),
                ("s/S", format!("sort: {}", app.sort.label())),
                ("q", "quit".into()),
            ]
        }
    };
    let picker_digits = app.rules_picker.as_ref().map(|p| format!("space/1-{}", p.library.len().clamp(1, 9))).unwrap_or_default();
    let keys = match (&app.share, &app.prompt_view, &app.rules_picker) {
        (Some(share), ..) => {
            draw_share(f, body, share, app.badges.as_ref(), app.cell_aspect, app.tick);
            share.keys().into_iter().map(|(k, label)| (k, label.to_string())).collect()
        }
        (None, Some(view), _) => {
            draw_prompt_view(f, body, view, app.tick);
            view.keys().into_iter().map(|(k, label)| (k, label.to_string())).collect()
        }
        (None, None, Some(picker)) => {
            draw_rules_picker(f, body, picker);
            if picker.deleting.is_some() {
                vec![("y", "delete".into()), ("any key", "keep".into())]
            } else if picker.versions.is_some() && picker.mode.is_none() {
                vec![("↑↓", "move".into()), ("enter", "restore".into()), ("esc", "back".into())]
            } else if picker.mode.is_some() {
                vec![("↑↓", "move".into()), ("1-3", "pick".into()), ("enter", "copy".into()), ("esc", "back".into())]
            } else {
                let mut keys = vec![
                    ("↑↓", "move".into()),
                    (picker_digits.as_str(), "tick".into()),
                    ("enter", "next".into()),
                    ("n", if picker.library.get(picker.cursor).is_some_and(|r| r.kind == crate::rules::Kind::Skill) { "new skill" } else { "new prompt" }.into()),
                    ("e", "edit".into()),
                    ("d", "delete".into()),
                    ("v", "versions".into()),
                    ("esc", "cancel".into()),
                ];
                if picker.restorable.is_some() {
                    keys.insert(7, ("u", "undo delete".into()));
                }
                keys
            }
        }
        (None, None, None) => keys,
    };
    let keys = if app.compose.is_some() {
        vec![("enter", "send".into()), ("esc", "cancel".into())]
    } else if let Some(c) = &app.control {
        vec![("y", if c.interrupt { "interrupt now" } else { "stop" }.into()), ("any key", "keep it running".into())]
    } else {
        keys
    };
    if let Some(c) = &app.compose {
        draw_compose(f, body, c, app.tick);
    } else if let Some(c) = &app.control {
        draw_control(f, body, c);
    }
    let note = app.footer_note().map(|n| match n {
        FooterNote::Error(e) => (format!("{e} "), Color::Red),
        FooterNote::Info(i) => (i.to_string(), Color::Green),
        FooterNote::Badge(a) => (format!("★ {a} "), Color::Green),
        FooterNote::Update(Notice::Available(v)) => (format!("v{v} available · run `jazari update` "), Color::Yellow),
        FooterNote::Update(Notice::Installed(v)) => (format!("updated to v{v} · restart jazari "), Color::Yellow),
    });
    if let Some(note) = &app.copy_note {
        draw_copy_note(f, copy_note, note);
    }
    let footer = match note {
        Some((text, color)) => {
            let [keys, note] =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(text.chars().count() as u16)]).areas(footer);
            f.render_widget(Paragraph::new(Span::styled(text, Style::new().fg(color))), note);
            keys
        }
        None => footer,
    };
    f.render_widget(Paragraph::new(key_hints(&keys, footer.width as usize)), footer);
}

/// The footer's `key label` pairs, in order of importance, cut to `width`. The last pair (quit or
/// back) always stays, and `…` marks where pairs were left out.
fn key_hints(keys: &[(&str, String)], width: usize) -> Line<'static> {
    let pair = |(k, label): &(&str, String)| [k.to_string().bold(), format!(" {label}  ").fg(MUTED)];
    let pair_width = |p: &(&str, String)| p.0.chars().count() + p.1.chars().count() + 3;
    let total = 1 + keys.iter().map(pair_width).sum::<usize>();
    let mut spans = vec![Span::raw(" ")];
    if total <= width {
        spans.extend(keys.iter().flat_map(pair));
        return Line::from(spans);
    }
    let Some((last, rest)) = keys.split_last() else { return Line::default() };
    let mut room = width.saturating_sub(1 + pair_width(last) + 3);
    for p in rest {
        let w = pair_width(p);
        if w > room {
            break;
        }
        room -= w;
        spans.extend(pair(p));
    }
    spans.push("…  ".fg(MUTED));
    spans.extend(pair(last));
    Line::from(spans)
}

fn draw_header(f: &mut Frame, area: Rect, app: &App, visible: &[&Session]) {
    let waiting = visible.iter().filter(|s| s.status.needs_user()).count();
    let active = visible.iter().filter(|s| s.status == Status::Active).count();
    let mut left = vec![
        Span::styled(" jazari ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
        Span::raw(format!("  {} sessions", visible.len())),
        Span::styled(format!(" · {active} active"), Style::new().fg(Color::Green)),
    ];
    if waiting > 0 {
        left.push(Span::styled(format!(" · {waiting} need you"), Style::new().fg(Color::Yellow).bold()));
    }
    let left = Line::from(left);
    let room = (area.width as usize).saturating_sub(left.width() + 2);
    f.render_widget(Paragraph::new(left), area);

    let (today, all_time) = app.counter.totals();
    // A narrow terminal drops the all-time total, then today's.
    let build = |drop: u8| {
        let mut right = Vec::new();
        if drop < 2 {
            right.extend([Span::raw("today "), Span::styled(fmt_tokens(today.max(0) as u64), Style::new().bold())]);
        }
        if drop < 1 {
            right.extend([
                Span::styled(" · ", Style::new().fg(MUTED)),
                Span::raw("all-time "),
                Span::styled(crate::model::fmt_count(all_time.max(0) as u64), Style::new().bold()),
            ]);
        }
        right.push(Span::raw(" "));
        Line::from(right)
    };
    let right = (0..=2).map(build).find(|l| l.width() <= room).unwrap_or_else(|| build(2));
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

/// The row above the footer, laid out like Claude Code's own status line: the selected session's
/// context, then the 5-hour and 7-day limits. None when there is nothing to show. The builder
/// takes whether to include reset times.
fn status_row(app: &App) -> Option<impl Fn(bool) -> Line<'static>> {
    let ctx = app.selected_session().and_then(|s| s.context(app.compact_window));
    let limits = app.rate_limits();
    if ctx.is_none() && limits.is_none() {
        return None;
    }
    Some(move |resets: bool| {
        let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
        if let Some(ctx) = &ctx {
            let r = ctx.used_ratio();
            let approx = if ctx.estimated { "≈" } else { "" };
            parts.push(vec![
                Span::styled(format!("ctx {approx}{:.0}%", r * 100.0), Style::new().fg(ratio_color(r)).bold()),
                dim(format!(" · {} left", fmt_tokens(ctx.left()))),
            ]);
        }
        for (label, w) in limits.into_iter().flat_map(|(five, seven)| [("5h", five), ("7d", seven)]) {
            if let Some(w) = w {
                let mut spans = vec![Span::styled(format!("{label} {:.0}%", w.used_pct), Style::new().fg(ratio_color(w.used_pct / 100.0)).bold())];
                if let Some(t) = w.resets_at.filter(|_| resets) {
                    spans.push(dim(format!(" ↻{}", crate::model::fmt_reset(t))));
                }
                parts.push(spans);
            }
        }
        let mut spans = vec![Span::raw(" ")];
        for (i, part) in parts.into_iter().enumerate() {
            if i > 0 {
                spans.push(dim(" · "));
            }
            spans.extend(part);
        }
        Line::from(spans)
    })
}

/// CPU, memory, swap and disk meters, one per line, each colored by how close it is to trouble.
/// Swap gets its own line when the machine has any. Narrow panels get shorter bars.
fn system_lines(s: &crate::system::Snapshot, width: u16) -> Vec<Line<'static>> {
    use crate::disk::Level as Disk;
    let wide = width >= 60;
    let bar_w = if wide { 10 } else { 6 };
    let meter = |name: &'static str, ratio: f64, color: Color| {
        let style = Style::new().fg(color);
        vec![
            label(name),
            Span::styled(bar(ratio, bar_w), style),
            Span::styled(format!(" {:>3.0}%", ratio * 100.0), style.bold()),
            Span::raw("  "),
        ]
    };
    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    let mut lines = Vec::new();
    let mut spans = match s.cpu_pct {
        Some(cpu) => {
            let r = cpu as f64 / 100.0;
            meter("cpu", r, ratio_color(r))
        }
        None => vec![label("cpu"), dim("…  ")],
    };
    // Idle cores, the CPU's "free". Load replaces it once processes queue at more than twice the
    // core count, since the queue matters more than idle capacity then.
    let room = (width as usize).saturating_sub(spans.iter().map(|x| x.width()).sum());
    let fits = |t: &str| t.chars().count() <= room;
    if s.cores > 0 && s.load[0] > s.cores as f64 * 2.0 {
        let mut text = format!("load {:.1}", s.load[0]);
        let per_core = format!("{text} · {:.1}× cores", s.load[0] / s.cores as f64);
        if fits(&per_core) {
            text = per_core;
        }
        spans.push(Span::styled(text, Style::new().fg(Color::Red)));
    } else if let (Some(cpu), true) = (s.cpu_pct, s.cores > 0) {
        let idle = s.cores as f64 * (1.0 - (cpu as f64 / 100.0).clamp(0.0, 1.0));
        let long = format!("{idle:.1} of {} cores idle", s.cores);
        let short = format!("{idle:.1} cores idle");
        if fits(&long) {
            spans.push(Span::raw(long));
        } else if fits(&short) {
            spans.push(Span::raw(short));
        }
    }
    lines.push(Line::from(spans));
    let mem = s.mem_used_ratio();
    let mut spans = meter("memory", mem, ratio_color(mem));
    spans.push(Span::raw(format!("{:.1} GiB free", gib(s.mem_available))));
    lines.push(Line::from(spans));
    if s.swap_total > 0 {
        // macOS grows swap on demand, so a full bar alone means little: the color comes from
        // swap in use against RAM, the same measure as the swap concern.
        let vs_ram = if s.mem_total > 0 { s.swap_used as f64 / s.mem_total as f64 } else { 0.0 };
        let color = if vs_ram > 0.5 { Color::Red } else if vs_ram > 0.25 { Color::Yellow } else { Color::Green };
        let mut spans = meter("swap", s.swap_used as f64 / s.swap_total as f64, color);
        spans.push(Span::raw(format!("{:.1} of {:.1} GiB used", gib(s.swap_used), gib(s.swap_total))));
        lines.push(Line::from(spans));
    }
    if let Some((level, r)) = &s.disk {
        let color = match level {
            Disk::Ok => Color::Green,
            Disk::Notice | Disk::Warning => Color::Yellow,
            Disk::Critical | Disk::Emergency => Color::Red,
        };
        let mut spans = meter("disk", r.used_pct() / 100.0, color);
        spans.push(Span::raw(format!("{:.1} GB free", r.free_gb())));
        if *level >= Disk::Warning {
            spans.push(Span::styled(format!(" · {}", level.key()), Style::new().fg(color).bold()));
        }
        lines.push(Line::from(spans));
    }
    lines
}

/// What jazari itself uses: its CPU, memory and data folder.
fn own_line(s: &crate::system::Snapshot) -> Line<'static> {
    let mut text = String::new();
    if let Some(cpu) = s.own_cpu {
        text.push_str(&format!("cpu {cpu:.1}% · "));
    }
    text.push_str(&format!("mem {:.0} MB", s.own_mem as f64 / 1e6));
    if let Some(n) = s.own_data {
        text.push_str(&format!(" · data {}", crate::clean::fmt_bytes(n)));
    }
    Line::from(vec![label("jazari"), dim(text)])
}

/// The bottom-right panel: the machine's CPU, memory, swap and disk, then jazari's own share.
fn draw_system(f: &mut Frame, area: Rect, snap: Option<&crate::system::Snapshot>) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(" System ").bold());
    let lines = match snap {
        Some(s) => {
            let mut lines = system_lines(s, block.inner(area).width);
            lines.push(own_line(s));
            lines
        }
        None => vec![Line::from(dim("reading…"))],
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_clean(f: &mut Frame, area: Rect, v: &crate::clean_view::CleanView, sys: Option<&crate::system::Snapshot>, tick: u64) {
    use crate::clean::{Tier, fmt_bytes};
    let dim = |t: String| Span::styled(t, Style::new().fg(MUTED));
    let mut title = vec![Span::styled(" Disk cleanup ", Style::new().fg(ACCENT).bold())];
    if let Some((_, r)) = sys.and_then(|s| s.disk.as_ref()) {
        title.push(dim(format!("· {:.0}% full, {:.1} GB free ", r.used_pct(), r.free_gb())));
    }
    let mut right = vec![dim(format!(" auto-clean {} ", if v.auto { "on" } else { "off" }))];
    if let Some(n) = v.freed_30d.filter(|n| *n > 0) {
        right.push(dim(format!("· freed {} in 30 days ", fmt_bytes(n))));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(title))
        .title(Line::from(right).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let status_lines = if v.confirm.is_some() || v.busy() || v.judging || !v.log.is_empty() { (v.log.len() as u16 + 1).clamp(2, 6) } else { 0 };
    let [list, status] = Layout::vertical([Constraint::Min(3), Constraint::Length(status_lines)]).areas(inner);
    let heads = [
        (Tier::A, "A · rebuilt automatically, holds no data", Color::Green),
        (Tier::B, "B · rebuilt, but slow to get back", Color::Yellow),
        (Tier::C, "C · holds data: one at a time, after you check it", Color::Red),
        (Tier::Info, "Report only · freed by the app or tool that owns it", MUTED),
    ];
    let mut lines = Vec::new();
    let mut selected_line = 0;
    for (tier, head, color) in heads {
        let items: Vec<(usize, &crate::clean::Item)> = v.items.iter().enumerate().filter(|(_, i)| i.tier == tier).collect();
        if items.is_empty() {
            continue;
        }
        let total: u64 = items.iter().map(|(_, i)| i.bytes).sum();
        lines.push(Line::from(vec![Span::styled(format!(" {head}"), Style::new().fg(color).bold()), dim(format!("  {}", fmt_bytes(total)))]));
        for (idx, item) in items {
            let sel = idx == v.selected;
            if sel {
                selected_line = lines.len();
            }
            let style = if sel { Style::new().reversed() } else { Style::new() };
            let id = if item.id.starts_with('/') { short_path(&item.id) } else { item.id.clone() };
            let mut spans = vec![
                Span::styled(format!("   {:>9}  {}", fmt_bytes(item.bytes), item.what), style),
                dim(format!("  {id}")),
            ];
            if !item.note.is_empty() {
                spans.push(dim(format!("  · {}", item.note)));
            }
            lines.push(Line::from(spans));
            if let Some(verdict) = v.verdict(item) {
                let color = match verdict.call.as_str() {
                    "safe" => Color::Green,
                    "suggest" => Color::Yellow,
                    _ => Color::Red,
                };
                lines.push(Line::from(vec![
                    Span::raw("              "),
                    Span::styled(format!("Claude: {}", verdict.call), Style::new().fg(color)),
                    dim(format!(" · {}", verdict.reason)),
                ]));
            }
        }
        lines.push(Line::from(""));
    }
    if lines.is_empty() {
        let msg = if v.scanning { "Measuring caches, idle projects and Docker…" } else { "Nothing to clean." };
        lines.push(Line::from(dim(format!(" {msg}"))));
    }
    let skip = selected_line.saturating_sub(list.height.saturating_sub(2) as usize);
    f.render_widget(Paragraph::new(lines).scroll((skip as u16, 0)), list);

    let mut out = Vec::new();
    let spin = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"][(tick as usize) % 10];
    if let Some(q) = v.question() {
        out.push(Line::from(Span::styled(format!(" {q}  y to remove, any other key keeps it"), Style::new().fg(Color::Yellow).bold())));
    } else if v.running {
        out.push(Line::from(dim(format!(" {spin} removing…"))));
    } else if v.judging {
        out.push(Line::from(dim(format!(" {spin} asking Claude about the data-holding items…"))));
    } else if v.scanning && !v.items.is_empty() {
        out.push(Line::from(dim(format!(" {spin} measuring…"))));
    }
    out.extend(v.log.iter().map(|l| Line::from(format!(" {l}"))));
    f.render_widget(Paragraph::new(out).wrap(Wrap { trim: false }), status);
}

fn draw_list(f: &mut Frame, area: Rect, app: &App, visible: &[&Session]) {
    let selected = app.selected_session();
    // Session info on the left, the machine on the right; the taller side sets the height.
    let session_rows = selected.map_or(0, |s| 3 + u16::from(s.flag.is_some()) + u16::from(s.said.is_some() || s.waiting > 0 || s.stop_requested));
    let system_rows = app.system.as_ref().map_or(1, |s| 3 + u16::from(s.disk.is_some()) + u16::from(s.swap_total > 0));
    let [table, bottom] =
        Layout::vertical([Constraint::Min(4), Constraint::Length(2 + session_rows.max(system_rows))]).areas(area);
    if visible.is_empty() {
        draw_empty(f, table, app);
    } else {
        draw_table(f, table, app, visible);
    }
    match selected {
        Some(s) => {
            let [details, system] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(bottom);
            draw_details(f, details, s);
            draw_system(f, system, app.system.as_ref());
        }
        None => draw_system(f, bottom, app.system.as_ref()),
    }
}

fn draw_empty(f: &mut Frame, area: Rect, app: &App) {
    let msg = if app.hooks_installed {
        vec![
            Line::from("No sessions yet.".bold()),
            Line::from(""),
            Line::from("Start (or restart) a Claude Code session and it will appear here.".fg(MUTED)),
            Line::from(""),
            Line::from(vec![
                "Press ".fg(MUTED),
                "c".bold(),
                " to copy a command that starts Claude Code with coordination or other prompts.".fg(MUTED),
            ]),
        ]
    } else {
        vec![
            Line::from("jazari hooks are not installed.".bold()),
            Line::from(""),
            Line::from(vec!["Run ".fg(MUTED), "jazari install".fg(ACCENT).bold(), ", then restart your Claude Code sessions.".fg(MUTED)]),
        ]
    };
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(FRAME).title_style(TITLE);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let height = msg.len() as u16;
    let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(height), Constraint::Fill(1)]).areas(inner);
    f.render_widget(Paragraph::new(msg).alignment(Alignment::Center), mid);
}

/// The launch command to copy by hand, drawn without borders so a mouse selection copies only the
/// command. When it is wider than the terminal, the flag and the path go on separate lines.
/// A small box over the list for typing a message to one session.
fn draw_compose(f: &mut Frame, area: Rect, c: &crate::app::Compose, tick: u64) {
    let w = area.width.saturating_sub(8).min(100);
    let [_, rect, _] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(w), Constraint::Fill(1)]).areas(area);
    let [_, rect, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(6), Constraint::Fill(1)]).areas(rect);
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(format!(" Message to {} ", c.name)).bold());
    let cursor = if tick % 4 < 2 { "▏" } else { " " };
    let lines = vec![
        Line::from(vec![Span::raw(c.text.clone()), Span::styled(cursor, Style::new().fg(ACCENT))]),
        Line::from(""),
        Line::from(Span::styled("Claude reads it at the session's next tool call or prompt.", Style::new().fg(MUTED))),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), rect);
}

/// The question before stopping or interrupting a session.
fn draw_control(f: &mut Frame, area: Rect, c: &crate::app::Control) {
    let (title, what) = if c.interrupt {
        (" Interrupt now ", format!("Interrupt {} now, as Esc does? Its current step stops.", c.name))
    } else {
        (" Stop ", format!("Stop {} before its next tool call? It is saved; `claude --resume` continues it.", c.name))
    };
    let w = (what.chars().count() as u16 + 4).min(area.width);
    let [_, rect, _] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(w), Constraint::Fill(1)]).areas(area);
    let [_, rect, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(4), Constraint::Fill(1)]).areas(rect);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(Color::Yellow)).title(Line::from(title).bold());
    let lines = vec![Line::from(what), hint(&[("y", " does it, any other key keeps it running")])];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), rect);
}

fn draw_copy_note(f: &mut Frame, area: Rect, note: &crate::app::CopyNote) {
    let mut lines = vec![Line::from(note.headline.yellow())];
    if note.command.chars().count() <= area.width as usize {
        lines.push(Line::from(note.command.clone().bold()));
    } else {
        lines.push(Line::from("claude --append-system-prompt-file".bold()));
        lines.push(Line::from(note.path.clone().bold()));
    }
    lines.push(Line::from(dim(format!("or run `{}` in another terminal", note.cli))));
    f.render_widget(Paragraph::new(lines), area);
}

/// The `c` picker, a small modal: the prompts with tick boxes, then a choice of session or team.
fn draw_rules_picker(f: &mut Frame, area: Rect, picker: &crate::app::RulesPicker) {
    let row = |selected: bool, label: String, title: String, tag: Option<Span<'static>>| {
        let mut spans = vec![Span::raw(if selected { "› " } else { "  " }), Span::styled(label, Style::new().fg(ACCENT)), title.bold()];
        spans.extend(tag);
        let line = Line::from(spans);
        if selected { line.patch_style(Style::new().add_modifier(Modifier::REVERSED)) } else { line }
    };
    let (mut lines, footer, title, cursor_line) = match (picker.mode, &picker.versions) {
        (None, Some(v)) => {
            let mut lines = vec![Line::from(dim(format!("Earlier versions of {}, newest first:", v.title))), Line::from("")];
            for (i, version) in v.list.iter().enumerate() {
                let title = if version.title == v.title { String::new() } else { format!("  {}", version.title) };
                lines.push(row(i == v.cursor, format!("v{:<3} ", version.number), format!("{}{title}", version.when()), None));
                lines.push(Line::from(dim(format!("      {}", version.summary))));
            }
            let mut footer = vec![Line::from("")];
            if let Some(chosen) = v.list.get(v.cursor) {
                footer.extend(chosen.body.lines().take(8).map(|l| Line::from(dim(format!("  │ {}", crate::rules::shorten(l, 72))))));
            }
            footer.push(Line::from(""));
            let noun = v.kind.noun();
            footer.push(hint(&[("enter", if noun == "skill" { " makes it the skill again; the current folder is kept as a version" } else { " makes it the prompt again; the current text is kept as a version" })]));
            (lines, footer, if noun == "skill" { " Skill versions " } else { " Prompt versions " }, 2 + v.cursor * 2)
        }
        (None, None) => {
            let mut lines = vec![
                Line::from(dim("Copy the command that starts Claude Code with:")),
                Line::from(""),
                Line::from(vec![Span::styled("Prompts", Style::new().bold()), dim(" · rules added to the session's system prompt")]),
            ];
            // The skills follow the prompts under a heading of their own.
            let first_skill = picker.library.iter().position(|r| r.kind == crate::rules::Kind::Skill);
            // Summaries wrap instead of being cut, so each one can be read in full.
            let text_width = (area.width as usize).saturating_sub(16).clamp(30, 100);
            let mut cursor_line = 0;
            for (i, rule) in picker.library.iter().enumerate() {
                if Some(i) == first_skill {
                    lines.push(Line::from(""));
                    lines.push(Line::from(vec![Span::styled("Skills", Style::new().bold()), dim(" · step-by-step guides the session must load and follow")]));
                }
                let tick = if picker.ticked[i] { "[x]" } else { "[ ]" };
                let digit = if i < 9 { format!("{}", i + 1) } else { " ".into() };
                // Every prompt says whose it is: built into jazari, or the user's own file.
                let tag = if rule.own { Span::styled("  [yours]", Style::new().fg(Color::Green)) } else { Span::styled("  [built-in]", Style::new().fg(MUTED)) };
                lines.push(row(i == picker.cursor, format!("{digit} {tick} "), rule.title.clone(), Some(tag)));
                // The highlighted skill shows its whole description: when Claude loads it, and when not.
                let (_, description) = crate::skills::front(rule.body());
                let summary = if i == picker.cursor && rule.kind == crate::rules::Kind::Skill && !description.trim().is_empty() {
                    description.split_whitespace().collect::<Vec<_>>().join(" ")
                } else {
                    rule.summary.clone()
                };
                for l in wrap_line(Line::from(dim(summary)), text_width) {
                    let mut spans = vec![Span::raw("        ")];
                    spans.extend(l.spans);
                    lines.push(Line::from(spans));
                }
                if i == picker.cursor {
                    // The scroll keeps two lines from here in view: so here is the entry's end less one.
                    cursor_line = lines.len().saturating_sub(2);
                }
            }
            let footer = match picker.deleting.and_then(|i| picker.library.get(i)) {
                Some(rule) => vec![
                    Line::from(""),
                    Line::from({
                        let warn = Style::new().fg(Color::Yellow).bold();
                        vec![
                            Span::styled(format!("Delete {}? ", rule.title), warn),
                            "y".bold(),
                            Span::styled(" deletes its file, any other key keeps it.", warn),
                        ]
                    }),
                ],
                None => vec![
                    Line::from(""),
                    hint(&[("n", " writes a new one with Claude, in the highlighted group · "), ("e", " edits yours or copies a built-in")]),
                    hint(&[("d", " deletes yours · "), ("v", " lists earlier versions to restore · "), ("u", " brings back the one just deleted")]),
                    Line::from(""),
                    Line::from(dim(format!("Your prompts: {} (one .md file each)", home_relative(&crate::rules::prompts_dir())))),
                    Line::from(dim(format!("Your skills: {} (one folder each)", home_relative(&crate::skills::skills_dir())))),
                    Line::from(dim(format!("A ticked skill is installed to {} when it is missing there", home_relative(&crate::skills::claude_skills_dir())))),
                ],
            };
            (lines, footer, " Launch command ", cursor_line)
        }
        (Some(mode), _) => {
            let mut lines = vec![Line::from(dim(format!("Start {} as:", crate::rules::titles(&picker.chosen())))), Line::from("")];
            for (i, m) in crate::rules::Choice::ALL.into_iter().enumerate() {
                lines.push(row(m == mode, format!("{} ", i + 1), m.title().to_string(), None));
                lines.push(Line::from(dim(format!("    {}", m.summary()))));
            }
            let footer = vec![Line::from(""), Line::from(dim("A command applies to new conversations; your choice is remembered."))];
            (lines, footer, " Launch command · 2 of 2 ", 2)
        }
    };
    let body_len = lines.len();
    // Footer lines wrap to the window, so a narrow terminal or a long path cuts nothing off.
    let room = area.width.saturating_sub(4) as usize;
    lines.extend(footer.into_iter().flat_map(|l| wrap_line(l, room)));
    let w = lines.iter().map(|l| l.width() as u16).max().unwrap_or(0) + 4;
    let (w, h) = (w.min(area.width), (lines.len() as u16 + 2).min(area.height));
    let [_, rect, _] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(w), Constraint::Fill(1)]).areas(area);
    let [_, rect, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(h), Constraint::Fill(1)]).areas(rect);
    f.render_widget(Clear, rect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT)).title(title.bold());
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    // A long library scrolls so the highlighted prompt and its summary stay in view.
    let visible = inner.height as usize;
    let scroll = (cursor_line + 2).saturating_sub(visible).min(body_len.saturating_sub(1));
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
}

/// `path` with the home directory shown as `~`.
fn home_relative(path: &std::path::Path) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    path.strip_prefix(&home).map(|rest| format!("~/{}", rest.display())).unwrap_or_else(|_| path.display().to_string())
}

/// The prompt writer over the picker: the rough idea, a spinner while Claude writes, then the review.
fn draw_prompt_view(f: &mut Frame, area: Rect, view: &crate::prompt_view::PromptView, tick: u64) {
    use crate::prompt_view::Screen as Prompt;
    f.render_widget(Clear, area);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT)).title(view.title().bold());
    let inner = block.inner(area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, area);
    let status: Vec<Line<'static>> = view
        .warning
        .iter()
        .map(|w| Line::from(w.clone().red()))
        .chain(view.notice.iter().map(|n| Line::from(n.clone().green())))
        .collect();
    match view.screen() {
        Prompt::Idea { idea, name, field } => draw_prompt_idea(f, inner, view.kind(), idea, name, field, status),
        Prompt::Generating { model, secs, revising } => {
            let what = format!("{} the {}", if revising { "Revising" } else { "Writing" }, view.kind().noun());
            let lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", SPINNER[(tick / 2 % 4) as usize]), Style::new().fg(ACCENT)),
                    Span::raw(format!("{what} with Claude ({model})… {}:{:02}", secs / 60, secs % 60)),
                ]),
                Line::from(""),
                Line::from(dim("A strong model can take a minute or two. Esc cancels.")),
            ];
            let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3), Constraint::Fill(1)]).areas(inner);
            f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), mid);
        }
        Prompt::Review { draft, key, line } => draw_prompt_review(f, inner, view, draft, key, line, status),
    }
}

/// A text field's lines with the cursor drawn as a reversed cell when it is focused.
fn cursor_lines(text: &crate::prompt_view::TextBox, focused: bool) -> Vec<Line<'static>> {
    let cursor = Style::new().add_modifier(Modifier::REVERSED);
    let (mut lines, mut spans, mut buf) = (Vec::new(), Vec::new(), String::new());
    for (i, &c) in text.chars.iter().enumerate() {
        if focused && i == text.cursor {
            spans.push(Span::raw(std::mem::take(&mut buf)));
            spans.push(Span::styled(if c == '\n' { " ".to_string() } else { c.to_string() }, cursor));
            if c != '\n' {
                continue;
            }
        }
        if c == '\n' {
            spans.push(Span::raw(std::mem::take(&mut buf)));
            lines.push(Line::from(std::mem::take(&mut spans)));
        } else {
            buf.push(c);
        }
    }
    spans.push(Span::raw(buf));
    if focused && text.cursor >= text.chars.len() {
        spans.push(Span::styled(" ", cursor));
    }
    lines.push(Line::from(spans));
    lines
}

fn draw_prompt_idea(
    f: &mut Frame,
    area: Rect,
    kind: crate::rules::Kind,
    idea: &crate::prompt_view::TextBox,
    name: &crate::prompt_view::TextBox,
    field: crate::prompt_view::Field,
    status: Vec<Line<'static>>,
) {
    use crate::prompt_view::Field;
    let width = area.width as usize;
    let intro = wrap_line(
        Line::from(dim(if kind == crate::rules::Kind::Skill {
            "Describe the skill you want, in your own words: what Claude should do, and when. Claude writes the skill's \
             SKILL.md (and any scripts it needs), and you review it before it is saved."
        } else {
            "Describe the standing instruction you want, in your own words. Claude turns it into a prompt for your library, \
             and you review it before it is saved."
        })),
        width,
    );
    let status: Vec<Line> = status.into_iter().flat_map(|l| wrap_line(l, width)).collect();
    let [intro_area, _, idea_label, idea_box, _, name_label, name_box, _, buttons, _, status_area, _] = Layout::vertical([
        Constraint::Length(intro.len() as u16),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Max(14),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(status.len() as u16),
        Constraint::Fill(1),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(intro), intro_area);
    let boxed = |focused: bool| Block::bordered().border_type(BorderType::Rounded).border_style(if focused { Style::new().fg(ACCENT) } else { Style::new().fg(MUTED) });
    let labelled = |text: &'static str, focused: bool| Paragraph::new(Span::styled(text, if focused { Style::new().fg(ACCENT).bold() } else { Style::new().bold() }));
    f.render_widget(labelled("Rough idea", field == Field::Idea), idea_label);
    let block = boxed(field == Field::Idea);
    let inner = block.inner(idea_box);
    f.render_widget(block, idea_box);
    let rows: Vec<Line> = cursor_lines(idea, field == Field::Idea).into_iter().flat_map(|l| wrap_line(l, inner.width as usize)).collect();
    // Typing happens at the end, so a long idea shows its last rows.
    let scroll = rows.len().saturating_sub(inner.height as usize) as u16;
    f.render_widget(Paragraph::new(rows).scroll((scroll, 0)), inner);
    f.render_widget(labelled("Name (optional)", field == Field::Name), name_label);
    let block = boxed(field == Field::Name);
    let inner = block.inner(name_box);
    f.render_widget(block, name_box);
    f.render_widget(Paragraph::new(cursor_lines(name, field == Field::Name)), inner);
    let button = |text: &'static str, focused: bool, primary: bool| {
        let style = match (focused, primary) {
            (true, _) => Style::new().fg(Color::Black).bg(ACCENT).bold(),
            (false, true) => Style::new().fg(ACCENT).bold(),
            (false, false) => Style::new().fg(MUTED),
        };
        Span::styled(format!("[ {text} ]"), style)
    };
    let mut row = vec![
        button(if kind == crate::rules::Kind::Skill { "Write skill" } else { "Write prompt" }, field == Field::Submit, true),
        Span::raw("  "),
        button("Cancel", field == Field::Cancel, false),
        Span::raw("   "),
    ];
    row.extend(hint(&[("tab", " moves here · "), ("enter", " presses")]).spans);
    let row = Line::from(row);
    f.render_widget(Paragraph::new(row), buttons);
    f.render_widget(Paragraph::new(status), status_area);
}

/// Markdown as styled lines: headings, bullets, code fences and inline code and bold.
fn markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut fenced = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            out.push(Line::from(dim(line.to_string())));
            continue;
        }
        if fenced {
            out.push(Line::from(Span::styled(line.to_string(), Style::new().fg(Color::Green))));
            continue;
        }
        if let Some(h) = line.strip_prefix("# ") {
            out.push(Line::from(Span::styled(h.to_string(), Style::new().fg(ACCENT).bold())));
            continue;
        }
        if let Some(h) = line.strip_prefix("## ").or_else(|| line.strip_prefix("### ")) {
            out.push(Line::from(Span::styled(h.to_string(), Style::new().bold())));
            continue;
        }
        let rest = line.trim_start();
        let indent = " ".repeat(line.len() - rest.len());
        let mut spans = vec![Span::raw(indent)];
        let body = match rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
            Some(item) => {
                spans.push(Span::styled("• ", Style::new().fg(ACCENT)));
                item
            }
            None => rest,
        };
        spans.extend(inline_markdown(body));
        out.push(Line::from(spans));
    }
    out
}

/// `code` in the accent colour and **bold** in bold; everything else as is.
fn inline_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            spans.push(Span::styled(part.to_string(), Style::new().fg(ACCENT)));
            continue;
        }
        for (j, piece) in part.split("**").enumerate() {
            if !piece.is_empty() {
                spans.push(if j % 2 == 1 { Span::styled(piece.to_string(), Style::new().bold()) } else { Span::raw(piece.to_string()) });
            }
        }
    }
    spans
}

fn draw_prompt_review(
    f: &mut Frame,
    area: Rect,
    view: &crate::prompt_view::PromptView,
    draft: &crate::prompt_gen::Draft,
    key: &str,
    line: Option<(crate::prompt_view::LineFor, &crate::prompt_view::TextBox)>,
    status: Vec<Line<'static>>,
) {
    use crate::prompt_view::LineFor;
    let wide = area.width >= 120;
    let side_width = if wide { (area.width * 2 / 5).clamp(44, 70) } else { area.width };
    let w = side_width as usize;
    let skill = view.kind() == crate::rules::Kind::Skill;
    let saved_at = if skill { crate::skills::skills_dir().join(key).join(crate::skills::SKILL_MD) } else { crate::rules::prompts_dir().join(format!("{key}.md")) };
    let mut side: Vec<Line<'static>> = vec![
        Line::from(vec![Span::styled("Saves as ", Style::new().bold()), Span::styled(key.to_string(), Style::new().fg(ACCENT).bold())]),
        Line::from(dim(home_relative(&saved_at))),
    ];
    if skill {
        let changed = view.changed_files();
        side.push(Line::from(""));
        side.push(Line::from(Span::styled("Files changed", Style::new().fg(ACCENT).bold())));
        if changed.is_empty() {
            side.push(Line::from(dim("none yet")));
        }
        for (path, what) in changed {
            side.push(Line::from(vec![Span::styled("• ", Style::new().fg(ACCENT)), Span::raw(path), dim(format!("  {what}"))]));
        }
    }
    let list = |side: &mut Vec<Line<'static>>, heading: &str, items: &[String], numbered: bool, empty: &str| {
        side.push(Line::from(""));
        side.push(Line::from(Span::styled(heading.to_string(), Style::new().fg(ACCENT).bold())));
        if items.is_empty() {
            side.push(Line::from(dim(empty.to_string())));
        }
        for (i, item) in items.iter().enumerate() {
            let mark = if numbered { format!("{}. ", i + 1) } else { "• ".into() };
            side.push(Line::from(vec![Span::styled(mark, Style::new().fg(ACCENT)), Span::raw(item.trim().to_string())]));
        }
    };
    list(&mut side, "What I assumed", &draft.assumptions, false, "nothing beyond the idea");
    list(&mut side, "Questions for you", &draft.questions, true, "none: the idea settled every choice");
    if !draft.questions.is_empty() && line.is_none() {
        side.push(Line::from(dim("Press a question's number to answer it; the answer revises the draft.")));
    }
    if line.is_none() {
        side.push(Line::from(""));
        side.push(Line::from(dim("Press r to ask Claude for a change in your own words.")));
    }
    let key_line = line.filter(|(purpose, _)| !purpose.multiline());
    if let Some((_, text)) = key_line {
        side.push(Line::from(""));
        side.push(Line::from(Span::styled("New name (lowercase letters, digits, hyphens):", Style::new().bold())));
        let mut input = vec![Span::styled("› ", Style::new().fg(ACCENT).bold())];
        input.extend(cursor_lines(text, true).into_iter().flat_map(|l| l.spans));
        side.push(Line::from(input));
    }
    side.extend(status.into_iter().flat_map(|l| [Line::from(""), l]));
    // Feedback and answers get a box of their own across the bottom, like a chat input.
    let area = match line.filter(|(purpose, _)| purpose.multiline()) {
        Some((purpose, text)) => {
            let label = match purpose {
                LineFor::Answer(i) => {
                    let question = draft.questions.get(i).map(|q| q.trim().to_string()).unwrap_or_default();
                    Line::from(vec![Span::styled(format!("Your answer to question {}: ", i + 1), Style::new().fg(ACCENT).bold()), Span::raw(question)])
                }
                _ => Line::from(vec![
                    Span::styled(format!("Ask Claude to change this {}", view.kind().noun()), Style::new().fg(ACCENT).bold()),
                    dim(format!("  {} rewrites the draft from what you write here; you review it before it is saved.", view.model())),
                ]),
            };
            let label = wrap_line(label, area.width as usize);
            let box_width = area.width.saturating_sub(2) as usize;
            let rows: Vec<Line> = cursor_lines(text, true).into_iter().flat_map(|l| wrap_line(l, box_width)).collect();
            let height = (rows.len() as u16).clamp(2, 6) + 2;
            let [rest, _, label_area, box_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Length(label.len() as u16), Constraint::Length(height)]).areas(area);
            f.render_widget(Paragraph::new(label), label_area);
            let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT));
            let inner = block.inner(box_area);
            f.render_widget(block, box_area);
            // Typing happens at the end, so a long request shows its last rows.
            let scroll = (rows.len() as u16).saturating_sub(inner.height);
            f.render_widget(Paragraph::new(rows).scroll((scroll, 0)), inner);
            rest
        }
        None => area,
    };
    let side: Vec<Line> = side.into_iter().flat_map(|l| wrap_line(l, w)).collect();
    let [draft_area, side_area] = if wide {
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(side_width)]).spacing(2).areas(area)
    } else {
        let rows = (side.len() as u16).min(area.height / 2);
        Layout::vertical([Constraint::Fill(1), Constraint::Length(rows)]).spacing(1).areas(area)
    };
    let file_title = if skill { format!(" {key}/{} ", crate::skills::SKILL_MD) } else { format!(" {key}.md ") };
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(FRAME).title_style(TITLE).title(file_title);
    let inner = block.inner(draft_area).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, draft_area);
    let rows: Vec<Line> = markdown_lines(&draft.body).into_iter().flat_map(|l| wrap_line(l, inner.width as usize)).collect();
    view.max_scroll.set((rows.len() as u16).saturating_sub(inner.height));
    f.render_widget(Paragraph::new(rows).scroll((view.scroll.min(view.max_scroll.get()), 0)), inner);
    // The name input is last, so a long side panel keeps it in view.
    let scroll = (side.len() as u16).saturating_sub(side_area.height);
    f.render_widget(Paragraph::new(side).scroll((if key_line.is_some() { scroll } else { 0 }, 0)), side_area);
}

fn rgb(c: crate::badge_art::Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// `c` lightened towards white by `t`, so dark glazes stay visible on a dark terminal.
fn lighten(c: crate::badge_art::Rgb, t: f64) -> Color {
    rgb(crate::badge_art::mix(c, (0xff, 0xff, 0xff), t))
}

fn level_color(level: usize) -> Color {
    match level {
        3 => Color::Rgb(0xe6, 0xb8, 0x3c),
        2 => Color::Rgb(0xc0, 0xc8, 0xd0),
        1 => Color::Rgb(0xcd, 0x7f, 0x32),
        _ => MUTED,
    }
}

/// The badge art's height in rows at most; its width follows the cell shape.
const ART_ROWS: u16 = 22;

/// A cell's height over its width when the terminal doesn't report its size in pixels.
pub const DEFAULT_CELL_ASPECT: f64 = 2.0;

/// A cell's height over its width, from the terminal's size in cells and pixels.
pub fn cell_aspect(size: Option<ratatui::backend::WindowSize>) -> f64 {
    match size {
        Some(s) if s.columns_rows.width > 0 && s.columns_rows.height > 0 && s.pixels.width > 0 && s.pixels.height > 0 => {
            let cell_w = s.pixels.width as f64 / s.columns_rows.width as f64;
            let cell_h = s.pixels.height as f64 / s.columns_rows.height as f64;
            (cell_h / cell_w).clamp(1.2, 3.0)
        }
        _ => DEFAULT_CELL_ASPECT,
    }
}

/// The rank, its progress, and the four achievements with their levels as text, beside the badge art
/// when there is room for it; the art is dropped first so the rows never clip. `cell_aspect` is a
/// cell's height over its width, so the art comes out square.
pub(crate) fn draw_badges(f: &mut Frame, area: Rect, badges: Option<&Badges>, cell_aspect: f64) {
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(FRAME).title_style(TITLE).title(" Badges ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(b) = badges else {
        f.render_widget(Paragraph::new(dim(" Reading your badges…")), inner);
        return;
    };
    let rows = 4 + 3 * b.achievements.len() as u16;
    // A cell is `cell_aspect` times as tall as wide, so that many columns per row is square; the art
    // is never taller than ART_ROWS, so a bigger terminal doesn't stretch it.
    let art_rows = inner.height.min(ART_ROWS).min((inner.width.saturating_sub(50) as f64 / cell_aspect) as u16);
    let text = if art_rows >= 12 {
        let art_cols = (art_rows as f64 * cell_aspect).round() as u16;
        let [art, _, text] = Layout::horizontal([Constraint::Length(art_cols), Constraint::Length(2), Constraint::Min(0)]).areas(inner);
        let [_, art, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(art_rows), Constraint::Fill(1)]).areas(art);
        draw_badge_art(f, art, b.rank.tier);
        text
    } else {
        inner
    };
    let [_, text, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(rows.min(text.height)), Constraint::Fill(1)]).areas(text);
    f.render_widget(Paragraph::new(badge_lines(b, text.width as usize)), text);
}

fn badge_lines(b: &Badges, width: usize) -> Vec<Line<'static>> {
    let palette = crate::badge_art::palette(b.rank.tier);
    // Room for the longest label after a bar, " 1000 subagents to gold".
    let bar_width = width.saturating_sub(26).clamp(10, 40);
    let tokens = crate::badges::fmt_value(b.rank.tokens, "tokens");
    let next = match (b.rank.next, b.rank.next_threshold) {
        (Some(name), Some(at)) => format!("{tokens} · {} to {name}", crate::badges::fmt_value(at - b.rank.tokens, "tokens")),
        _ => format!("{tokens} · top tier"),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(b.rank.name.to_string(), Style::new().fg(rgb(palette.metal.light)).bold()),
            dim(format!("   rank {} of {}", b.rank.tier + 1, crate::badges::TIERS.len())),
        ]),
        Line::from(vec![
            Span::styled(bar(b.rank.progress, bar_width), Style::new().fg(lighten(palette.field, 0.35))),
            Span::raw(format!(" {:.0}%", b.rank.progress * 100.0)),
        ]),
        Line::from(dim(next)),
        Line::from(""),
    ];
    for a in &b.achievements {
        lines.extend(achievement_lines(a, bar_width));
    }
    lines
}

fn achievement_lines(a: &BadgeLevel, bar_width: usize) -> Vec<Line<'static>> {
    let level = a.level_name.unwrap_or("not yet");
    let progress = match (a.next_level, a.next_threshold) {
        (Some(next), Some(at)) => format!(" {} to {next}", crate::badges::fmt_value((at - a.value).max(0), a.unit)),
        _ => " complete".to_string(),
    };
    vec![
        Line::from(vec![
            Span::styled(format!("{:<16}", a.name), Style::new().bold()),
            Span::styled(format!("{level:<8}"), Style::new().fg(level_color(a.level)).bold()),
            Span::raw(crate::badges::fmt_value(a.value, a.unit)),
        ]),
        Line::from(vec![Span::styled(bar(a.progress, bar_width), Style::new().fg(level_color(a.level + 1))), dim(progress)]),
        Line::from(""),
    ]
}

/// A tier's badge as coloured line segments, built once per tier: the geometry is too costly to
/// rebuild on every frame.
fn badge_segments(tier: usize) -> &'static [CanvasLine] {
    static SEGMENTS: [std::sync::OnceLock<Vec<CanvasLine>>; crate::badge_art::TIERS] = [const { std::sync::OnceLock::new() }; crate::badge_art::TIERS];
    let tier = tier.min(crate::badge_art::TIERS - 1);
    SEGMENTS[tier].get_or_init(|| {
        let badge = crate::badge_art::badge(tier);
        let (metal, field) = (badge.palette.metal, badge.palette.field);
        badge
            .segments()
            .into_iter()
            .map(|((x1, y1), (x2, y2), role)| {
                let color = match role {
                    crate::badge_art::Role::Rim => rgb(metal.mid),
                    crate::badge_art::Role::Band => rgb(metal.light),
                    crate::badge_art::Role::Tile => lighten(field, 0.3),
                };
                CanvasLine { x1, y1, x2, y2, color }
            })
            .collect()
    })
}

/// The tier's badge drawn with braille lines: rim, band ornament and tile outlines.
fn draw_badge_art(f: &mut Frame, area: Rect, tier: usize) {
    let segments = badge_segments(tier);
    let canvas = Canvas::default().marker(Marker::Braille).x_bounds([-1.05, 1.05]).y_bounds([-1.05, 1.05]).paint(|ctx| {
        for line in segments {
            ctx.draw(line);
        }
    });
    f.render_widget(canvas, area);
}

/// The share view over the view it was opened from: the badges beside the post when the terminal is
/// wide enough, above it otherwise. `fallback` stands in for the badges until the share has its own.
fn draw_share(f: &mut Frame, area: Rect, share: &ShareView, fallback: Option<&Badges>, cell_aspect: f64, tick: u64) {
    f.render_widget(Clear, area);
    let [badges, rect] = if area.width >= 140 {
        let post = (area.width * 2 / 5).max(60);
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(post)]).areas(area)
    } else {
        // The rank and achievements' rows plus the border, leaving the post at least 10 rows.
        let rows = (6 + 3 * 4).min(area.height.saturating_sub(10));
        Layout::vertical([Constraint::Length(rows), Constraint::Fill(1)]).areas(area)
    };
    draw_badges(f, badges, share.badges().or(fallback), cell_aspect);
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT)).title(" Share to LinkedIn ".bold());
    let inner = block.inner(rect).inner(Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);
    match share.screen() {
        Screen::Busy(what) => {
            let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
            let line = Line::from(vec![Span::styled(format!("{} ", SPINNER[(tick / 2 % 4) as usize]), Style::new().fg(ACCENT)), Span::raw(format!("{what}…"))]);
            f.render_widget(Paragraph::new(line).alignment(Alignment::Center), mid);
        }
        Screen::Failed(why) => {
            let lines = vec![Line::from("Couldn't prepare the share.".red().bold()), Line::from(""), Line::from(why.to_string())];
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
        Screen::Preview { post, editing, note, warning, notice } => draw_share_preview(f, inner, share, &post, editing, note, warning, notice),
        Screen::Done(done) => {
            let mut lines = vec![Line::from("Shared.".green().bold()), Line::from("")];
            for (name, mark, text) in done.steps() {
                let (sym, style) = match mark {
                    Mark::Done => ("✓", Style::new().fg(Color::Green)),
                    Mark::Skipped => ("–", Style::new().fg(MUTED)),
                    Mark::Failed => ("✗", Style::new().fg(Color::Red)),
                };
                lines.push(Line::from(vec![Span::styled(format!("{sym} "), style.bold()), Span::styled(format!("{name:<10}"), Style::new().bold()), Span::raw(text)]));
            }
            if let Some(url) = done.manual_url() {
                lines.extend([Line::from(""), Line::from(dim("Open this link to post:")), Line::from(url.to_string())]);
            }
            if done.copied == crate::clipboard::Copied::Failed {
                lines.extend([Line::from(""), Line::from(dim("The post:"))]);
                lines.extend(done.post.lines().map(|l| Line::from(l.to_string())));
            }
            let (notice, warning) = share.status();
            lines.extend(warning.map(|w| Line::from(w.to_string().red())).into_iter().chain(notice.map(|n| Line::from(n.to_string().green()))));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_share_preview(f: &mut Frame, area: Rect, share: &ShareView, post: &str, editing: Option<(&[char], usize)>, note: Option<String>, warning: Option<&str>, notice: Option<&str>) {
    let width = area.width as usize;
    let edit_rows = editing.map_or(0, |(t, _)| (t.len() + 7) / width.max(1) + 3) as u16;
    let status: Vec<Line> = warning
        .map(|w| Line::from(w.to_string().red()))
        .into_iter()
        .chain(note.map(|n| Line::from(n.yellow())))
        .chain(notice.map(|n| Line::from(n.to_string().green())))
        .flat_map(|l| wrap_line(l, width))
        .collect();
    let [edit, body, status_area, count] = Layout::vertical([
        Constraint::Length(edit_rows),
        Constraint::Min(3),
        Constraint::Length(status.len() as u16),
        Constraint::Length(1),
    ])
    .areas(area);
    if let Some((chars, cursor)) = editing {
        let (before, at, after): (String, String, String) = (
            chars[..cursor].iter().collect(),
            chars.get(cursor).map_or(" ".to_string(), |c| c.to_string()),
            chars.get(cursor + 1..).map_or(String::new(), |r| r.iter().collect()),
        );
        let lines = vec![
            hint(&[("", "Edit the line · "), ("enter", " keeps it, "), ("esc", " discards it")]),
            Line::from(vec![Span::styled("› ", Style::new().fg(ACCENT).bold()), Span::raw(before), Span::styled(at, Style::new().add_modifier(Modifier::REVERSED)), Span::raw(after)]),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), edit);
    }
    let rows: Vec<Line> = post.lines().flat_map(|l| wrap_line(Line::from(l.to_string()), width)).collect();
    share.max_scroll.set((rows.len() as u16).saturating_sub(body.height));
    f.render_widget(Paragraph::new(rows).scroll((share.scroll.min(share.max_scroll.get()), 0)), body);
    f.render_widget(Paragraph::new(status), status_area);
    let n = post.chars().count();
    let style = if n > crate::share_view::MAX_POST { Style::new().fg(Color::Red).bold() } else { Style::new().fg(MUTED) };
    f.render_widget(Paragraph::new(Span::styled(format!("{n} / {} characters", crate::share_view::MAX_POST), style)).alignment(Alignment::Right), count);
}

fn draw_board(f: &mut Frame, area: Rect, app: &App) {
    let state = &app.board;
    let path = state.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
    let repo = state
        .path
        .as_ref()
        .and_then(|p| p.parent())
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(format!(" Board · {repo} ")).fg(ACCENT));
    let inner = block.inner(area).inner(Margin::new(1, 0));
    f.render_widget(block, area);

    let Some(board) = &state.board else {
        let msg = match &state.read_error {
            Some(e) => vec![Line::from(format!("Couldn't read the board: {e}").red())],
            None => {
                vec![
                    Line::from(format!("No board for {repo} yet.").bold()),
                    Line::from(""),
                    Line::from(dim("Sessions started with the coordination rules create it when they begin work.")),
                    Line::from(vec![dim("Press "), "c".bold(), dim(" and tick Coordination to copy the command that starts one, or run:")]),
                    Line::from("jazari build coordination".fg(ACCENT)),
                ]
            }
        };
        let height = msg.len() as u16;
        let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(height), Constraint::Fill(1)]).areas(inner);
        f.render_widget(Paragraph::new(msg).alignment(Alignment::Center).wrap(Wrap { trim: false }), mid);
        state.max_scroll.set(0);
        return;
    };

    let now = chrono::Local::now().naive_local();
    let stale = crate::board::staleness(board, now);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &state.read_error {
        lines.push(Line::from(format!("Couldn't re-read the board ({e}); showing the last version.").red()));
    }
    if state.truncated {
        lines.push(Line::from(dim(format!("The board is over {} KB; the rest is not shown.", crate::board::READ_CAP / 1024))));
    }
    lines.push(Line::from(dim(path)));
    lines.push(Line::from(dim("Only sessions started with the coordination rules register here; `c` copies that command.")));
    let tag = |stale: bool, orphan: bool| match (orphan, stale) {
        (true, _) => dim("  [orphan]"),
        (false, true) => dim("  [stale]"),
        _ => Span::raw(""),
    };

    relation_map(&mut lines, board, &stale.agents);
    heading(&mut lines, "Agents", board.agents.len());
    for (a, &is_stale) in board.agents.iter().zip(&stale.agents) {
        let name = if is_stale { dim(a.name.clone()) } else { a.name.clone().bold() };
        lines.push(Line::from(vec![
            Span::raw("  "),
            name,
            Span::raw(format!("  {}", a.task)),
            dim(format!("  last seen {}", a.last_seen)),
            tag(is_stale, false),
        ]));
    }
    heading(&mut lines, "Claims", board.claims.len());
    // An orphan claim is always stale too.
    for ((c, &is_stale), &orphan) in board.claims.iter().zip(&stale.claims).zip(&stale.orphans) {
        let path = if is_stale { dim(c.path.clone()) } else { c.path.clone().bold() };
        lines.push(Line::from(vec![
            Span::raw("  "),
            path,
            Span::raw(format!("  {}", c.agent)),
            dim(format!("  since {}", c.since)),
            tag(is_stale, orphan),
        ]));
    }
    heading(&mut lines, "Decisions", board.decisions.len());
    lines.extend(board.decisions.iter().map(|d| Line::from(format!("  {d}"))));
    heading(&mut lines, "Messages", board.messages.len());
    lines.extend(board.messages.iter().map(|m| Line::from(format!("  {m}"))));
    heading(&mut lines, "Conflicts", board.conflicts.len());
    for c in &board.conflicts {
        // The status already shows as the tag or the styling, so drop its trailing `| status` cell.
        let text = match c.status {
            crate::board::ConflictStatus::Unknown => c.text.clone(),
            _ => c.text.rsplit_once('|').map_or(c.text.as_str(), |(t, _)| t.trim_end()).to_string(),
        };
        lines.push(match c.status {
            crate::board::ConflictStatus::NeedsHuman => Line::from(vec![
                Span::raw("  "),
                Span::styled("NEEDS HUMAN ", Style::new().fg(Color::Yellow).bold()),
                Span::styled(text, Style::new().fg(Color::Yellow)),
            ]),
            crate::board::ConflictStatus::Resolved => Line::from(vec![Span::raw("  "), dim(format!("{text}  (resolved)"))]),
            _ => Line::from(format!("  {text}")),
        });
    }
    heading(&mut lines, "Handoff notes", board.handoffs.len());
    lines.extend(board.handoffs.iter().map(|h| Line::from(format!("  {h}"))));
    if !board.other.is_empty() {
        heading(&mut lines, "Other lines", board.other.len());
        lines.extend(board.other.iter().map(|o| Line::from(dim(format!("  {o}")))));
    }

    // Wrapped here rather than by the Paragraph, so the scroll limit counts exactly the rows drawn.
    let rows: Vec<Line> = lines.into_iter().flat_map(|l| wrap_line(l, inner.width as usize)).collect();
    state.max_scroll.set(rows.len().saturating_sub(inner.height as usize));
    let scroll = state.scroll.min(state.max_scroll.get()) as u16;
    f.render_widget(Paragraph::new(rows).scroll((scroll, 0)), inner);
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

/// The agents as nodes, then who messages whom, who is in conflict and whose claims overlap.
fn relation_map(lines: &mut Vec<Line<'static>>, board: &crate::board::Board, stale: &[bool]) {
    use crate::board::{ConflictStatus, Link};
    let links = crate::board::relations(board);
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled("Relations", Style::new().fg(ACCENT).bold()), dim(format!("  {} · {}", plural(board.agents.len(), "agent"), plural(links.len(), "link")))]));
    let mut nodes = vec![Span::raw("  ")];
    for (a, &is_stale) in board.agents.iter().zip(stale) {
        let style = if is_stale { Style::new().fg(MUTED) } else { Style::new().fg(Color::Green).bold() };
        nodes.extend([dim("["), Span::styled(format!(" {} ", a.name), style), dim("]  ")]);
    }
    if board.agents.is_empty() {
        nodes.push(dim("(no agents registered)"));
    }
    lines.push(Line::from(nodes));
    let name = |n: &str| if n == "all" { Span::styled("everyone".to_string(), Style::new().italic()) } else { Span::styled(n.to_string(), Style::new().bold()) };
    for r in &links {
        let (arrow, style) = match &r.link {
            Link::Messages(n) => (format!(" ──✉ {n}──▶ "), Style::new().fg(ACCENT)),
            Link::Conflict(status) => {
                let (what, color) = match status {
                    ConflictStatus::NeedsHuman => ("needs human", Color::Yellow),
                    ConflictStatus::Resolved => ("resolved", MUTED),
                    _ => ("open", Color::Red),
                };
                (format!(" ◀──✗ conflict, {what}──▶ "), Style::new().fg(color))
            }
            Link::Overlap(path) => (format!(" ◀┄┄ both claim {path} ┄┄▶ "), Style::new().fg(Color::Yellow)),
        };
        lines.push(Line::from(vec![Span::raw("  "), name(&r.from), Span::styled(arrow, style), name(&r.to)]));
    }
    let linked = |n: &str| links.iter().any(|r| r.from == n || r.to == n);
    let alone: Vec<&str> = board.agents.iter().map(|a| a.name.as_str()).filter(|n| !linked(n)).collect();
    if !alone.is_empty() && !links.is_empty() {
        lines.push(Line::from(dim(format!("  no links: {}", alone.join(", ")))));
    } else if links.is_empty() && !board.agents.is_empty() {
        lines.push(Line::from(dim("  no messages, conflicts or overlapping claims yet")));
    }
}

/// Splits a line into rows of at most `width` columns, breaking after whitespace where it can and
/// mid-word only when a word is wider than a row. Whitespace that falls at a break is dropped.
fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let base = line.style;
    let mut rows: Vec<Vec<(String, Style, usize)>> = vec![Vec::new()];
    let (mut used, mut brk) = (0, None);
    for g in line.spans.iter().flat_map(|s| s.styled_graphemes(base)) {
        let w = Span::raw(g.symbol).width();
        let space = g.symbol.chars().all(char::is_whitespace);
        if used + w > width && used > 0 {
            if space {
                (used, brk) = (0, None);
                rows.push(Vec::new());
                continue;
            }
            // Carry the word in progress to the next row when the row has a break before it.
            let row = rows.last_mut().unwrap();
            let carry = match brk {
                Some(i) if i < row.len() => row.split_off(i),
                _ => Vec::new(),
            };
            used = carry.iter().map(|c| c.2).sum();
            brk = None;
            rows.push(carry);
            if used + w > width && used > 0 {
                used = 0;
                rows.push(Vec::new());
            }
        }
        used += w;
        let row = rows.last_mut().unwrap();
        row.push((g.symbol.to_string(), g.style, w));
        if space {
            brk = Some(row.len());
        }
    }
    rows.into_iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (sym, style, _) in row {
                match spans.last_mut() {
                    Some(last) if last.style == style => last.content.to_mut().push_str(&sym),
                    _ => spans.push(Span::styled(sym, style)),
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// A board section's heading with its count, and `(none)` under it when empty.
fn heading(lines: &mut Vec<Line<'static>>, name: &str, count: usize) {
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(name.to_string(), Style::new().fg(ACCENT).bold()), dim(format!("  {count}"))]));
    if count == 0 {
        lines.push(Line::from(dim("  (none)")));
    }
}

fn draw_table(f: &mut Frame, area: Rect, app: &App, visible: &[&Session]) {
    let now = crate::now();
    let rows = visible.iter().map(|s| {
        let (style, label) = status_style(s.status);
        let running = s.subagents.values().filter(|a| a.running).count();
        let agents = if running > 0 { format!("+{running}") } else { String::new() };
        let context = match s.context(app.compact_window) {
            Some(ctx) => {
                let style = Style::new().fg(ratio_color(ctx.used_ratio()));
                Line::from(vec![
                    Span::styled(bar(ctx.used_ratio(), 5), style),
                    Span::styled(format!(" {:>6}", fmt_tokens(ctx.left())), style),
                ])
            }
            None => Line::from(""),
        };
        // Status sits at the right edge so it stays visible when another window covers the left side.
        let row = Row::new(vec![
            Cell::from(Span::styled(format!(" {}", s.name()), Style::new().bold())),
            Cell::from(Span::styled(agents, Style::new().fg(ACCENT))),
            Cell::from(Span::styled(s.project(), Style::new().fg(ACCENT))),
            Cell::from(context),
            right_cell(fmt_duration(s.running_secs(now))),
            right_cell(fmt_tokens(s.total_tokens().total())),
            Cell::from(Line::from(vec![
                Span::styled(label, style),
                Span::styled(format!(" {} ", status_glyph(s.status, app.tick)), style),
            ])
            .alignment(Alignment::Right)),
        ]);
        if s.status == Status::Ended { row.style(Style::new().fg(MUTED)) } else { row }
    });
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(3),
        Constraint::Length(18),
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Length(8),
        Constraint::Length(13),
    ];
    let sorted_col = match app.sort {
        SortKey::Project => 2,
        SortKey::Context => 3,
        SortKey::Age => 4,
        SortKey::Tokens => 5,
        SortKey::Status => 6,
    };
    let arrow = if app.sort_reversed { "▴" } else { "▾" };
    let header = Row::new([" SESSION", "", "PROJECT", "CONTEXT LEFT", "TIME", "TOKENS", "STATUS"].into_iter().enumerate().map(
        |(i, h)| {
            let span = if i == sorted_col { Span::styled(format!("{h}{arrow}"), Style::new().fg(ACCENT)) } else { Span::raw(h) };
            // Status hugs the right edge, so its header does too.
            if i == 6 { Cell::from(Line::from(vec![span, Span::raw(" ")]).alignment(Alignment::Right)) } else { Cell::from(span) }
        },
    ))
    .style(Style::new().fg(MUTED));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(" Sessions ").fg(ACCENT));
    let table = styled_table(rows, &widths, block).header(header);
    let mut state = TableState::default().with_selected(app.selected_index());
    f.render_stateful_widget(table, area, &mut state);
}

fn usage_spans(u: &TokenUsage) -> Vec<Span<'static>> {
    let kv = |k: &'static str, v: u64| [Span::styled(format!("{k} "), Style::new().fg(MUTED)), Span::raw(format!("{}   ", fmt_tokens(v)))];
    let mut spans = Vec::new();
    spans.extend(kv("in", u.input));
    spans.extend(kv("out", u.output));
    spans.extend(kv("cache read", u.cache_read));
    spans.extend(kv("cache write", u.cache_write));
    spans.push(Span::styled("total ", Style::new().fg(MUTED)));
    spans.push(Span::styled(fmt_tokens(u.total()), Style::new().bold()));
    spans
}

fn label(t: &'static str) -> Span<'static> {
    Span::styled(format!("{t:<9}"), Style::new().fg(ACCENT))
}

/// A line of key hints styled like the footer: each key bold, what it does muted.
fn hint(parts: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, text) in parts {
        if !key.is_empty() {
            spans.push(key.to_string().bold());
        }
        spans.push(dim(text.to_string()));
    }
    Line::from(spans)
}

fn dim(t: impl Into<String>) -> Span<'static> {
    Span::styled(t.into(), Style::new().fg(MUTED))
}

fn session_heading(s: &Session) -> Line<'static> {
    Line::from(vec![Span::styled(format!(" {} ", s.name()), Style::new().bold()), dim(format!("· {} ", s.project()))])
}

fn context_line(s: &Session, app: &App) -> Line<'static> {
    let mut spans = vec![label("context")];
    match s.context(app.compact_window) {
        Some(ctx) => spans.extend(context_spans(&ctx, 20, true)),
        None => spans.push(dim("no API calls yet")),
    }
    Line::from(spans)
}

fn draw_details(f: &mut Frame, area: Rect, s: &Session) {
    let now = now_line(s, area.width.saturating_sub(2) as usize);
    let id = vec![label("id"), Span::styled(s.id.clone(), Style::new().fg(Color::Yellow))];
    let model = s.snapshot.as_ref().and_then(|x| x.model.clone()).or(s.model.clone());
    let model = vec![label("model"), model.map_or_else(|| dim("not known yet"), Span::raw)];
    let mut lines = vec![now, Line::from(id), Line::from(model)];
    lines.extend(flag_line(s));
    lines.extend(channel_line(s));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(session_heading(s));
    // Each line keeps to one row; a long one is cut rather than pushing the others out.
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// What the session is doing now, cut to `width`.
fn now_line(s: &Session, width: usize) -> Line<'static> {
    let now = if !s.detail.is_empty() {
        Span::raw(s.detail.clone())
    } else {
        dim(match s.status {
            Status::Idle => "waiting for your next prompt",
            Status::Ended => "ended",
            _ => "working",
        })
    };
    Line::from(vec![label("now"), Span::styled(crate::hook::truncate(&now.content, width.saturating_sub(10)), now.style)])
}

/// Model, permission mode and folder, then the agents by type and the skills the session ran.
fn session_lines(s: &Session, tick: u64) -> Vec<Line<'static>> {
    let sep = || dim("   ·   ");
    let mut about = vec![label("session")];
    let model = s.snapshot.as_ref().and_then(|x| x.model.clone()).or(s.model.clone());
    if let Some(m) = model {
        about.extend([dim("model "), Span::raw(m), sep()]);
    }
    if let Some(m) = s.permission_mode.as_ref() {
        about.extend([dim("mode "), Span::raw(m.clone()), sep()]);
    }
    about.push(Span::raw(short_path(&s.cwd)));
    let mut lines = vec![Line::from(about)];
    let mut agents = vec![label("agents"), Span::raw("main "), Span::styled(fmt_tokens(s.stats.tokens.total()), Style::new().bold())];
    let mut by_type: std::collections::BTreeMap<&str, (usize, usize, u64)> = Default::default();
    for a in s.subagents.values() {
        let e = by_type.entry(a.agent_type.as_str()).or_default();
        e.0 += 1;
        e.1 += usize::from(a.running);
        e.2 += a.stats.tokens.total();
    }
    for (agent_type, (count, running, tokens)) in by_type {
        let (mark, mark_style) = if running > 0 {
            (format!("{} {running} running", SPINNER[(tick / 2 % 4) as usize]), Style::new().fg(Color::Green))
        } else {
            ("✓".to_string(), Style::new().fg(MUTED))
        };
        let count = if count > 1 { format!(" ×{count}") } else { String::new() };
        agents.extend([dim(" · "), Span::raw(format!("{agent_type}{count} ")), Span::styled(format!("{mark} "), mark_style), Span::raw(fmt_tokens(tokens))]);
    }
    lines.push(Line::from(agents));
    let mut skills_list: Vec<_> = s.all_skills().into_iter().collect();
    skills_list.sort_by(|a, b| b.1.tokens.total().cmp(&a.1.tokens.total()).then(b.1.calls.cmp(&a.1.calls)));
    if !skills_list.is_empty() {
        let mut skills = vec![label("skills")];
        for (i, (name, st)) in skills_list.iter().enumerate() {
            if i > 0 {
                skills.push(dim(", "));
            }
            skills.extend([Span::styled(short_skill(name).to_string(), Style::new().fg(Color::Magenta)), dim(format!(" ×{}", st.calls))]);
        }
        lines.push(Line::from(skills));
    }
    lines
}

/// What the session last posted with `jazari watch say`, and what jazari has waiting for it.
fn channel_line(s: &Session) -> Option<Line<'static>> {
    let mut spans = vec![label("jazari")];
    if let Some(said) = &s.said {
        spans.push(Span::raw(format!("says: {said}")));
    }
    let mut waiting = Vec::new();
    if s.waiting > 0 {
        waiting.push(format!("{} message{} waiting", s.waiting, if s.waiting == 1 { "" } else { "s" }));
    }
    if s.stop_requested {
        waiting.push("stop requested".to_string());
    }
    if !waiting.is_empty() {
        if spans.len() > 1 {
            spans.push(dim("   ·   "));
        }
        spans.push(Span::styled(waiting.join(" · "), Style::new().fg(Color::Yellow)));
    }
    (spans.len() > 1).then(|| Line::from(spans))
}

/// The command check's latest finding for the session, in yellow, until the next prompt.
fn flag_line(s: &Session) -> Option<Line<'static>> {
    let flag = s.flag.as_ref()?;
    Some(Line::from(vec![label("check"), Span::styled(flag.clone(), Style::new().fg(Color::Yellow))]))
}

const EVENT_WIDTHS: [Constraint; 6] = [
    Constraint::Length(3),
    Constraint::Length(12),
    Constraint::Length(16),
    Constraint::Length(9),
    Constraint::Fill(1),
    Constraint::Length(8),
];
const EVENT_HEADER: [&str; 6] = ["  #", "TIME", "AGENT", "EVENT", "DETAIL", "TOKENS"];

/// Label and color of an event kind.
fn event_style(kind: &EventKind) -> (&'static str, Style) {
    match kind {
        EventKind::Prompt(_) => ("› prompt", Style::new().fg(Color::Green).bold()),
        EventKind::Response(_) => ("‹ reply", Style::new().fg(Color::Blue)),
        EventKind::Skill(_) => ("skill", Style::new().fg(Color::Magenta).bold()),
        EventKind::Agent { .. } => ("agent", Style::new().fg(ACCENT)),
        EventKind::Question { .. } => ("question", Style::new().fg(Color::Yellow)),
    }
}

/// Text on one line, for a table cell.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn agent_state(s: &Session, tool_use_id: &str, result: &Option<String>, tick: u64) -> Span<'static> {
    // Once its report is in it's done; before that, trust the subagent's own hooks.
    let running = result.is_none() && s.subagent_for(tool_use_id).map_or(s.status != Status::Ended, |a| a.running);
    if running {
        Span::styled(format!("running {}", SPINNER[(tick / 2 % 4) as usize]), Style::new().fg(Color::Green))
    } else {
        dim("done ✓")
    }
}

/// One-line summary of an event, for the DETAIL column.
fn event_summary(e: &TimelineEntry, s: &Session, tick: u64) -> Line<'static> {
    match &e.event.kind {
        EventKind::Prompt(text) | EventKind::Response(text) => Line::from(one_line(text)),
        EventKind::Skill(_) => match e.skill_run() {
            Some(r) => Line::from(vec![
                Span::styled(r.name.clone(), Style::new().fg(Color::Magenta)),
                Span::raw(" "),
                via_span(r.via),
                dim(format!("  {} turns · out {}", r.turns, fmt_tokens(r.tokens.output))),
            ]),
            None => Line::from(""),
        },
        EventKind::Agent { agent_type, description, tool_use_id, result, .. } => Line::from(vec![
            Span::styled(format!("{agent_type} "), Style::new().fg(ACCENT)),
            Span::raw(format!("{description}  ")),
            agent_state(s, tool_use_id, result, tick),
        ]),
        EventKind::Question { question, answer } => Line::from(vec![
            Span::raw(format!("{} ", one_line(question))),
            match answer {
                Some(a) => Span::styled(format!("→ {}", one_line(a)), Style::new().fg(Color::Yellow)),
                None => Span::styled("… waiting", Style::new().fg(Color::Yellow).bold()),
            },
        ]),
    }
}

/// Tokens shown for an event: a prompt's turn, a skill's window, or a subagent's own transcript.
fn event_tokens(e: &TimelineEntry, s: &Session) -> String {
    let n = match &e.event.kind {
        EventKind::Prompt(_) => e.event.tokens.total(),
        EventKind::Skill(_) => return e.skill_run().map(|r| format!("≈{}", fmt_tokens(r.tokens.total()))).unwrap_or_default(),
        EventKind::Agent { tool_use_id, .. } => s.subagent_for(tool_use_id).map_or(0, |a| a.stats.tokens.total()),
        _ => 0,
    };
    if n > 0 { fmt_tokens(n) } else { String::new() }
}

/// Everything known about one event, for the pane under the timeline.
fn event_detail(e: &TimelineEntry, s: &Session, app: &App) -> Vec<Line<'static>> {
    let text = |t: &str| t.lines().map(|l| Line::from(l.to_string())).collect::<Vec<_>>();
    let mut lines = Vec::new();
    match &e.event.kind {
        EventKind::Prompt(prompt) => {
            let t = &e.event.tokens;
            if t.total() > 0 {
                lines.push(Line::from(vec![label("turn"), dim("tokens "), Span::raw(fmt_tokens(t.total())), dim("   out "), Span::raw(fmt_tokens(t.output))]));
                lines.push(Line::from(""));
            }
            lines.extend(text(prompt));
        }
        EventKind::Response(reply) => lines.extend(text(reply)),
        EventKind::Skill(_) => {
            if let Some(r) = e.skill_run() {
                let how = match r.via {
                    Via::Tool => "Claude called the Skill tool",
                    Via::Command => "you typed the command",
                };
                lines.push(Line::from(vec![label("skill"), Span::styled(r.name.clone(), Style::new().fg(Color::Magenta).bold()), dim(format!("   {how}"))]));
                lines.push(Line::from(vec![label("turns"), Span::raw(r.turns.to_string())]));
                let mut tokens = vec![label("tokens")];
                tokens.extend(usage_spans(&r.tokens));
                lines.push(Line::from(tokens));
                lines.push(Line::from(dim("≈ tokens of the turns run inside the skill's window")));
            }
        }
        EventKind::Agent { agent_type, description, tool_use_id, model, result } => {
            lines.push(Line::from(vec![label("agent"), Span::styled(agent_type.clone(), Style::new().fg(ACCENT).bold()), Span::raw("  "), agent_state(s, tool_use_id, result, app.tick)]));
            lines.push(Line::from(vec![label("task"), Span::raw(description.clone())]));
            let agent = s.subagent_for(tool_use_id);
            // The subagent's own transcript names the model it ran on; until it has
            // replied, the launch's override is all there is.
            match (agent.and_then(|a| a.stats.model.clone()), model) {
                (Some(m), _) => lines.push(Line::from(vec![label("model"), Span::raw(m)])),
                (None, Some(m)) => lines.push(Line::from(vec![label("model"), Span::raw(m.clone()), dim("  requested")])),
                (None, None) => {}
            }
            if let Some(a) = agent {
                let mut tokens = vec![label("tokens")];
                tokens.extend(usage_spans(&a.stats.tokens));
                lines.push(Line::from(tokens));
                if !a.stats.skills.is_empty() {
                    lines.push(Line::from(vec![label("skills"), Span::styled(skill_names(&a.stats), Style::new().fg(Color::Magenta))]));
                }
            }
            lines.push(Line::from(""));
            match result {
                Some(r) => {
                    lines.push(Line::from(Span::styled("report", Style::new().fg(ACCENT))));
                    lines.extend(text(r));
                }
                None => lines.push(Line::from(dim("No report yet."))),
            }
        }
        EventKind::Question { question, answer } => {
            lines.push(Line::from(Span::styled("question", Style::new().fg(Color::Yellow))));
            lines.extend(text(question));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("answer", Style::new().fg(Color::Yellow))));
            match answer {
                Some(a) => lines.extend(text(a)),
                None => lines.push(Line::from(dim("Waiting for an answer."))),
            }
        }
    }
    lines
}

/// Full-screen details of a session: what it is doing, its context, tokens, agents and skills,
/// then a scrollable history of prompts, replies, skills, subagents and questions with their
/// answers, with the selected event in full underneath.
fn draw_history(f: &mut Frame, area: Rect, app: &App, s: &Session) {
    let all = s.timeline();
    let events = app.history(s);
    let total = s.total_tokens();
    let about = session_lines(s, app.tick);
    let [summary, list, detail] = Layout::vertical([
        Constraint::Length(6 + about.len() as u16),
        Constraint::Min(6),
        Constraint::Percentage(40),
    ])
    .areas(area);

    let mut tokens = vec![label("tokens")];
    tokens.extend(usage_spans(&total));
    let mut counts = vec![label("history")];
    let mut filter = HistoryFilter::All.next();
    while filter != HistoryFilter::All {
        if counts.len() > 1 {
            counts.push(dim("  ·  "));
        }
        let n = all.iter().filter(|e| filter.matches(&e.event.kind)).count();
        counts.extend([Span::styled(n.to_string(), Style::new().bold()), dim(format!(" {}", filter.label()))]);
        filter = filter.next();
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(session_heading(s))
        .title(Line::from(" Details ").fg(ACCENT).alignment(Alignment::Right));
    let mut lines = vec![now_line(s, summary.width.saturating_sub(2) as usize), context_line(s, app), Line::from(tokens)];
    lines.extend(about);
    lines.push(Line::from(counts));
    f.render_widget(Paragraph::new(lines).block(block), summary);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(" History ").fg(ACCENT))
        .title(Line::from(dim(format!(" showing {} · {} events ", app.history_filter.label(), events.len()))).alignment(Alignment::Right));
    let detail_block = Block::bordered().border_type(BorderType::Rounded).border_style(FRAME).title_style(TITLE);
    if events.is_empty() {
        let msg = if all.is_empty() { "Nothing in this session yet." } else { "No events of this kind. Press f to change the filter." };
        f.render_widget(Paragraph::new(dim(msg)).alignment(Alignment::Center).block(block), list);
        f.render_widget(detail_block, detail);
        return;
    }

    // Newest first, numbered in the order they happened.
    let rows: Vec<Row> = events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let (kind, kind_style) = event_style(&e.event.kind);
            let agent_style = if e.agent == MAIN_AGENT { Style::new() } else { Style::new().fg(ACCENT) };
            Row::new(vec![
                Cell::from(dim(format!("{:>3}", events.len() - i))),
                Cell::from(Span::raw(fmt_local(e.event.at.as_deref()))),
                Cell::from(Span::styled(e.agent.to_string(), agent_style)),
                Cell::from(Span::styled(kind, kind_style)),
                Cell::from(event_summary(e, s, app.tick)),
                right_cell(event_tokens(e, s)),
            ])
        })
        .collect();
    let len = rows.len();
    let table = styled_table(rows, &EVENT_WIDTHS, block).header(Row::new(EVENT_HEADER).style(Style::new().fg(MUTED)));
    let scroll = app.history_scroll.min(len - 1);
    render_scrolled(f, table, list, len, scroll);

    let e = &events[scroll];
    let (kind, kind_style) = event_style(&e.event.kind);
    let detail_block = detail_block.title(Line::from(vec![
        Span::styled(format!(" {kind} "), kind_style),
        dim(format!("· {} · {} ", e.agent, fmt_local(e.event.at.as_deref()))),
    ]));
    let lines = event_detail(e, s, app);
    // Rough bound: wrapped lines can only add to this, so it never hides the end.
    let scroll = app.detail_scroll.min(lines.len().saturating_sub(1) as u16);
    let detail_block = detail_block.title_bottom(Line::from(vec![dim(" J/K "), Span::raw("scroll ")]).alignment(Alignment::Right));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(detail_block), detail);
}

/// Renders a bordered table with a header, selecting row `scroll`, plus a scrollbar
/// when the rows overflow.
fn render_scrolled(f: &mut Frame, table: Table<'static>, area: Rect, len: usize, scroll: usize) {
    let scroll = scroll.min(len.saturating_sub(1));
    let mut state = TableState::default().with_selected(Some(scroll));
    f.render_stateful_widget(table, area, &mut state);

    // Borders and header take three rows.
    if len <= area.height.saturating_sub(3) as usize {
        return;
    }
    let mut sb = ScrollbarState::new(len).position(scroll);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(Style::new().fg(MUTED)),
        area.inner(Margin { vertical: 1, horizontal: 0 }),
        &mut sb,
    );
}

const AGENT_WIDTHS: [Constraint; 10] = [
    Constraint::Length(3),
    Constraint::Length(12),
    Constraint::Length(16),
    Constraint::Fill(3),
    Constraint::Fill(1),
    Constraint::Fill(1),
    Constraint::Length(9),
    Constraint::Length(7),
    Constraint::Length(13),
    Constraint::Length(7),
];
const AGENT_HEADER: [&str; 10] =
    ["  #", "STARTED", "AGENT", "TASK", "LAUNCHED BY", "SKILLS RUN", "STATUS", "USED", "CONTEXT", "LEFT"];

/// Skill name without its plugin prefix, so `compound-engineering:ce-plan` fits as `ce-plan`.
fn short_skill(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// `ce-plan, ce-work ×2` for the skills one agent invoked itself.
fn skill_names(stats: &crate::model::TranscriptStats) -> String {
    let names: Vec<String> = stats
        .skills
        .iter()
        .map(|(name, st)| {
            let name = short_skill(name);
            if st.calls > 1 { format!("{name} ×{}", st.calls) } else { name.to_string() }
        })
        .collect();
    if names.is_empty() { "—".into() } else { names.join(", ") }
}

/// Gauge and tokens-left cells; context is measured against each agent's own auto-compact point.
fn context_cells(ctx: Option<ContextInfo>) -> [Cell<'static>; 2] {
    match ctx {
        Some(ctx) => {
            let style = Style::new().fg(ratio_color(ctx.used_ratio()));
            let approx = if ctx.estimated { "≈" } else { "" };
            [
                Cell::from(Line::from(vec![
                    Span::styled(bar(ctx.used_ratio(), 6), style),
                    Span::styled(format!(" {approx}{:.0}%", ctx.used_ratio() * 100.0), style.bold()),
                ])),
                right_cell(fmt_tokens(ctx.left())),
            ]
        }
        None => [Cell::from(dim("—")), right_cell("—")],
    }
}

/// Full-screen list of a session's subagents: what launched each one, what it ran,
/// how many tokens it used and how much context it has left.
fn draw_agents(f: &mut Frame, area: Rect, app: &App, s: &Session) {
    let agents = s.subagents_by_start();
    let total = s.total_tokens().total();
    let in_agents: u64 = agents.iter().map(|(a, _)| a.stats.tokens.total()).sum();
    let running = agents.iter().filter(|(a, _)| a.running).count();
    let [summary, list] = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(area);

    let share = if total > 0 { in_agents as f64 / total as f64 * 100.0 } else { 0.0 };
    let mut tokens = vec![label("tokens")];
    tokens.extend(usage_spans(&s.total_tokens()));
    let mut count = vec![
        label("agents"),
        Span::styled(format!("{} launched", agents.len()), Style::new().bold()),
    ];
    if running > 0 {
        count.push(Span::styled(format!(" · {running} running"), Style::new().fg(Color::Green)));
    }
    count.extend([dim("   "), Span::raw(fmt_tokens(in_agents)), dim(format!(" tokens ({share:.0}% of session)"))]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(session_heading(s));
    f.render_widget(Paragraph::new(vec![context_line(s, app), Line::from(tokens), Line::from(count)]).block(block), summary);

    // The main agent comes first so each subagent can be compared against it.
    let (main_style, _) = status_style(s.status);
    let mut rows = vec![Row::new(
        [
            Cell::from(dim(format!("{:>3}", 0))),
            Cell::from(Span::raw(fmt_local(s.stats.started.as_deref()))),
            Cell::from(Span::styled(MAIN_AGENT, Style::new().bold())),
            Cell::from(Span::raw(s.title.clone().or_else(|| s.stats.title.clone()).unwrap_or_default())),
            Cell::from(dim("—")),
            Cell::from(Span::styled(skill_names(&s.stats), Style::new().fg(Color::Magenta))),
            Cell::from(Span::styled(format!("{} {}", s.status.as_str(), status_glyph(s.status, app.tick)), main_style)),
            right_cell(fmt_tokens(s.stats.tokens.total())),
        ]
        .into_iter()
        .chain(context_cells(s.context(app.compact_window)))
        .collect::<Vec<_>>(),
    )];
    for (i, (a, launched_by)) in agents.iter().enumerate() {
        let status = if a.running {
            Span::styled(format!("running {}", SPINNER[(app.tick / 2 % 4) as usize]), Style::new().fg(Color::Green))
        } else {
            dim("done ✓")
        };
        let launched_by = match launched_by {
            Some(skill) => Span::styled(short_skill(skill).to_string(), Style::new().fg(Color::Magenta).bold()),
            None => dim("—"),
        };
        let cells = [
            Cell::from(dim(format!("{:>3}", i + 1))),
            Cell::from(Span::raw(fmt_local(a.stats.started.as_deref()))),
            Cell::from(Span::styled(a.agent_type.clone(), Style::new().fg(ACCENT))),
            Cell::from(Span::raw(a.description.clone())),
            Cell::from(launched_by),
            Cell::from(Span::styled(skill_names(&a.stats), Style::new().fg(Color::Magenta))),
            Cell::from(status),
            right_cell(fmt_tokens(a.stats.tokens.total())),
        ];
        rows.push(Row::new(cells.into_iter().chain(context_cells(a.context(app.compact_window))).collect::<Vec<_>>()));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(" Subagents ").fg(ACCENT))
        .title(Line::from(dim(" context left = tokens before each agent's auto-compact ")).alignment(Alignment::Right));
    let len = rows.len();
    let table = styled_table(rows, &AGENT_WIDTHS, block).header(Row::new(AGENT_HEADER).style(Style::new().fg(MUTED)));
    render_scrolled(f, table, list, len, app.history_scroll);
}

fn pane_block(title: &'static str, focused: bool) -> Block<'static> {
    let border = if focused { Style::new().fg(ACCENT) } else { FRAME };
    let title_style = if focused { Style::new().fg(ACCENT).bold() } else { TITLE };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title_style(TITLE)
        .title(Line::from(Span::styled(format!(" {title} "), title_style)))
}

fn fmt_day(day: &str) -> String {
    match chrono::NaiveDate::parse_from_str(day, DAY_FMT) {
        Ok(d) if d == chrono::Local::now().date_naive() => "Today".into(),
        Ok(d) => d.format("%a %b %d").to_string(),
        Err(_) => day.to_string(),
    }
}

/// A day with its year, e.g. "Sep 21, 2026".
pub fn fmt_since(day: &str) -> String {
    chrono::NaiveDate::parse_from_str(day, DAY_FMT).map_or_else(|_| day.to_string(), |d| d.format("%b %-d, %Y").to_string())
}

fn weight(v: i64, max: i64) -> f64 {
    if max <= 0 { 0.0 } else { v as f64 / max as f64 }
}

/// Totals for the period, and a daily chart with one column per day (oldest left).
fn draw_summary(f: &mut Frame, area: Rect, u: &Usage) {
    let sum = &u.summary;
    let mut spans = vec![
        label("tokens"),
        Span::styled(fmt_tokens(sum.total as u64), Style::new().bold()),
        dim("   sessions "),
        Span::raw(sum.sessions.to_string()),
        dim("   per day "),
        Span::raw(fmt_tokens(sum.daily_average() as u64)),
        dim(format!(" over {} days", sum.span_days)),
    ];
    if let Some((day, total)) = &sum.busiest {
        spans.extend([dim("   busiest "), Span::raw(format!("{} ", fmt_day(day))), Span::raw(fmt_tokens(*total as u64))]);
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(Span::styled(format!(" Usage · {} ", u.period_label()), Style::new().fg(ACCENT).bold())));
    let block = match &u.lifetime.since {
        Some(since) => block.title(
            Line::from(vec![
                dim(" all time "),
                Span::styled(crate::model::fmt_count(u.lifetime.total as u64), Style::new().bold()),
                dim(format!(" since {} ", fmt_since(since))),
            ])
            .alignment(Alignment::Right),
        ),
        None => block,
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [text, chart] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    f.render_widget(Paragraph::new(Line::from(spans)), text);

    // Zero-filled daily totals, newest last, trimmed to the columns available; each day is
    // repeated so a short period still spans the width.
    let today = chrono::Local::now().date_naive();
    let n = (sum.span_days.max(0) as usize).min(chart.width as usize);
    let per_day = (chart.width as usize).checked_div(n).unwrap_or(1).max(1);
    let series: Vec<u64> = (0..n)
        .rev()
        .flat_map(|back| {
            let day = (today - chrono::Days::new(back as u64)).format(DAY_FMT).to_string();
            let total = u.days.iter().find(|d| d.day == day).map_or(0, |d| d.total.max(0) as u64);
            std::iter::repeat_n(total, per_day)
        })
        .collect();
    f.render_widget(Sparkline::default().data(&series).style(Style::new().fg(ACCENT)), chart);
}

/// Usage browser: a summary of the period on top, its days on the left, and the selected
/// day's (or whole period's) sessions and skills on the right.
fn draw_usage(f: &mut Frame, area: Rect, u: &Usage) {
    let [top, area] = Layout::vertical([Constraint::Length(6), Constraint::Fill(1)]).areas(area);
    draw_summary(f, top, u);
    let [left, right] = Layout::horizontal([Constraint::Length(34), Constraint::Fill(1)]).areas(area);
    let [sessions_area, skills_area] = Layout::vertical([Constraint::Fill(3), Constraint::Fill(2)]).areas(right);

    // Days
    let max_day = u.days.iter().map(|d| d.total).max().unwrap_or(0);
    let all_total: i64 = u.days.iter().map(|d| d.total).sum();
    let mut rows = vec![Row::new(vec![
        Cell::from(Span::styled("Total", Style::new().bold())),
        Cell::from(""),
        right_cell(fmt_tokens(all_total as u64)),
    ])];
    rows.extend(u.days.iter().map(|d| {
        Row::new(vec![
            Cell::from(fmt_day(&d.day)),
            Cell::from(Span::styled(bar(weight(d.total, max_day), 8), Style::new().fg(ACCENT))),
            right_cell(fmt_tokens(d.total as u64)),
        ])
    }));
    let mut title = pane_block("Days", u.focus == Pane::Days);
    if u.syncing {
        title = title.title_bottom(Line::from(dim(" indexing… ")).alignment(Alignment::Right));
    } else if let Some(e) = &u.error {
        title = title.title_bottom(Line::from(Span::styled(format!(" {} ", crate::hook::truncate(e, 28)), Style::new().fg(Color::Red))));
    }
    let widths = [Constraint::Length(11), Constraint::Length(8), Constraint::Fill(1)];
    let table = styled_table(rows, &widths, title);
    f.render_stateful_widget(table, left, &mut TableState::default().with_selected(Some(u.day_index())));

    let scope = u.day.as_deref().map(fmt_day).unwrap_or_else(|| u.period_label());

    // Sessions
    let max_s = u.sessions.first().map(|s| s.total).unwrap_or(0);
    let rows = u.sessions.iter().map(|s| {
        Row::new(vec![
            Cell::from(Span::styled(s.project.clone(), Style::new().fg(ACCENT))),
            Cell::from(Span::raw(s.title.clone())),
            Cell::from(Span::raw(s.model.trim_start_matches("claude-").to_string())),
            right_cell(s.turns.to_string()),
            right_cell(if s.skill_runs > 0 { s.skill_runs.to_string() } else { String::new() }),
            right_cell(fmt_tokens(s.output as u64)),
            right_cell(fmt_tokens(s.total as u64)),
            Cell::from(Span::styled(bar(weight(s.total, max_s), 8), Style::new().fg(ACCENT))),
        ])
    });
    let widths = [
        Constraint::Length(18),
        Constraint::Fill(1),
        Constraint::Length(12),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(7),
        Constraint::Length(8),
    ];
    let header = Row::new(["PROJECT", "TITLE", "MODEL", "TURNS", "SKILLS", "OUT", "TOKENS", ""]).style(Style::new().fg(MUTED));
    let block = pane_block("Sessions", u.focus == Pane::Sessions)
        .title(Line::from(dim(format!(" {scope} · {} ", u.sessions.len()))).alignment(Alignment::Right));
    let sel = (u.focus == Pane::Sessions && !u.sessions.is_empty()).then_some(u.session_idx);
    let table = styled_table(rows, &widths, block).header(header);
    f.render_stateful_widget(table, sessions_area, &mut TableState::default().with_selected(sel));

    // Skills
    let max_k = u.skills.first().map(|s| s.total).unwrap_or(0);
    let rows = u.skills.iter().map(|s| {
        Row::new(vec![
            Cell::from(Span::styled(s.skill.clone(), Style::new().fg(Color::Magenta).bold())),
            right_cell(s.calls.to_string()),
            right_cell(s.sessions.to_string()),
            right_cell(fmt_tokens(s.output as u64)),
            right_cell(format!("≈{}", fmt_tokens(s.total as u64))),
            Cell::from(Span::styled(bar(weight(s.total, max_k), 8), Style::new().fg(Color::Magenta))),
        ])
    });
    let widths = [
        Constraint::Fill(1),
        Constraint::Length(5),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
    ];
    let header = Row::new(["SKILL", "CALLS", "SESSIONS", "OUT", "TOKENS", ""]).style(Style::new().fg(MUTED));
    let block = pane_block("Skills", u.focus == Pane::Skills)
        .title(Line::from(dim(format!(" {scope} · ≈ tokens inside skill windows "))).alignment(Alignment::Right));
    let sel = (u.focus == Pane::Skills && !u.skills.is_empty()).then_some(u.skill_idx);
    let table = styled_table(rows, &widths, block).header(header);
    f.render_stateful_widget(table, skills_area, &mut TableState::default().with_selected(sel));
}

/// One session from history: per-day totals and every skill run.
fn draw_drill(f: &mut Frame, area: Rect, u: &Usage) {
    let Some(d) = u.drill.as_ref() else { return };
    let s = &d.session;
    let [summary, list] = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(area);

    let mut days = vec![label("days")];
    for (i, (day, total)) in d.days.iter().enumerate() {
        if i > 0 {
            days.push(dim("  ·  "));
        }
        days.extend([dim(format!("{} ", fmt_day(day))), Span::raw(fmt_tokens(*total as u64))]);
    }
    let lines = vec![
        Line::from(vec![
            label("session"),
            dim(s.session_id.clone()),
            dim("   model "),
            Span::raw(s.model.clone()),
        ]),
        Line::from(vec![
            label("tokens"),
            Span::styled(fmt_tokens(s.total as u64), Style::new().bold()),
            dim(format!("   out {}   turns {}   skill runs {}", fmt_tokens(s.output as u64), s.turns, d.runs.len())),
        ]),
        Line::from(days),
    ];
    let mut heading = vec![Span::styled(format!(" {} ", s.project), Style::new().bold())];
    if !s.title.is_empty() {
        heading.push(dim(format!("· {} ", s.title)));
    }
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(FRAME).title_style(TITLE).title(Line::from(heading));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), summary);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME).title_style(TITLE)
        .title(Line::from(" Skill runs ").fg(ACCENT));
    if d.runs.is_empty() {
        f.render_widget(Paragraph::new(dim("No skills invoked in this session.")).alignment(Alignment::Center).block(block), list);
        return;
    }
    let max = d.runs.iter().map(|r| r.total).max().unwrap_or(0) as u64;
    let rows = d
        .runs
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let turns = r.turns as u32;
            run_row(i, Some(&r.at), &r.agent_type, &r.skill, r.via, turns, r.output as u64, r.total as u64, max)
        })
        .collect();
    let table = runs_table(rows, block);
    f.render_stateful_widget(table, list, &mut TableState::default().with_selected(Some(d.scroll)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Line]) -> Vec<String> {
        rows.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_counts_every_row() {
        // Width alone would say two rows; word wrapping needs three.
        let rows = wrap_line(Line::from("aaaaaa bbbbbb cccccc"), 10);
        assert_eq!(text(&rows), ["aaaaaa ", "bbbbbb ", "cccccc"]);
        assert_eq!(text(&wrap_line(Line::from("abcdefghijkl"), 5)), ["abcde", "fghij", "kl"], "a long word breaks mid-word");
        assert_eq!(text(&wrap_line(Line::from("short"), 10)), ["short"]);
        assert_eq!(text(&wrap_line(Line::from(""), 10)), [""], "an empty line still takes a row");
        assert_eq!(text(&wrap_line(Line::from("ab cd"), 0)), ["a", "b", "c", "d"], "a zero width still makes progress");
    }

    #[test]
    fn wrapping_keeps_each_span_style() {
        let rows = wrap_line(Line::from(vec![Span::raw("  "), "name".bold(), Span::raw(" task text")]), 8);
        assert_eq!(text(&rows), ["  name ", "task ", "text"]);
        assert_eq!(rows[0].spans[1], "name".bold());
        assert!(rows.iter().all(|r| r.width() <= 8));
    }

    #[test]
    fn hint_keys_are_bold_and_their_text_muted() {
        let line = hint(&[("", "Edit the line · "), ("enter", " keeps it")]);
        assert_eq!(line.spans, vec![dim("Edit the line · "), "enter".bold(), dim(" keeps it")]);
    }
}

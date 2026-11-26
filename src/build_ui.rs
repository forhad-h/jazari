//! Draws the Build view: Needs you, the products, the selected product's tasks or a task's
//! details, the input box and the status lines. The footer keys come from `BuildView::keys()`.

use crate::build_view::{BuildView, Pane};
use crate::jazari::dashboard::TaskRow;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};

// The same colors as `ui.rs`.
const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::Indexed(247);
const FRAME: Style = Style::new().fg(Color::Indexed(240));
const TITLE: Style = Style::new().fg(Color::Reset);
const HIGHLIGHT: Style = Style::new().bg(Color::Indexed(236)).add_modifier(Modifier::BOLD);
/// The most rows Needs you takes; more scroll.
const NEEDS_ROWS: usize = 4;

fn dim(t: impl Into<String>) -> Span<'static> {
    Span::styled(t.into(), Style::new().fg(MUTED))
}

fn cut(s: &str, n: usize) -> String {
    crate::hook::truncate(s.lines().next().unwrap_or(""), n)
}

fn block(title: &str, focused: bool) -> Block<'static> {
    let style = if focused { Style::new().fg(ACCENT).bold() } else { TITLE };
    Block::bordered().border_type(BorderType::Rounded).border_style(FRAME).title(Span::styled(format!(" {title} "), style))
}

/// The rows of a list that fit in `height`, scrolled so that `selected` shows.
fn window(len: usize, selected: usize, height: usize) -> std::ops::Range<usize> {
    let start = (selected + 1).saturating_sub(height.max(1));
    start..len.min(start + height)
}

pub fn draw(f: &mut Frame, area: Rect, v: &BuildView) {
    let state = if v.snap.supervisor { Span::styled(" supervisor running ", Style::new().fg(Color::Green)) } else { dim(" supervisor stopped ") };
    let outer = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(FRAME)
        .title_style(TITLE)
        .title(Line::from(vec![Span::styled(" Build ", Style::new().fg(ACCENT).bold()), dim("· jazari's sandbox work ")]))
        .title(Line::from(state).alignment(Alignment::Right));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let first = if !v.loaded {
        Some("Reading jazari's store…".to_string())
    } else if let Some(e) = &v.error {
        Some(format!("Couldn't read jazari's store: {e}"))
    } else if v.snap.products.is_empty() {
        Some("No products yet. Press N to start one from an idea, or run `jazari build new`.".to_string())
    } else {
        None
    };

    let needs = v.snap.needs_you();
    let needs_h = if first.is_some() { 0 } else { needs.len().clamp(1, NEEDS_ROWS) as u16 + 1 };
    let input_h = if v.input.is_some() { 3 } else { 0 };
    let [needs_area, panes, input_area, status_area] =
        Layout::vertical([Constraint::Length(needs_h), Constraint::Min(3), Constraint::Length(input_h), Constraint::Length(2)]).areas(inner);

    if let Some(text) = first {
        f.render_widget(Paragraph::new(Line::from(dim(format!(" {text}")))).wrap(Wrap { trim: false }), panes);
    } else {
        draw_needs(f, needs_area, v, &needs);
        let [left, right] = Layout::horizontal([Constraint::Percentage(30), Constraint::Percentage(70)]).areas(panes);
        draw_products(f, left, v);
        match v.task().filter(|_| v.details) {
            Some(t) => draw_details(f, right, t),
            None => draw_tasks(f, right, v),
        }
    }
    if let Some(input) = &v.input {
        draw_input(f, input_area, input);
    }
    draw_status(f, status_area, v);
}

fn draw_needs(f: &mut Frame, area: Rect, v: &BuildView, needs: &[&TaskRow]) {
    let focused = v.focus == Pane::Needs;
    let head = Style::new().fg(if needs.is_empty() { MUTED } else { Color::Yellow }).bold();
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" Needs you ({})", needs.len()), if focused && !needs.is_empty() { head.fg(ACCENT) } else { head }),
    ])];
    if needs.is_empty() {
        lines.push(Line::from(dim("   Nothing waits for you.")));
    }
    let width = area.width as usize;
    for i in window(needs.len(), v.sel_needs, NEEDS_ROWS) {
        let t = needs[i];
        let (mark, what) = match (&t.approval, &t.question) {
            (Some(a), _) => ("approve?", a.as_str()),
            (None, Some(q)) => ("question", q.as_str()),
            _ => ("", ""),
        };
        let lead = format!("   #{:<4} {:<12} {:<9}", t.id, cut(&t.product, 12), mark);
        let line = Line::from(vec![Span::raw(lead.clone()), Span::raw(cut(what, width.saturating_sub(lead.chars().count() + 1)))]);
        lines.push(if focused && i == v.sel_needs { line.style(HIGHLIGHT) } else { line });
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_products(f: &mut Frame, area: Rect, v: &BuildView) {
    let b = block("Products", v.focus == Pane::Products);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let w = inner.width as usize;
    let mut lines = Vec::new();
    for i in window(v.snap.products.len(), v.sel_product, inner.height as usize) {
        let p = &v.snap.products[i];
        let mode = if p.mode == "autonomous" { " auto" } else { "" };
        let counts = [(p.running, "▶"), (p.blocked, "?"), (p.queued, "·")]
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, s)| format!("{s}{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let name = cut(&p.name, w.saturating_sub(counts.chars().count() + mode.len() + 2));
        let pad = w.saturating_sub(name.chars().count() + mode.len() + counts.chars().count() + 1);
        let line = Line::from(vec![Span::raw(format!(" {name}")), Span::styled(mode.to_string(), Style::new().fg(ACCENT)), Span::raw(" ".repeat(pad)), dim(counts)]);
        lines.push(if i == v.sel_product { line.style(HIGHLIGHT) } else { line });
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn status_style(status: &str) -> Style {
    match status {
        "running" => Style::new().fg(Color::Green),
        "blocked" => Style::new().fg(Color::Yellow),
        "failed" => Style::new().fg(Color::Red),
        "queued" => Style::new(),
        _ => Style::new().fg(MUTED),
    }
}

fn draw_tasks(f: &mut Frame, area: Rect, v: &BuildView) {
    let title = v.product().map(|p| format!("Tasks · {}", p.name)).unwrap_or_else(|| "Tasks".into());
    let b = block(&title, v.focus == Pane::Tasks);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let tasks = v.product_tasks();
    if tasks.is_empty() {
        f.render_widget(Paragraph::new(Line::from(dim(" No tasks. Press i for an idea or t for a task."))), inner);
        return;
    }
    let w = inner.width as usize;
    let mut lines = Vec::new();
    for i in window(tasks.len(), v.sel_task, inner.height as usize) {
        let t = tasks[i];
        let lead = format!(" #{:<4} ", t.id);
        let role = format!(" {:<14} ", cut(&t.role, 14));
        let used = lead.chars().count() + 8 + role.chars().count();
        let line = Line::from(vec![
            Span::raw(lead),
            Span::styled(format!("{:<8}", t.status), status_style(&t.status)),
            dim(role),
            Span::raw(cut(&t.title, w.saturating_sub(used + 1))),
        ]);
        lines.push(if v.focus == Pane::Tasks && i == v.sel_task { line.style(HIGHLIGHT) } else { line });
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_details(f: &mut Frame, area: Rect, t: &TaskRow) {
    let b = block(&format!("#{} · {}", t.id, t.product), true).padding(ratatui::widgets::Padding::horizontal(1));
    let inner = b.inner(area);
    f.render_widget(b, area);
    let label = |k: &str| Span::styled(format!("{k:<10}"), Style::new().fg(MUTED));
    let mut lines = vec![
        Line::from(Span::raw(t.title.clone()).bold()),
        Line::from(vec![label("status"), Span::styled(t.status.clone(), status_style(&t.status)), dim(format!(" · {}", t.role))]),
    ];
    if let Some(b) = &t.branch {
        lines.push(Line::from(vec![label("branch"), Span::raw(b.clone())]));
    }
    if let Some(a) = &t.approval {
        lines.push(Line::default());
        lines.push(Line::from(vec![Span::styled("Waits for your approval: ", Style::new().fg(Color::Yellow).bold()), Span::raw(a.clone())]));
        lines.push(Line::from(dim("y approves, d denies. Each needs a second press.")));
    }
    if let Some(q) = &t.question {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Waits for your answer:", Style::new().fg(Color::Yellow).bold())));
        lines.extend(q.lines().map(|l| Line::from(format!("  {l}"))));
        if t.recommended().is_some() {
            lines.push(Line::from(dim(
                "If this question crosses a hard line, another option than the recommended one needs an answer that starts with \"override:\" and gives your reason.",
            )));
        }
    }
    if !t.summary.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(vec![label("summary"), Span::raw(t.summary.clone())]));
    }
    if !t.brief.is_empty() {
        lines.push(Line::default());
        lines.extend(t.brief.lines().map(|l| Line::from(dim(l.to_string()))));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_input(f: &mut Frame, area: Rect, input: &crate::build_view::Input) {
    let b = block(&input.kind.label(), true);
    let inner = b.inner(area);
    f.render_widget(b, area);
    let chars = &input.text.chars;
    let cursor = input.text.cursor;
    // Scroll so that the cursor stays in view.
    let w = inner.width.saturating_sub(2) as usize;
    let start = (cursor + 1).saturating_sub(w);
    let before: String = chars[start..cursor].iter().collect();
    let at: String = chars.get(cursor).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
    let after: String = chars.iter().skip(cursor + 1).take(w.saturating_sub(cursor - start + 1)).collect();
    let line = Line::from(vec![Span::raw(format!(" {before}")), Span::styled(at, Style::new().add_modifier(Modifier::REVERSED)), Span::raw(after)]);
    f.render_widget(Paragraph::new(line), inner);
}

fn draw_status(f: &mut Frame, area: Rect, v: &BuildView) {
    let mut lines = Vec::new();
    lines.push(match v.status() {
        Some((t, true)) => Line::from(Span::styled(format!(" {t}"), Style::new().fg(Color::Red))),
        Some((t, false)) => Line::from(Span::styled(format!(" {t}"), Style::new().fg(ACCENT))),
        None if !v.checks => Line::from(dim(" Read-only here: approvals, answers and new work need Touch ID or the Mac password. d denies.")),
        None => Line::default(),
    });
    if !v.snap.limits.is_empty() {
        lines.push(Line::from(dim(format!(" {}", v.snap.limits))));
    }
    f.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_keeps_the_selection_in_view() {
        assert_eq!(window(10, 0, 4), 0..4);
        assert_eq!(window(10, 5, 4), 2..6);
        assert_eq!(window(3, 2, 4), 0..3);
        assert_eq!(window(0, 0, 4), 0..0);
    }
}

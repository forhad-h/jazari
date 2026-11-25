//! An always-on-top window listing the sessions, so they stay in view over a browser or editor.
//! The dashboard writes a small status file each second; the window shows it and closes when the
//! file goes stale, so a crashed dashboard never leaves it behind.

use crate::model::{ContextInfo, Level, RateWindow, Session, Status};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const SCRIPT: &str = include_str!("../assets/pin.js");

/// At most this many sessions are listed, most urgent first.
const MAX_ROWS: usize = 12;

pub struct Pin {
    child: Child,
    status: PathBuf,
}

impl Pin {
    /// Opens the floating window showing `text`. macOS only: elsewhere it says so.
    pub fn open(text: &str) -> anyhow::Result<Pin> {
        if !cfg!(target_os = "macos") {
            anyhow::bail!("the always-on-top window needs macOS; most terminals have their own 'always on top' setting");
        }
        let dir = crate::data_dir().join("pin");
        std::fs::create_dir_all(&dir)?;
        let (script, status) = (dir.join("pin.js"), dir.join("status.txt"));
        std::fs::write(&script, SCRIPT)?;
        std::fs::write(&status, text)?;
        let child = Command::new("osascript")
            .arg("-l")
            .arg("JavaScript")
            .arg(&script)
            .arg(&status)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("couldn't start osascript: {e}"))?;
        Ok(Pin { child, status })
    }

    /// Updates the window; it also keeps the file fresh so the window stays open.
    pub fn show(&self, text: &str) {
        let _ = std::fs::write(&self.status, text);
    }

    /// False once the user closed the window.
    pub fn is_open(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The window's text: a title line, the dashboard's status row (the selected session's context and
/// the 5-hour limit), then one line per session.
pub fn text(sessions: &[&Session], ctx: Option<ContextInfo>, limits: Option<(Option<RateWindow>, Option<RateWindow>)>) -> String {
    let live: Vec<&&Session> = sessions.iter().filter(|s| s.status != Status::Ended).collect();
    let waiting = live.iter().filter(|s| s.status.needs_user()).count();
    let active = live.iter().filter(|s| s.status == Status::Active).count();
    let mut out = match (waiting, active) {
        (0, 0) => format!("marsad · {} idle", live.len()),
        (0, n) => format!("marsad · {n} working"),
        (n, _) => format!("marsad · {n} need{} you", if n == 1 { "s" } else { "" }),
    };
    out.push('\n');
    let mut gauges = Vec::new();
    if let Some(ctx) = ctx {
        let approx = if ctx.estimated { "≈" } else { "" };
        gauges.push(format!("{} · {} left", gauge(&format!("ctx {approx}{:.0}%", ctx.used_ratio() * 100.0), ctx.used_ratio()), crate::model::fmt_tokens(ctx.left())));
    }
    if let Some(w) = limits.and_then(|(five, _)| five) {
        let reset = w.resets_at.map(|t| format!(" ↻{}", crate::model::fmt_reset(t))).unwrap_or_default();
        gauges.push(format!("{}{reset}", gauge(&format!("5h {:.0}%", w.used_pct), w.used_pct / 100.0)));
    }
    if !gauges.is_empty() {
        out.push_str(&gauges.join(" · "));
        out.push('\n');
    }
    // Same columns as the dashboard: session, project, then the status at the right edge, which
    // the window colors by its label.
    for s in live.iter().take(MAX_ROWS) {
        let (label, glyph) = match s.status {
            Status::Approve => ("APPROVE", "●"),
            Status::Question => ("QUESTION", "?"),
            Status::Error => ("ERROR", "✗"),
            Status::Active => ("ACTIVE", "◐"),
            _ => ("IDLE", "○"),
        };
        out.push_str(&format!("{:<28}  {:<14}  {label:>8} {glyph}\n", clip(&s.name(), 28), clip(&s.project(), 14)));
    }
    if live.len() > MAX_ROWS {
        out.push_str(&format!("+{} more\n", live.len() - MAX_ROWS));
    }
    if live.is_empty() {
        out.push_str("no live sessions\n");
    }
    out
}

/// `[ok]ctx 18%[/]`: the window shows the text bold in the dashboard's color for how full it is,
/// and dims the rest of the line.
fn gauge(text: &str, ratio: f64) -> String {
    let level = match Level::of(ratio) {
        Level::Ok => "ok",
        Level::Warn => "warn",
        Level::Critical => "crit",
    };
    format!("[{level}]{text}[/]")
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_string() } else { format!("{}…", text.chars().take(max - 1).collect::<String>().trim_end()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, title: &str, status: Status) -> Session {
        let mut s = Session::new(id, 0.0);
        s.cwd = "/p/web".into();
        s.title = Some(title.into());
        s.status = status;
        s
    }

    #[test]
    fn the_window_lists_live_sessions_under_a_summary() {
        let (a, b, c) = (
            session("a", "Which database should we use for the new reporting service", Status::Question),
            session("b", "Dark mode", Status::Active),
            session("c", "Old work", Status::Ended),
        );
        let limits = Some((Some(RateWindow { used_pct: 64.2, resets_at: None }), Some(RateWindow { used_pct: 38.0, resets_at: None })));
        let ctx = ContextInfo { used: 18_000, window: 200_000, limit: 100_000, estimated: false };
        let text = text(&[&a, &b, &c], Some(ctx), limits);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "marsad · 1 needs you");
        assert_eq!(lines[1], "[ok]ctx 18%[/] · 82.0k left · [warn]5h 64%[/]", "the 7-day limit is left out");
        assert_eq!(lines[2], "Which database should we us…  web             QUESTION ?");
        assert_eq!(lines[3], "Dark mode                     web               ACTIVE ◐");
        assert_eq!(lines.len(), 4, "ended sessions are left out");
        assert_eq!(super::text(&[], None, None), "marsad · 0 idle\nno live sessions\n");
    }
}

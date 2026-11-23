//! The lines of posts shared or copied in the last days, kept so a new draft doesn't open, phrase or
//! build its sentences the way a recent post did. One JSON object per line in
//! `<data dir>/share/posts.jsonl`; entries older than `KEEP_DAYS` are dropped on every write.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How many days of earlier posts are kept and shown to the drafting.
pub const KEEP_DAYS: i64 = 10;

/// The most earlier lines a draft is shown: enough to steer it, short enough to keep the call small.
pub const MAX_RECENT: usize = 10;

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct Entry {
    /// The local day it was shared, `YYYY-MM-DD`.
    date: String,
    line: String,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("share").join("posts.jsonl")
}

/// Days from `date` to `today`, or None when either doesn't parse.
fn age(date: &str, today: &str) -> Option<i64> {
    let d = NaiveDate::parse_from_str(date, crate::model::DAY_FMT).ok()?;
    let t = NaiveDate::parse_from_str(today, crate::model::DAY_FMT).ok()?;
    Some((t - d).num_days())
}

fn read(path: &Path) -> Vec<Entry> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn fresh(e: &Entry, today: &str) -> bool {
    age(&e.date, today).is_some_and(|n| (0..KEEP_DAYS).contains(&n))
}

/// The lines posted within `KEEP_DAYS` of `today`, newest first, without repeats.
pub fn recent(data_dir: &Path, today: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in read(&path(data_dir)).into_iter().rev().filter(|e| fresh(e, today)) {
        if !out.contains(&e.line) {
            out.push(e.line);
        }
    }
    out.truncate(MAX_RECENT);
    out
}

/// Records `line` as posted on `today` and drops entries past `KEEP_DAYS`. The same line on the same
/// day is kept once, so copying and then sharing it doesn't count twice.
pub fn record(data_dir: &Path, today: &str, line: &str) -> std::io::Result<()> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    let path = path(data_dir);
    let mut entries: Vec<Entry> = read(&path).into_iter().filter(|e| fresh(e, today)).collect();
    if entries.iter().any(|e| e.date == today && e.line == line) {
        return Ok(());
    }
    entries.push(Entry { date: today.to_string(), line: line.to_string() });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text: String = entries.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
    // Written whole to a temp file and renamed, so a crash never leaves half a file.
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marsad-share-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn posts_are_kept_for_ten_days_newest_first() {
        let dir = scratch("keep");
        assert!(recent(&dir, "2026-11-23").is_empty(), "no file yet");
        record(&dir, "2026-11-10", "Old post.").unwrap();
        record(&dir, "2026-11-20", "First.").unwrap();
        record(&dir, "2026-11-22", "Second.").unwrap();
        record(&dir, "2026-11-22", " Second. ").unwrap();
        assert_eq!(recent(&dir, "2026-11-23"), ["Second.", "First."], "the same line on the same day counts once");
        // Ten days on, the first is gone, and the next write prunes it from the file.
        assert_eq!(recent(&dir, "2026-11-30"), ["Second."]);
        record(&dir, "2026-11-30", "Third.").unwrap();
        let text = std::fs::read_to_string(path(&dir)).unwrap();
        assert!(!text.contains("Old post") && !text.contains("First."), "{text}");
        assert_eq!(text.lines().count(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unreadable_lines_and_empty_posts_are_skipped() {
        let dir = scratch("junk");
        std::fs::create_dir_all(path(&dir).parent().unwrap()).unwrap();
        std::fs::write(path(&dir), "not json\n{\"date\":\"2026-11-22\",\"line\":\"Kept.\"}\n").unwrap();
        record(&dir, "2026-11-23", "   ").unwrap();
        assert_eq!(recent(&dir, "2026-11-23"), ["Kept."]);
        std::fs::remove_dir_all(dir).ok();
    }
}

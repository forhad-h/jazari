//! The one-time copy of marsad's data from `~/.claude/marsad` (versions before 0.11) to `~/.marsad`.
//!
//! It runs at the start of every command, the hooks included. The old folder is left as it is:
//! sessions started from it still read its files (a resumed lead reads its teammates' file by the
//! path recorded in its system prompt), and the paths earlier versions printed are kept current by
//! `rules`. Its generated launch files aren't copied, since new commands write new ones.
//!
//! The copy takes a lock in the new folder, so concurrent runs don't race it, and is finished by a
//! marker file; after that it never runs again. Every step is safe to repeat, so a crash halfway is
//! resumed by the next run. Where an item exists in both folders, the two are merged and newer data
//! in the new folder is never overwritten. Event logs are appended to, never replaced, so a hook
//! logging while the copy runs keeps its event.

use anyhow::Context;
use serde_json::Value;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Claude Code's own files, which go to `claude/` in the new folder. Everything else is shared by
/// every coding tool and goes to the top.
const CLAUDE_ITEMS: &[&str] = &["events.jsonl", "events.1.jsonl", "status", "context-warned", "statusline-original.json"];

/// Not copied: the paths earlier versions printed, and the generated launch files.
const SKIP: &[&str] = &["coordination.md", "output-hygiene.md", "coordination+output-hygiene.md", "launch"];

/// The usage database and the files SQLite keeps beside it.
const DB: &str = "usage.db";
const DB_SIDECARS: &[&str] = &["usage.db-wal", "usage.db-shm", "usage.db-journal"];

/// Copied last.
const LOGS: &[&str] = &["events.1.jsonl", "events.jsonl"];

/// The lock file in the new folder, held while copying.
const LOCK: &str = ".copy.lock";

/// Written to the new folder when the copy is complete, holding the old folder's path.
const DONE: &str = ".copied-from";

/// Written beside `DONE` and removed once the user has been told.
const NOTICE: &str = ".copied-notice";

/// How long the hooks wait for another process's copy. They run in the background, so a few
/// seconds is free; the dashboard and the other commands wait longer.
pub const HOOK_WAIT: Duration = Duration::from_secs(10);
pub const WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to copy, or copied before.
    Idle,
    /// This run copied the data.
    Copied,
    /// Another process is still copying after the wait.
    Busy,
}

/// The old and new folders, or None when `MARSAD_HOME` names the folder to use as it is.
fn folders(home_override: Option<PathBuf>) -> Option<(PathBuf, PathBuf)> {
    match home_override {
        Some(_) => None,
        None => Some((crate::legacy_dir(), crate::data_dir())),
    }
}

/// Copies the old folder's data when that hasn't been done. With `report`, a copy that failed
/// prints a warning; it is retried by the next run.
pub fn from_legacy(wait: Duration, report: bool) -> Outcome {
    let Some((old, new)) = folders(crate::home_override()) else {
        return Outcome::Idle;
    };
    migrate(&old, &new, wait).unwrap_or_else(|e| {
        if report {
            eprintln!("warning: copying marsad's data to {}: {e:#}", new.display());
        }
        Outcome::Idle
    })
}

/// The one-line notice that the data was copied, once: it is removed as it is read.
pub fn take_notice() -> Option<String> {
    take_notice_in(&crate::data_dir())
}

fn take_notice_in(new: &Path) -> Option<String> {
    let path = new.join(NOTICE);
    let old = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    Some(format!("marsad now keeps its data in {} (copied from {})", new.display(), old.trim()))
}

/// Whether there is anything to copy: not done before, and the old folder has data.
fn pending(old: &Path, new: &Path) -> bool {
    !new.join(DONE).exists()
        && std::fs::read_dir(old)
            .is_ok_and(|entries| entries.filter_map(Result::ok).any(|e| !SKIP.contains(&e.file_name().to_string_lossy().as_ref())))
}

pub(crate) fn migrate(old: &Path, new: &Path, wait: Duration) -> anyhow::Result<Outcome> {
    if !pending(old, new) {
        return Ok(Outcome::Idle);
    }
    std::fs::create_dir_all(new).with_context(|| format!("creating {}", new.display()))?;
    let lock = File::options().create(true).truncate(false).write(true).open(new.join(LOCK))?;
    let deadline = Instant::now() + wait;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(std::fs::TryLockError::WouldBlock) => return Ok(Outcome::Busy),
            Err(std::fs::TryLockError::Error(e)) => return Err(e).context("locking the data folder"),
        }
    }
    // Another process may have finished while this one waited.
    if !pending(old, new) {
        return Ok(Outcome::Idle);
    }
    copy_all(old, new)?;
    std::fs::write(new.join(NOTICE), old.display().to_string())?;
    std::fs::write(new.join(DONE), old.display().to_string())?;
    Ok(Outcome::Copied)
}

/// Where an item from the old folder belongs in the new one.
fn target(new: &Path, name: &str) -> PathBuf {
    if CLAUDE_ITEMS.contains(&name) { new.join("claude").join(name) } else { new.join(name) }
}

/// Copies everything but what is skipped: the database whole, the logs last. Every item is tried;
/// the first error is returned.
fn copy_all(old: &Path, new: &Path) -> anyhow::Result<()> {
    let mut names: Vec<String> = std::fs::read_dir(old)?.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let later = |n: &str| SKIP.contains(&n) || n == DB || DB_SIDECARS.contains(&n) || LOGS.contains(&n);
    let mut results: Vec<anyhow::Result<()>> =
        names.iter().filter(|n| !later(n)).map(|name| copy_item(&old.join(name), &target(new, name))).collect();
    if old.join(DB).exists() {
        results.push(copy_db(&old.join(DB), &new.join(DB)));
    }
    for name in LOGS {
        if old.join(name).exists() {
            results.push(copy_item(&old.join(name), &target(new, name)));
        }
    }
    results.into_iter().collect()
}

/// Copies `src` to `dst`, merging with what is already at `dst`.
fn copy_item(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let meta = std::fs::metadata(src).with_context(|| format!("reading {}", src.display()))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
        for entry in std::fs::read_dir(src)?.filter_map(Result::ok) {
            copy_item(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if LOGS.contains(&name.as_str()) {
        return append_log(src, dst);
    }
    if !dst.exists() {
        return copy_file(src, dst);
    }
    if name == "settings.json" {
        return merge_settings(src, dst);
    }
    // The copy keeps the modified time, so a copy made by an interrupted run counts as the same.
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if modified(src) > modified(dst) { copy_file(src, dst) } else { Ok(()) }
}

/// Appends the log at `src` to the one at `dst`, unless `dst` already starts with it (a run that
/// stopped after copying it). An append, not a replace, so lines a hook writes meanwhile stay.
fn append_log(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let lines = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    if lines.is_empty() || std::fs::read(dst).is_ok_and(|have| have.starts_with(&lines)) {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // One write on an append handle, as the hook does, so a concurrent event isn't split.
    std::fs::OpenOptions::new().create(true).append(true).open(dst)?.write_all(&lines).with_context(|| format!("writing {}", dst.display()))
}

/// Adds the keys the new settings lack from the old ones.
fn merge_settings(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let read = |p: &Path| -> serde_json::Map<String, Value> {
        std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    };
    let mut merged = read(dst);
    let before = merged.len();
    for (k, v) in read(src) {
        merged.entry(k).or_insert(v);
    }
    if merged.len() > before {
        crate::rules::write_if_changed(dst, &serde_json::to_string_pretty(&Value::Object(merged))?)?;
    }
    Ok(())
}

/// A temporary path beside `dst`, renamed into place once complete, so a crash never leaves a
/// partial file under the real name.
fn beside(dst: &Path) -> PathBuf {
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    dst.with_file_name(format!(".{name}.copying"))
}

/// Copies a file into place, keeping its modified time, which the merge compares.
fn copy_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = beside(dst);
    std::fs::copy(src, &tmp).with_context(|| format!("copying {}", src.display()))?;
    if let Ok(t) = std::fs::metadata(src).and_then(|m| m.modified()) {
        let _ = File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
    }
    std::fs::rename(&tmp, dst).with_context(|| format!("replacing {}", dst.display()))
}

/// Copies the usage database with `VACUUM INTO`, which reads it read-only and consistently,
/// write-ahead log included, while other processes keep using it. When the new folder already has
/// a database, the copy is merged into it the way `marsad import --restore` merges this machine's
/// own export, which never counts a row twice.
fn copy_db(src: &Path, dst: &Path) -> anyhow::Result<()> {
    use rusqlite::{Connection, OpenFlags};
    let tmp = beside(dst);
    let _ = std::fs::remove_file(&tmp);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    {
        let conn = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_context(|| format!("opening {}", src.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute("VACUUM INTO ?1", [tmp.to_string_lossy()]).with_context(|| format!("copying {}", src.display()))?;
    }
    if !dst.exists() {
        return std::fs::rename(&tmp, dst).with_context(|| format!("replacing {}", dst.display()));
    }
    let merged = (|| -> anyhow::Result<()> {
        let from = crate::store::open_at(&tmp)?;
        let export = crate::portable::export_value(&from, serde_json::Map::new())?;
        drop(from);
        let mut into = crate::store::open_at(dst)?;
        for (table, res) in crate::portable::import_value(&mut into, &export, true)? {
            res.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
        }
        Ok(())
    })();
    for side in DB_SIDECARS.iter().map(|s| s.trim_start_matches(DB)) {
        let _ = std::fs::remove_file(tmp.with_file_name(format!("{}{side}", tmp.file_name().unwrap_or_default().to_string_lossy())));
    }
    let _ = std::fs::remove_file(&tmp);
    merged.context("merging the old usage history")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("marsad-migrate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (root.join("dot-claude").join("marsad"), root.join("dot-marsad"))
    }

    fn cleanup(old: &Path) {
        let _ = std::fs::remove_dir_all(old.parent().unwrap().parent().unwrap());
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    /// The old folder's names without the files SQLite keeps beside an open database.
    fn old_names(dir: &Path) -> Vec<String> {
        names(dir).into_iter().filter(|n| !DB_SIDECARS.contains(&n.as_str())).collect()
    }

    /// Every file under `dir` with its contents, to check the old folder is left as it was.
    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let path = entry.path();
            if path.is_dir() {
                out.extend(snapshot(&path));
            } else if !path.to_string_lossy().contains("usage.db") {
                out.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
        out.sort();
        out
    }

    const OLD_NAMES: [&str; 12] = [
        "context-warned", "coordination+output-hygiene.md", "events.1.jsonl", "events.jsonl", "generator", "launch", "prompts",
        "settings.json", "status", "statusline-original.json", "update.json", "usage.db",
    ];

    /// An old folder as a 0.10 install leaves it, with an open usage database in WAL mode. Returns
    /// the connection, which keeps unflushed rows in the WAL, and the all-time total.
    fn populate(old: &Path) -> (rusqlite::Connection, i64) {
        write(&old.join("settings.json"), r#"{"alerts":false,"rules":["output-hygiene"]}"#);
        write(&old.join("update.json"), "{}");
        write(&old.join("events.jsonl"), "{\"n\":1}\n{\"n\":2}\n");
        write(&old.join("events.1.jsonl"), "{\"n\":0}\n");
        write(&old.join("status").join("abc.json"), "{}");
        write(&old.join("context-warned").join("abc"), "");
        write(&old.join("statusline-original.json"), r#"{"command":"mine"}"#);
        write(&old.join("prompts").join("mine.md"), "# Mine\n");
        write(&old.join("launch").join("teammate-0123456789abcdef.md"), "mate");
        write(&old.join("generator").join("0123.md"), "gen");
        write(&old.join("coordination+output-hygiene.md"), "legacy");
        let conn = crate::store::open_at(&old.join(DB)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS probe(v INTEGER); INSERT INTO probe(v) VALUES (42);")
            .unwrap();
        let total = crate::store::lifetime(&conn).unwrap().total;
        (conn, total)
    }

    #[test]
    fn a_fresh_install_has_nothing_to_copy() {
        let (old, new) = temp("fresh");
        assert_eq!(migrate(&old, &new, WAIT).unwrap(), Outcome::Idle);
        assert!(!new.exists(), "nothing is created when there is nothing to copy");
        cleanup(&old);
    }

    #[test]
    fn marsad_home_means_no_copy() {
        assert_eq!(folders(Some(PathBuf::from("/somewhere"))), None);
        assert!(folders(None).is_some());
    }

    #[test]
    fn a_populated_folder_is_copied_into_the_new_layout_and_left_as_it_was() {
        let (old, new) = temp("copy");
        let (_open, _) = populate(&old);
        let before = snapshot(&old);
        assert_eq!(migrate(&old, &new, WAIT).unwrap(), Outcome::Copied);
        assert_eq!(old_names(&old), OLD_NAMES, "nothing in the old folder is removed");
        assert!(old.join("usage.db-wal").exists(), "the open database is left to its owner, not checkpointed");
        assert_eq!(snapshot(&old), before, "or changed");
        assert_eq!(names(&new), [DONE, NOTICE, LOCK, "claude", "generator", "prompts", "settings.json", "update.json", "usage.db"]);
        assert_eq!(names(&new.join("claude")), ["context-warned", "events.1.jsonl", "events.jsonl", "status", "statusline-original.json"]);
        assert_eq!(read(&new.join("claude").join("events.jsonl")), "{\"n\":1}\n{\"n\":2}\n");
        assert_eq!(read(&new.join("prompts").join("mine.md")), "# Mine\n");
        let conn = rusqlite::Connection::open(new.join(DB)).unwrap();
        let v: i64 = conn.query_row("SELECT v FROM probe", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 42, "rows still in the open database's WAL are in the copy");

        write(&old.join("update.json"), "written by an old session later");
        assert_eq!(migrate(&old, &new, WAIT).unwrap(), Outcome::Idle, "a second run does nothing");
        assert_eq!(read(&new.join("update.json")), "{}");
        let notice = take_notice_in(&new).unwrap();
        assert!(notice.contains("dot-claude") && notice.contains("dot-marsad"), "{notice}");
        assert_eq!(take_notice_in(&new), None, "told once");
        cleanup(&old);
    }

    #[test]
    fn a_crash_halfway_resumes_without_doubling_anything() {
        let (old, new) = temp("crash");
        let (_open, total) = populate(&old);
        // As if the last run stopped after copying the database, part of a file and one log.
        copy_db(&old.join(DB), &new.join(DB)).unwrap();
        write(&new.join("generator").join(".0123.md.copying"), "ge");
        append_log(&old.join("events.jsonl"), &new.join("claude").join("events.jsonl")).unwrap();
        assert_eq!(migrate(&old, &new, WAIT).unwrap(), Outcome::Copied);
        assert_eq!(read(&new.join("generator").join("0123.md")), "gen");
        assert_eq!(read(&new.join("claude").join("events.jsonl")), "{\"n\":1}\n{\"n\":2}\n", "the log isn't appended twice");
        assert_eq!(read(&new.join("claude").join("events.1.jsonl")), "{\"n\":0}\n");
        let conn = crate::store::open_at(&new.join(DB)).unwrap();
        assert_eq!(crate::store::lifetime(&conn).unwrap().total, total, "the history isn't counted twice");
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM probe", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        cleanup(&old);
    }

    #[test]
    fn both_folders_merge_without_overwriting_newer_data() {
        let (old, new) = temp("both");
        let (_open, total) = populate(&old);
        write(&new.join("settings.json"), r#"{"alerts":true}"#);
        write(&new.join("claude").join("events.jsonl"), "{\"n\":3}\n");
        write(&new.join("prompts").join("mine.md"), "# Mine, edited later\n");
        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        File::options().write(true).open(new.join("prompts").join("mine.md")).unwrap().set_modified(later).unwrap();
        write(&new.join("update.json"), "stale");
        File::options().write(true).open(new.join("update.json")).unwrap().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        drop(crate::store::open_at(&new.join(DB)).unwrap());

        assert_eq!(migrate(&old, &new, WAIT).unwrap(), Outcome::Copied);
        let settings: Value = serde_json::from_str(&read(&new.join("settings.json"))).unwrap();
        assert_eq!(settings["alerts"], true, "the new folder's value wins");
        assert_eq!(settings["rules"], serde_json::json!(["output-hygiene"]), "a key it lacked comes over");
        assert_eq!(read(&new.join("claude").join("events.jsonl")), "{\"n\":3}\n{\"n\":1}\n{\"n\":2}\n", "logs are joined");
        assert_eq!(read(&new.join("prompts").join("mine.md")), "# Mine, edited later\n", "a newer file is kept");
        assert_eq!(read(&new.join("update.json")), "{}", "an older one is replaced");
        let conn = crate::store::open_at(&new.join(DB)).unwrap();
        assert_eq!(crate::store::lifetime(&conn).unwrap().total, total, "the old history is merged into the new database");
        assert_eq!(old_names(&old), OLD_NAMES);
        cleanup(&old);
    }

    #[test]
    fn a_held_lock_makes_a_caller_wait_then_report_busy() {
        let (old, new) = temp("busy");
        write(&old.join("update.json"), "{}");
        std::fs::create_dir_all(&new).unwrap();
        let held = File::options().create(true).truncate(false).write(true).open(new.join(LOCK)).unwrap();
        held.lock().unwrap();
        assert_eq!(migrate(&old, &new, Duration::from_millis(100)).unwrap(), Outcome::Busy);
        drop(held);
        assert_eq!(migrate(&old, &new, WAIT).unwrap(), Outcome::Copied);
        cleanup(&old);
    }

    /// Hooks keep logging while the copy runs: every event ends up in the new log exactly once.
    #[test]
    fn no_event_is_lost_to_a_concurrent_copy() {
        let (old, new) = temp("race");
        let (_open, _) = populate(&old);
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let (old, new) = (old.clone(), new.clone());
                std::thread::spawn(move || {
                    for i in 0..50 {
                        let line = format!("{}\n", serde_json::json!({ "w": w, "i": i }));
                        // A hook logs whether or not the copy has finished.
                        let _ = migrate(&old, &new, Duration::from_millis(5)).unwrap();
                        crate::hook::append(&new.join("claude"), line.as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let log = read(&new.join("claude").join("events.jsonl"));
        let events: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(events.iter().filter(|e| e.get("n").is_some()).count(), 2, "the old log once");
        let mut seen: Vec<(i64, i64)> = events.iter().filter_map(|e| Some((e.get("w")?.as_i64()?, e["i"].as_i64()?))).collect();
        assert_eq!(seen.len(), 4 * 50);
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4 * 50, "each event exactly once");
        assert_eq!(read(&old.join("events.jsonl")), "{\"n\":1}\n{\"n\":2}\n", "the old log is untouched");
        cleanup(&old);
    }
}

//! An append-only record of what jazari did (`~/.jazari/build/audit.jsonl`). Each entry carries
//! the hash of the one before it, so an edited or removed entry breaks the chain and
//! `jazari build audit` finds it.
//!
//! The chain alone can't show entries cut from the end, or a whole file written again by someone
//! with access to the Mac. So every [`EVERY`] entries the supervisor sends the founder the entry
//! count and the head (the last entry's hash) on Telegram, which is off this machine, and keeps a
//! copy in `audit-head.json`. `jazari build audit` checks the record against that copy and prints
//! the current head, so the founder can compare it with the last Telegram message.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn digest(prev: &str, ts: f64, kind: &str, data: &Value) -> String {
    let body = json!({ "ts": ts, "kind": kind, "data": data, "prev": prev }).to_string();
    Sha256::digest(body.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn last_hash(path: &Path) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().rev().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str::<Value>(l).ok()))
        .and_then(|v| v["hash"].as_str().map(String::from))
        .unwrap_or_else(|| GENESIS.into())
}

/// How often the head is sent to the founder: every 50 entries.
pub const EVERY: usize = 50;

/// Appends an entry. Returns the number of entries now in the record, and the new head.
pub fn append(path: &Path, ts: f64, kind: &str, data: Value) -> anyhow::Result<(usize, String)> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let count = text.lines().filter(|l| !l.trim().is_empty()).count();
    let prev = last_hash(path);
    let hash = digest(&prev, ts, kind, &data);
    let mut line = serde_json::to_vec(&json!({ "ts": ts, "kind": kind, "data": data, "prev": prev, "hash": hash }))?;
    line.push(b'\n');
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(&line)?;
    Ok((count + 1, hash))
}

/// The first 16 characters of a head: enough to compare by eye.
pub fn short(head: &str) -> &str {
    &head[..head.len().min(16)]
}

/// The message that gives the founder the head.
pub fn head_text(count: usize, head: &str) -> String {
    format!("🔏 Audit record: {count} entries, head {}. `jazari build audit` checks the record against this.", short(head))
}

/// Where the last head sent to the founder is kept, next to the record.
pub fn head_path(record: &Path) -> std::path::PathBuf {
    record.with_file_name("audit-head.json")
}

pub fn save_head(record: &Path, count: usize, head: &str) -> anyhow::Result<()> {
    std::fs::write(head_path(record), json!({ "count": count, "head": head }).to_string())?;
    Ok(())
}

/// A whole record: the number of entries, and the head (the last hash).
#[derive(Debug, PartialEq)]
pub struct Whole {
    pub count: usize,
    pub head: String,
}

/// Verifies the chain, then checks it against the last head sent to the founder: the record must
/// still have that entry, with that hash. Entries cut from the end, or a record written again,
/// fail here.
pub fn check(record: &Path) -> Result<Whole, String> {
    let hashes = chain(record)?;
    let whole = Whole { count: hashes.len(), head: hashes.last().cloned().unwrap_or_else(|| GENESIS.into()) };
    let Ok(text) = std::fs::read_to_string(head_path(record)) else { return Ok(whole) };
    let sent: Value = serde_json::from_str(&text).map_err(|_| "audit-head.json can't be read".to_string())?;
    let (Some(count), Some(head)) = (sent["count"].as_u64(), sent["head"].as_str()) else {
        return Err("audit-head.json can't be read".into());
    };
    let count = count as usize;
    if count > hashes.len() {
        return Err(format!("the record has {} entries, but {count} were sent to you on Telegram: entries were removed", hashes.len()));
    }
    if count > 0 && hashes[count - 1] != head {
        return Err(format!("entry {count} doesn't match the head sent to you on Telegram: the record was written again"));
    }
    Ok(whole)
}

/// Each entry's hash when the chain is whole; otherwise the first line that breaks it.
fn chain(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut prev = GENESIS.to_string();
    let mut hashes = Vec::new();
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let bad = || format!("line {} doesn't match the chain", i + 1);
        let v: Value = serde_json::from_str(line).map_err(|_| bad())?;
        let (ts, kind) = (v["ts"].as_f64().ok_or_else(bad)?, v["kind"].as_str().ok_or_else(bad)?);
        if v["prev"] != prev.as_str() || v["hash"] != digest(&prev, ts, kind, &v["data"]).as_str() {
            return Err(bad());
        }
        prev = v["hash"].as_str().unwrap_or_default().to_string();
        hashes.push(prev.clone());
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_catches_an_edited_or_removed_entry() {
        let dir = std::env::temp_dir().join(format!("jazari-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audit.jsonl");
        assert_eq!(check(&path), Ok(Whole { count: 0, head: GENESIS.into() }));
        for i in 0..3 {
            let (n, _) = append(&path, i as f64, "task.start", json!({ "task": i })).unwrap();
            assert_eq!(n, i + 1);
        }
        assert_eq!(check(&path).map(|w| w.count), Ok(3));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"task\":1", "\"task\":9")).unwrap();
        assert_eq!(check(&path), Err("line 2 doesn't match the chain".into()));
        let lines: Vec<_> = text.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(check(&path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn the_head_sent_to_the_founder_catches_cut_and_rewritten_records() {
        let dir = std::env::temp_dir().join(format!("jazari-audit-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audit.jsonl");
        let mut heads = Vec::new();
        for i in 0..4 {
            heads.push(append(&path, i as f64, "task.start", json!({ "task": i })).unwrap().1);
        }
        save_head(&path, 3, &heads[2]).unwrap();
        let whole = check(&path).unwrap();
        assert_eq!((whole.count, whole.head.as_str()), (4, heads[3].as_str()), "later entries are fine");

        // Cut from the end: the chain alone is still whole.
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[1])).unwrap();
        assert_eq!(chain(&path).map(|h| h.len()), Ok(2));
        assert_eq!(check(&path), Err("the record has 2 entries, but 3 were sent to you on Telegram: entries were removed".into()));

        // Written again with other entries: a whole chain, but not the one the founder was sent.
        std::fs::remove_file(&path).unwrap();
        for i in 0..4 {
            append(&path, i as f64, "task.start", json!({ "task": i + 100 })).unwrap();
        }
        assert_eq!(check(&path), Err("entry 3 doesn't match the head sent to you on Telegram: the record was written again".into()));
        assert_eq!(short(&heads[0]).len(), 16);
        assert!(head_text(50, &heads[0]).contains(&format!("50 entries, head {}", short(&heads[0]))));
        std::fs::remove_dir_all(dir).ok();
    }
}

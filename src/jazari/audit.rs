//! An append-only record of what jazari did (`~/.marsad/jazari/audit.jsonl`). Each entry carries
//! the hash of the one before it, so an edited or removed entry breaks the chain and
//! `marsad jazari audit` finds it.

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

pub fn append(path: &Path, ts: f64, kind: &str, data: Value) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let prev = last_hash(path);
    let hash = digest(&prev, ts, kind, &data);
    let mut line = serde_json::to_vec(&json!({ "ts": ts, "kind": kind, "data": data, "prev": prev, "hash": hash }))?;
    line.push(b'\n');
    std::fs::OpenOptions::new().create(true).append(true).open(path)?.write_all(&line)?;
    Ok(())
}

/// The number of entries when the chain is whole; otherwise the first line that breaks it.
pub fn verify(path: &Path) -> Result<usize, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut prev = GENESIS.to_string();
    let mut n = 0;
    for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let bad = || format!("line {} doesn't match the chain", i + 1);
        let v: Value = serde_json::from_str(line).map_err(|_| bad())?;
        let (ts, kind) = (v["ts"].as_f64().ok_or_else(bad)?, v["kind"].as_str().ok_or_else(bad)?);
        if v["prev"] != prev.as_str() || v["hash"] != digest(&prev, ts, kind, &v["data"]).as_str() {
            return Err(bad());
        }
        prev = v["hash"].as_str().unwrap_or_default().to_string();
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_catches_an_edited_or_removed_entry() {
        let dir = std::env::temp_dir().join(format!("marsad-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("audit.jsonl");
        assert_eq!(verify(&path), Ok(0));
        for i in 0..3 {
            append(&path, i as f64, "task.start", json!({ "task": i })).unwrap();
        }
        assert_eq!(verify(&path), Ok(3));
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"task\":1", "\"task\":9")).unwrap();
        assert_eq!(verify(&path), Err("line 2 doesn't match the chain".into()));
        let lines: Vec<_> = text.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(verify(&path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}

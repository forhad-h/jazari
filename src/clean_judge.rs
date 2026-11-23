//! Claude's opinion on cleanup items that hold data (backups, unused Docker volumes, worktrees):
//! keep, suggest or safe, with a one-line reason. One isolated `claude -p` call per batch, set up
//! like the prompt generator (`--safe-mode`, no tools, no transcript, `MARKER_ENV`), given only a
//! redacted description: names, sizes and ages, never file contents. Claude only advises; marsad
//! deletes, and only after the user confirms each item.

use crate::clean::{Action, DirKind, Item, Tier};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// Models tried in order: a careful judgement on a small input, so Sonnet first; Opus when Sonnet
/// isn't offered; then the session default. `models.clean_judge` in settings overrides the first.
const MODELS: &[&str] = &["sonnet", "opus"];
const WAIT: Duration = Duration::from_secs(180);

const SCHEMA: &str = r#"{"type":"object","properties":{"verdicts":{"type":"array","items":{"type":"object","properties":{"ref":{"type":"string"},"call":{"type":"string","enum":["keep","suggest","safe"]},"reason":{"type":"string"}},"required":["ref","call","reason"]}}},"required":["verdicts"]}"#;

const SYSTEM: &str = "You review folders and Docker volumes on a developer's machine that a disk cleanup tool \
found, and say for each whether deleting it would lose anything. You see names, sizes, ages and top-level \
entry names only. Answer `safe` only when the item is clearly a redundant copy or rebuildable (for example \
an older backup of a folder that still exists, when a newer copy is kept). Answer `keep` when it may hold \
the only copy of something: database data, uploads, unique files, or anything you can't tell. Answer \
`suggest` when deleting is probably fine but the user should look first. Be conservative: losing data is \
far worse than keeping a few GB. Give one short reason per item, naming what you based it on.";

#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub call: String,
    pub reason: String,
}

/// Saved verdicts, by item id, with the size they were given for.
fn store_path() -> PathBuf {
    crate::data_dir().join("clean-verdicts.json")
}

/// The saved verdict for an item, unless its size changed by more than a fifth since.
pub fn lookup(saved: &Map<String, Value>, item: &Item) -> Option<Verdict> {
    let v = saved.get(&item.id)?;
    let bytes = v["bytes"].as_u64()?;
    let close = (bytes as f64 - item.bytes as f64).abs() <= item.bytes.max(1) as f64 * 0.2;
    close.then(|| Verdict { call: v["call"].as_str().unwrap_or("").into(), reason: v["reason"].as_str().unwrap_or("").into() })
}

pub fn load() -> Map<String, Value> {
    std::fs::read_to_string(store_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(items: &[&Item], verdicts: &[(usize, Verdict)]) {
    let mut all = load();
    for (i, v) in verdicts {
        if let Some(item) = items.get(*i) {
            all.insert(item.id.clone(), json!({ "call": v.call, "reason": v.reason, "bytes": item.bytes, "at": crate::now() }));
        }
    }
    if let Ok(text) = serde_json::to_string_pretty(&Value::Object(all)) {
        let _ = std::fs::create_dir_all(crate::data_dir());
        let _ = std::fs::write(store_path(), text);
    }
}

/// `path` with the home folder shown as `~`, so the user's name never leaves the machine.
fn redact(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

/// Up to 20 top-level entry names, folders marked with `/`.
fn entries(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let dir = e.file_type().is_ok_and(|t| t.is_dir());
                    format!("{}{}", e.file_name().to_string_lossy(), if dir { "/" } else { "" })
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.truncate(20);
    names
}

/// What Claude is told about each item.
pub fn describe(items: &[&Item], home: &Path) -> Value {
    let list: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let mut d = json!({
                "ref": (i + 1).to_string(),
                "what": item.what,
                "size_gb": (item.bytes as f64 / 1e8).round() / 10.0,
                "note": item.note,
            });
            match &item.action {
                Action::RemoveDir { path, kind: DirKind::Backup { original } } => {
                    d["path"] = json!(redact(path, home));
                    d["copy_of"] = json!(redact(original, home));
                    d["copy_of_still_exists"] = json!(original.is_dir());
                    d["entries"] = json!(entries(path));
                    d["original_entries"] = json!(entries(original));
                }
                Action::RemoveDir { path, .. } | Action::GitWorktreeRemove { path, .. } => d["path"] = json!(redact(path, home)),
                Action::DockerVolumeRm(name) => d["docker_volume"] = json!(name),
                Action::DockerContainerRm(_) => d["docker_container"] = json!(item.what),
                _ => {}
            }
            d
        })
        .collect();
    json!({ "items": list })
}

/// The verdicts in a `claude -p --output-format json` reply, by index into the batch.
fn parse(stdout: &[u8], count: usize) -> Result<Vec<(usize, Verdict)>, Option<String>> {
    let v: Value = serde_json::from_slice(stdout).map_err(|_| Some("claude's output wasn't JSON".to_string()))?;
    let result = v["result"].as_str().unwrap_or("");
    if v["is_error"].as_bool() == Some(true) {
        // None: the model was rejected, so the next one is tried.
        if v["api_error_status"].as_u64() == Some(404) || result.contains("selected model") {
            return Err(None);
        }
        return Err(Some(format!("claude returned an error: {}", result.lines().next().unwrap_or("no details"))));
    }
    let out = match &v["structured_output"] {
        o @ Value::Object(_) => o.clone(),
        _ => {
            let (a, b) = (result.find('{'), result.rfind('}'));
            a.zip(b).filter(|(a, b)| a < b).and_then(|(a, b)| serde_json::from_str(&result[a..=b]).ok()).ok_or_else(|| Some("claude gave no verdicts".to_string()))?
        }
    };
    let verdicts = out["verdicts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| {
            let i = x["ref"].as_str()?.parse::<usize>().ok()?.checked_sub(1).filter(|i| *i < count)?;
            let call = x["call"].as_str().filter(|c| matches!(*c, "keep" | "suggest" | "safe"))?;
            Some((i, Verdict { call: call.into(), reason: crate::hook::truncate(x["reason"].as_str().unwrap_or(""), 160) }))
        })
        .collect();
    Ok(verdicts)
}

fn models() -> Vec<Option<String>> {
    let first = crate::settings::get("models").and_then(|m| m["clean_judge"].as_str().map(str::to_string)).filter(|m| !m.trim().is_empty());
    let mut list: Vec<Option<String>> = first.into_iter().map(Some).collect();
    for m in MODELS {
        if !list.iter().any(|x| x.as_deref() == Some(*m)) {
            list.push(Some((*m).into()));
        }
    }
    list.push(None);
    list
}

/// The items Claude is asked about: data-holding ones marsad could remove.
pub fn candidates(items: &[Item]) -> Vec<&Item> {
    items.iter().filter(|i| i.tier == Tier::C && i.action != Action::None).collect()
}

/// Asks Claude about `items`, saves and returns the verdicts by item id.
pub fn judge(items: &[&Item]) -> Result<Vec<(String, Verdict)>, String> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let program = std::env::var_os("MARSAD_CLAUDE").filter(|p| !p.is_empty()).unwrap_or_else(|| "claude".into());
    let home = dirs::home_dir().unwrap_or_default();
    let input = describe(items, &home).to_string();
    let dir = crate::data_dir().join("generator");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let system = dir.join("clean-judge.md");
    crate::rules::write_if_changed(&system, SYSTEM).map_err(|e| e.to_string())?;
    let cancel = AtomicBool::new(false);
    for model in models() {
        let mut args: Vec<String> = ["-p", "--safe-mode", "--no-session-persistence", "--tools", "", "--max-turns", "3"].map(String::from).to_vec();
        if let Some(m) = &model {
            args.extend(["--model".into(), m.clone()]);
        }
        args.extend(["--output-format", "json", "--json-schema", SCHEMA].map(String::from));
        args.extend(["--append-system-prompt-file".into(), system.to_string_lossy().into_owned()]);
        let mut cmd = Command::new(&program);
        cmd.args(&args).current_dir(&dir).env(crate::prompt_gen::MARKER_ENV, "1");
        let out = crate::share::run_captured(&mut cmd, input.as_bytes(), WAIT, &cancel).map_err(|e| match e {
            crate::share::RunError::NotFound => "Claude Code (`claude`) was not found".to_string(),
            crate::share::RunError::Timeout => "claude took too long".to_string(),
            crate::share::RunError::Spawn(why) => format!("couldn't run claude: {why}"),
            crate::share::RunError::Cancelled => "cancelled".to_string(),
        })?;
        match parse(&out.stdout, items.len()) {
            Ok(verdicts) => {
                save(items, &verdicts);
                return Ok(verdicts.into_iter().map(|(i, v)| (items[i].id.clone(), v)).collect());
            }
            Err(None) => continue,
            Err(Some(why)) => return Err(why),
        }
    }
    Err("claude rejected every model it was offered".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(home: &Path) -> Item {
        Item {
            id: home.join("p/src.bak").display().to_string(),
            tier: Tier::C,
            what: "backup of src".into(),
            bytes: 1_200_000_000,
            action: Action::RemoveDir { path: home.join("p/src.bak"), kind: DirKind::Backup { original: home.join("p/src") } },
            note: "changed 3 days ago".into(),
        }
    }

    #[test]
    fn descriptions_hide_the_home_folder_and_carry_no_contents() {
        let home = Path::new("/Users/somebody");
        let vol = Item { id: "docker-volume:web_pg".into(), tier: Tier::C, what: "Docker volume pg of web".into(), bytes: 2_000_000_000, action: Action::DockerVolumeRm("web_pg".into()), note: String::new() };
        let b = backup(home);
        let d = describe(&[&b, &vol], home).to_string();
        assert!(!d.contains("somebody"), "{d}");
        assert!(d.contains("~/p/src.bak") && d.contains("\"copy_of\":\"~/p/src\"") && d.contains("web_pg"), "{d}");
        assert!(d.contains("\"ref\":\"1\"") && d.contains("\"size_gb\":1.2"), "{d}");
    }

    #[test]
    fn verdicts_parse_and_unknown_refs_or_calls_are_dropped() {
        let out = json!({ "structured_output": { "verdicts": [
            { "ref": "1", "call": "safe", "reason": "an older copy; src still exists" },
            { "ref": "2", "call": "delete it all", "reason": "x" },
            { "ref": "9", "call": "keep", "reason": "x" },
        ] } });
        let got = parse(out.to_string().as_bytes(), 2).unwrap();
        assert_eq!(got, [(0, Verdict { call: "safe".into(), reason: "an older copy; src still exists".into() })]);
        let rejected = json!({ "is_error": true, "api_error_status": 404, "result": "model not found" });
        assert_eq!(parse(rejected.to_string().as_bytes(), 1), Err(None), "a rejected model tries the next");
        assert!(matches!(parse(b"nope", 1), Err(Some(_))));
        let backwards = json!({ "result": "} then {" });
        assert!(matches!(parse(backwards.to_string().as_bytes(), 1), Err(Some(_))), "no panic on a brace before the opening one");
    }

    #[test]
    fn a_saved_verdict_goes_stale_when_the_size_changes() {
        let item = backup(Path::new("/h"));
        let saved: Map<String, Value> = [(item.id.clone(), json!({ "call": "safe", "reason": "r", "bytes": 1_100_000_000u64 }))].into_iter().collect();
        assert_eq!(lookup(&saved, &item).unwrap().call, "safe");
        let grown = Item { bytes: 3_000_000_000, ..item };
        assert!(lookup(&saved, &grown).is_none());
    }

    #[test]
    fn the_judge_prefers_sonnet_then_opus_then_the_default() {
        let m = models();
        assert_eq!(m.last(), Some(&None));
        assert!(m.iter().position(|x| x.as_deref() == Some("sonnet")) < m.iter().position(|x| x.as_deref() == Some("opus")));
    }
}

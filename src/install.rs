//! Adds/removes jazari hook entries in ~/.claude/settings.json without touching anything else.

use anyhow::{Context, bail};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub const EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Elicitation",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
];

/// Personal skill that teaches Claude Code how to query jazari's usage history.
const SKILL_MD: &str = include_str!("../assets/SKILL.md");

fn skill_dir() -> PathBuf {
    crate::claude_dir().join("skills").join("jazari-usage")
}

fn settings_path() -> PathBuf {
    crate::claude_dir().join("settings.json")
}

fn is_ours(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| {
        hs.iter().any(|h| {
            h["command"].as_str().is_some_and(|c| c.contains("jazari") && (c.ends_with(" hook") || c.ends_with(CONTEXT_HOOK) || c.ends_with(GUARD_HOOK)))
        })
    })
}

/// Arguments of the hook that adds the handoff warning to a session's context, on each prompt and
/// after each tool call, so a long turn is warned before it reaches auto-compact.
const CONTEXT_HOOK: &str = " watch context --hook";

/// Arguments of the hook that checks the disk before each tool call and prompt.
const GUARD_HOOK: &str = " guard --hook";

/// Registers jazari for every event. The context and disk guard hooks are synchronous: only a hook
/// Claude Code waits for can add text to the prompt or stop a tool call.
fn add_hooks(settings: &mut Map<String, Value>, exe: &str) -> anyhow::Result<()> {
    let command = format!("\"{exe}\" hook");
    let hooks = settings.entry("hooks").or_insert_with(|| json!({}));
    let Value::Object(hooks) = hooks else { bail!("`hooks` in settings.json is not an object") };
    for event in EVENTS {
        let list = hooks.entry(*event).or_insert_with(|| json!([]));
        let Value::Array(list) = list else { bail!("hooks.{event} is not an array") };
        list.push(json!({
            "hooks": [{ "type": "command", "command": command, "async": true, "timeout": 10 }]
        }));
        if matches!(*event, "UserPromptSubmit" | "PostToolUse") {
            list.push(json!({
                "hooks": [{ "type": "command", "command": format!("\"{exe}\"{CONTEXT_HOOK}"), "timeout": 5 }]
            }));
        }
        if matches!(*event, "UserPromptSubmit" | "PreToolUse") {
            list.push(json!({
                "hooks": [{ "type": "command", "command": format!("\"{exe}\"{GUARD_HOOK}"), "timeout": 5 }]
            }));
        }
    }
    Ok(())
}

fn load() -> anyhow::Result<(PathBuf, Map<String, Value>)> {
    let path = settings_path();
    let settings = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        _ => json!({}),
    };
    let Value::Object(map) = settings else { bail!("{} is not a JSON object", path.display()) };
    Ok((path, map))
}

fn save(path: &PathBuf, settings: Map<String, Value>) -> anyhow::Result<()> {
    if path.exists() {
        let backup = path.with_extension("json.jazari-backup");
        std::fs::copy(path, &backup)?;
        println!("Backed up settings to {}", backup.display());
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    let text = serde_json::to_string_pretty(&Value::Object(settings))? + "\n";
    std::fs::write(path, text)?;
    Ok(())
}

/// Removes our entries from every event; drops events (and `hooks`) left empty.
fn strip(settings: &mut Map<String, Value>) {
    let Some(Value::Object(hooks)) = settings.get_mut("hooks") else { return };
    for entries in hooks.values_mut() {
        if let Value::Array(list) = entries {
            list.retain(|e| !is_ours(e));
        }
    }
    hooks.retain(|_, v| !v.as_array().is_some_and(Vec::is_empty));
    if hooks.is_empty() {
        settings.remove("hooks");
    }
}

fn is_our_statusline(v: &Value) -> bool {
    v["command"].as_str().is_some_and(|c| c.contains("jazari") && c.ends_with(" statusline"))
}

/// Points statusLine at jazari. An existing, foreign status line is saved and
/// wrapped: `jazari statusline` keeps running it and prints its output.
/// `original` is where a replaced statusLine is kept (`statusline::original_path()` outside tests).
fn install_statusline(settings: &mut Map<String, Value>, exe: &str, original: &Path) -> anyhow::Result<()> {
    let command = format!("\"{exe}\" statusline");
    let mut line = match settings.get("statusLine") {
        Some(existing) if !is_our_statusline(existing) && existing["command"].is_string() => {
            if let Some(dir) = original.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(original, serde_json::to_vec_pretty(existing)?)?;
            println!("Wrapped your existing status line; it keeps showing as before.");
            existing.clone()
        }
        Some(existing) if is_our_statusline(existing) => existing.clone(),
        _ => {
            // No status line to wrap: drop any original left from an earlier install, or
            // `jazari statusline` would keep running a command the user already removed.
            let _ = std::fs::remove_file(original);
            json!({})
        }
    };
    line["type"] = json!("command");
    line["command"] = json!(command);
    settings.insert("statusLine".into(), line);
    Ok(())
}

fn uninstall_statusline(settings: &mut Map<String, Value>, original: &Path) {
    if !settings.get("statusLine").is_some_and(is_our_statusline) {
        return;
    }
    match std::fs::read(original).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        Some(prev) => {
            settings.insert("statusLine".into(), prev);
            let _ = std::fs::remove_file(original);
            println!("Restored your original status line.");
        }
        None => {
            settings.remove("statusLine");
        }
    }
}

/// How many hook or status line entries still run the project jazari grew out of (`marsad`). They
/// are reported, not removed: two sets of guards on every tool call would check each call twice.
fn older_entries(settings: &Map<String, Value>) -> usize {
    let older = |c: &str| c.split(['/', '\\', '"', ' ']).any(|w| w == "marsad" || w == "marsad.exe");
    let hooks = settings
        .get("hooks")
        .and_then(Value::as_object)
        .map(|h| {
            h.values()
                .filter_map(Value::as_array)
                .flatten()
                .filter(|e| e["hooks"].as_array().is_some_and(|hs| hs.iter().any(|x| x["command"].as_str().is_some_and(older))))
                .count()
        })
        .unwrap_or(0);
    hooks + usize::from(settings.get("statusLine").and_then(|v| v["command"].as_str()).is_some_and(older))
}

pub fn install(with_statusline: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let (path, mut settings) = load()?;
    let older = older_entries(&settings);
    if older > 0 {
        println!(
            "Note: {older} hook or status line entries in {} still run `marsad`. Remove them, or both would check every tool call.",
            path.display()
        );
    }
    strip(&mut settings);
    if with_statusline {
        install_statusline(&mut settings, &exe.display().to_string(), &crate::statusline::original_path())?;
    }

    add_hooks(&mut settings, &exe.display().to_string())?;
    save(&path, settings)?;
    std::fs::create_dir_all(crate::data_dir())?;
    std::fs::create_dir_all(skill_dir())?;
    std::fs::write(skill_dir().join("SKILL.md"), SKILL_MD.replace("{{JAZARI_EXE}}", &exe.display().to_string()))?;
    println!("Installed jazari hooks for {} events in {}", EVENTS.len(), path.display());
    println!("Installed the jazari-usage skill in {}", skill_dir().display());
    match std::fs::create_dir_all(crate::rules::prompts_dir()) {
        Ok(()) => println!("Add your own prompts as .md files in {} (`jazari build rules` lists them)", crate::rules::prompts_dir().display()),
        Err(e) => eprintln!("warning: couldn't create the prompts folder: {e:#}"),
    }
    crate::terminal::offer_tmux();
    println!("Sessions already running pick them up after a restart (or /hooks reload). Run `jazari` to open the dashboard.");
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let (path, mut settings) = load()?;
    strip(&mut settings);
    uninstall_statusline(&mut settings, &crate::statusline::original_path());
    save(&path, settings)?;
    println!("Removed jazari hooks from {}", path.display());
    if skill_dir().exists() {
        std::fs::remove_dir_all(skill_dir())?;
        println!("Removed the jazari-usage skill");
    }
    if crate::rules::remove_all()? > 0 {
        println!("Removed the generated prompt files (your own prompts are kept)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    #[test]
    fn hooks_of_the_earlier_project_are_counted_not_touched() {
        let settings: Map<String, Value> = serde_json::from_value(json!({
            "hooks": {
                "PreToolUse": [
                    {"hooks": [{"type": "command", "command": "\"/Users/me/.cargo/bin/marsad\" guard --hook"}]},
                    {"hooks": [{"type": "command", "command": "\"/Users/me/.cargo/bin/jazari\" guard --hook"}]},
                    {"hooks": [{"type": "command", "command": "my-marsad-like-tool run"}]}
                ],
                "Stop": [{"hooks": [{"type": "command", "command": "C:\\bin\\marsad.exe hook"}]}]
            },
            "statusLine": {"type": "command", "command": "\"/Users/me/.cargo/bin/marsad\" statusline"}
        }))
        .unwrap();
        assert_eq!(older_entries(&settings), 3);
        assert_eq!(older_entries(&Map::new()), 0);
    }

    use super::*;

    const OURS: &str = "\"/x/jazari\" statusline";

    fn temp_original(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jazari-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("statusline-original.json")
    }

    fn settings(v: Value) -> Map<String, Value> {
        let Value::Object(map) = v else { unreachable!() };
        map
    }

    #[test]
    fn foreign_statusline_is_wrapped_kept_on_reinstall_and_restored() {
        let original = temp_original("wrap");
        let foreign = json!({ "type": "command", "command": "~/bin/my-line", "padding": 2 });
        let mut s = settings(json!({ "statusLine": foreign.clone() }));

        install_statusline(&mut s, "/x/jazari", &original).unwrap();
        assert_eq!(s["statusLine"]["command"], OURS);
        assert_eq!(s["statusLine"]["padding"], 2, "other statusLine fields are kept");
        let saved: Value = serde_json::from_slice(&std::fs::read(&original).unwrap()).unwrap();
        assert_eq!(saved, foreign);

        install_statusline(&mut s, "/x/jazari", &original).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&original).unwrap()).unwrap();
        assert_eq!(saved, foreign, "re-install must not overwrite the saved original");

        uninstall_statusline(&mut s, &original);
        assert_eq!(s["statusLine"], foreign);
        assert!(!original.exists());
        std::fs::remove_dir_all(original.parent().unwrap()).ok();
    }

    #[test]
    fn statusline_without_original_is_removed_on_uninstall() {
        let original = temp_original("plain");
        let mut s = settings(json!({ "model": "opus" }));
        install_statusline(&mut s, "/x/jazari", &original).unwrap();
        assert_eq!(s["statusLine"]["command"], OURS);
        uninstall_statusline(&mut s, &original);
        assert!(!s.contains_key("statusLine"));
        assert_eq!(s["model"], "opus");

        // A corrupt saved original also falls back to removing ours.
        install_statusline(&mut s, "/x/jazari", &original).unwrap();
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, "not json").unwrap();
        uninstall_statusline(&mut s, &original);
        assert!(!s.contains_key("statusLine"));
        std::fs::remove_dir_all(original.parent().unwrap()).ok();
    }

    #[test]
    fn foreign_statusline_untouched_by_uninstall_and_stale_original_dropped() {
        let original = temp_original("stale");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, r#"{"type":"command","command":"old"}"#).unwrap();

        let foreign = json!({ "type": "command", "command": "mine" });
        let mut s = settings(json!({ "statusLine": foreign.clone() }));
        uninstall_statusline(&mut s, &original);
        assert_eq!(s["statusLine"], foreign, "uninstall leaves a status line it does not own");

        let mut empty = settings(json!({}));
        install_statusline(&mut empty, "/x/jazari", &original).unwrap();
        assert!(!original.exists(), "a stale original from an earlier install is dropped");

        let mut odd = settings(json!({ "statusLine": { "type": "static" } }));
        install_statusline(&mut odd, "/x/jazari", &original).unwrap();
        assert!(!original.exists(), "a statusLine without a command is not wrapped");
        std::fs::remove_dir_all(original.parent().unwrap()).ok();
    }

    #[test]
    fn strip_keeps_foreign_hooks() {
        let mut s = json!({
            "model": "opus",
            "hooks": {
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "\"/x/jazari\" hook" }] },
                    { "hooks": [{ "type": "command", "command": "say done" }] }
                ],
                "PreToolUse": [{ "hooks": [{ "type": "command", "command": "\"/x/jazari\" hook" }] }]
            }
        });
        let Value::Object(ref mut map) = s else { unreachable!() };
        strip(map);
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(s["hooks"].get("PreToolUse").is_none());
        assert_eq!(s["model"], "opus");
    }

    #[test]
    fn reinstall_keeps_one_of_each_jazari_hook() {
        let foreign = json!({ "hooks": [{ "type": "command", "command": "echo hi" }] });
        let mut s = settings(json!({ "hooks": { "UserPromptSubmit": [foreign.clone()] } }));
        for _ in 0..2 {
            strip(&mut s);
            add_hooks(&mut s, "/x/jazari").unwrap();
        }
        let prompt = s["hooks"]["UserPromptSubmit"].as_array().unwrap();
        let commands: Vec<(&str, bool)> = prompt
            .iter()
            .map(|e| (e["hooks"][0]["command"].as_str().unwrap(), e["hooks"][0]["async"] == true))
            .collect();
        assert_eq!(commands, [
            ("echo hi", false),
            ("\"/x/jazari\" hook", true),
            ("\"/x/jazari\" watch context --hook", false),
            ("\"/x/jazari\" guard --hook", false),
        ]);
        let tool: Vec<&str> = s["hooks"]["PreToolUse"].as_array().unwrap().iter().map(|e| e["hooks"][0]["command"].as_str().unwrap()).collect();
        assert_eq!(tool, ["\"/x/jazari\" hook", "\"/x/jazari\" guard --hook"], "the disk guard checks every tool call");
        let after: Vec<&str> = s["hooks"]["PostToolUse"].as_array().unwrap().iter().map(|e| e["hooks"][0]["command"].as_str().unwrap()).collect();
        assert_eq!(after, ["\"/x/jazari\" hook", "\"/x/jazari\" watch context --hook"], "a long turn is warned between tool calls");
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1, "other events get only the async hook");
        strip(&mut s);
        assert_eq!(s["hooks"]["UserPromptSubmit"], json!([foreign]), "uninstall removes all three");
        assert!(s["hooks"].get("PreToolUse").is_none());
        assert!(s["hooks"].get("PostToolUse").is_none());
    }
}

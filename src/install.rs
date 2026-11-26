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

/// A hook or status line entry is jazari's when its command runs a file named `jazari`, wherever
/// that file is: the installed binary, an older install, or a development build. A refresh by the
/// installed binary then replaces every such entry with its own.
fn is_ours(entry: &Value) -> bool {
    entry["hooks"].as_array().is_some_and(|hs| hs.iter().any(|h| h["command"].as_str().and_then(jazari_args).is_some()))
}

/// The arguments of `command` when it runs a file named `jazari` (`jazari.exe` on Windows), quoted
/// or not; None for any other program.
fn jazari_args(command: &str) -> Option<&str> {
    let c = command.trim_start();
    let (program, rest) = match c.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"')?,
        None => c.split_once(char::is_whitespace).unwrap_or((c, "")),
    };
    // Split at both separators: a settings.json can hold a Windows path on any system.
    let name = program.rsplit(['/', '\\']).next()?;
    (name == "jazari" || name.eq_ignore_ascii_case("jazari.exe")).then(|| rest.trim())
}

/// Arguments of the hook that adds the handoff warning to a session's context, on each prompt and
/// after each tool call, so a long turn is warned before it reaches auto-compact.
const CONTEXT_HOOK: &str = " watch context --hook";

/// Arguments of the hook that checks the disk before each tool call and prompt.
const GUARD_HOOK: &str = " guard --hook";

/// Arguments of the hook that lets the dashboard answer a permission prompt or question. It is
/// always registered and exits at once with no output while the feature is off.
const ANSWER_HOOK: &str = " watch answer --hook";

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
        if *event == "PermissionRequest" {
            // Longer than the longest wait plus the Touch ID confirmation, so Claude Code never cuts
            // it short.
            let timeout = crate::answer::MAX_WAIT_SECS + crate::answer::CONFIRM_SECS + 10;
            list.push(json!({
                "hooks": [{ "type": "command", "command": format!("\"{exe}\"{ANSWER_HOOK}"), "timeout": timeout }]
            }));
        }
    }
    Ok(())
}

fn load() -> anyhow::Result<(PathBuf, Map<String, Value>)> {
    let path = settings_path();
    let settings = load_at(&path)?;
    Ok((path, settings))
}

fn load_at(path: &Path) -> anyhow::Result<Map<String, Value>> {
    let settings = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(&text)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        _ => json!({}),
    };
    let Value::Object(map) = settings else { bail!("{} is not a JSON object", path.display()) };
    Ok(map)
}

fn save(path: &Path, settings: Map<String, Value>) -> anyhow::Result<()> {
    if let Some(backup) = save_at(path, settings)? {
        println!("Backed up settings to {}", backup.display());
    }
    Ok(())
}

/// Writes settings.json through a temporary file and a rename, so Claude Code never reads a
/// half-written file. The first write keeps a copy of the user's file, which later writes leave
/// alone. Returns that copy's path when it was made now.
fn save_at(path: &Path, settings: Map<String, Value>) -> anyhow::Result<Option<PathBuf>> {
    // A settings.json that links into a dotfiles folder stays a link: write to its target.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = path.parent().context("settings.json has no folder")?;
    std::fs::create_dir_all(dir)?;
    let backup = path.with_extension("json.jazari-backup");
    let backed_up = if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup)?;
        Some(backup)
    } else {
        None
    };
    let text = serde_json::to_string_pretty(&Value::Object(settings))? + "\n";
    let tmp = path.with_extension(format!("json.jazari-tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("couldn't replace {}", path.display()));
    }
    Ok(backed_up)
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
    v["command"].as_str().and_then(jazari_args) == Some("statusline")
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

/// What `jazari install --refresh` did.
#[derive(Debug, PartialEq)]
pub enum Refresh {
    /// jazari's hooks aren't in settings.json (never installed, or uninstalled): nothing changed.
    NotInstalled,
    /// The hooks were already the ones this version registers.
    Unchanged,
    /// The hooks were replaced with this version's set.
    Updated,
    /// This binary may not touch the user's settings (see `skip_reason`); nothing was read or written.
    Skipped(String),
}

/// jazari's own hook entries, by event, in the order they appear.
fn ours_by_event(settings: &Map<String, Value>) -> Vec<(String, Vec<Value>)> {
    let Some(Value::Object(hooks)) = settings.get("hooks") else { return Vec::new() };
    let mut out: Vec<(String, Vec<Value>)> = hooks
        .iter()
        .filter_map(|(event, list)| {
            let ours: Vec<Value> = list.as_array()?.iter().filter(|e| is_ours(e)).cloned().collect();
            (!ours.is_empty()).then(|| (event.clone(), ours))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Which of jazari's hooks an entry is: the async event log, the context warning or the guard.
/// A jazari entry of any other kind (a newer version's, after a rollback) is `"unknown"`; a
/// refresh removes it.
fn kind_of(entry: &Value) -> Option<&'static str> {
    if !is_ours(entry) {
        return None;
    }
    let args = entry["hooks"][0]["command"].as_str().and_then(jazari_args)?;
    Some(match args {
        "hook" => "hook",
        a if a == CONTEXT_HOOK.trim() => "context",
        a if a == GUARD_HOOK.trim() => "guard",
        a if a == ANSWER_HOOK.trim() => "answer",
        _ => "unknown",
    })
}

/// The hooks jazari has registered, as `<event> <kind>` keys (`PreToolUse guard`). A hook in this
/// record that is missing from settings.json was removed by the user, so a refresh leaves it out.
type Record = std::collections::BTreeSet<String>;

/// What `installed-hooks.json` holds: the installed binary, which alone may refresh the hooks, and
/// the hooks it registered. Versions before 1.0.9 wrote only the list of hooks.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug, PartialEq)]
struct Installed {
    #[serde(default)]
    exe: Option<String>,
    #[serde(default)]
    hooks: Record,
}

fn record_path() -> PathBuf {
    crate::data_dir().join("installed-hooks.json")
}

fn load_record(path: &Path) -> Option<Installed> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Installed>(&bytes)
        .ok()
        .or_else(|| serde_json::from_slice::<Record>(&bytes).ok().map(|hooks| Installed { exe: None, hooks }))
}

fn save_record(path: &Path, record: &Installed) -> anyhow::Result<()> {
    std::fs::create_dir_all(path.parent().context("the record has no folder")?)?;
    std::fs::write(path, serde_json::to_vec_pretty(record)?)?;
    Ok(())
}

/// Every hook this version registers, in order, with its record key.
fn wanted_hooks(exe: &str) -> anyhow::Result<Vec<(String, String, Value)>> {
    let mut wanted = Map::new();
    add_hooks(&mut wanted, exe)?;
    let mut out = Vec::new();
    let Some(Value::Object(wanted)) = wanted.remove("hooks") else { return Ok(out) };
    for (event, list) in wanted {
        for entry in list.as_array().into_iter().flatten() {
            let kind = kind_of(entry).context("add_hooks made an entry that isn't jazari's")?;
            out.push((event.clone(), format!("{event} {kind}"), entry.clone()));
        }
    }
    Ok(out)
}

/// Brings jazari's hooks up to date with this version, when jazari is installed:
/// - a hook that is in settings.json is kept, pointed at `exe`;
/// - a hook new in this version (not in `record`) is added;
/// - a hook in `record` but missing from settings.json was removed by the user and stays out;
/// - a jazari hook this version no longer registers is removed.
///
/// With no record (installs from before the record existed), every hook is added one time. The
/// user's own hooks and settings stay as they are. Of the status line, only jazari's own command
/// is pointed at `exe`; a missing one is not added, and a foreign one is not wrapped. Returns the
/// outcome and the record to keep.
fn refresh_settings(settings: &mut Map<String, Value>, exe: &str, record: Option<&Record>) -> anyhow::Result<(Refresh, Record)> {
    let current = ours_by_event(settings);
    if current.is_empty() {
        return Ok((Refresh::NotInstalled, record.cloned().unwrap_or_default()));
    }
    let present: Record = current
        .iter()
        .flat_map(|(event, list)| list.iter().filter_map(move |e| Some(format!("{event} {}", kind_of(e)?))))
        .collect();
    let wanted = wanted_hooks(exe)?;
    let mut next = record.cloned().unwrap_or_default();
    let mut keep = Map::new();
    for (event, key, entry) in wanted {
        let removed_by_user = record.is_some_and(|r| r.contains(&key)) && !present.contains(&key);
        if !removed_by_user {
            let list = keep.entry(event).or_insert_with(|| json!([]));
            if let Value::Array(list) = list {
                list.push(entry);
            }
        }
        next.insert(key);
    }
    let line = format!("\"{exe}\" statusline");
    let line_moved = settings.get("statusLine").is_some_and(|v| is_our_statusline(v) && v["command"] != json!(line));
    let keep_settings: Map<String, Value> = [("hooks".to_string(), Value::Object(keep.clone()))].into_iter().collect();
    if current == ours_by_event(&keep_settings) && !line_moved {
        return Ok((Refresh::Unchanged, next));
    }
    strip(settings);
    let hooks = settings.entry("hooks").or_insert_with(|| json!({}));
    let Value::Object(hooks) = hooks else { bail!("`hooks` in settings.json is not an object") };
    for (event, entries) in keep {
        let list = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Value::Array(list) = list else { bail!("hooks.{event} is not an array") };
        list.extend(entries.as_array().into_iter().flatten().cloned());
    }
    if line_moved && let Some(v) = settings.get_mut("statusLine") {
        v["command"] = json!(line);
    }
    Ok((Refresh::Updated, next))
}

/// Why this binary must not touch the user's Claude Code settings, or None when it may: it is the
/// installed jazari. A development build (under a cargo `target/` folder), a run with
/// `JAZARI_HOME` (a scratch data folder for tests and development), and any binary other than the
/// one recorded at install time never refresh. `installed` is the path `jazari update` names for
/// the binary it just installed.
fn skip_reason(exe: &Path, recorded: Option<&str>, installed: Option<&Path>, scratch_home: bool) -> Option<String> {
    if scratch_home {
        return Some("JAZARI_HOME is set, so this is a scratch run".into());
    }
    if in_cargo_target(exe) {
        return Some(format!("{} is a development build", exe.display()));
    }
    let same = |a: &Path, b: &Path| std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b) || a == b;
    if let Some(installed) = installed {
        return (!same(exe, installed)).then(|| format!("{} is not the binary that was just installed ({})", exe.display(), installed.display()));
    }
    match recorded {
        Some(r) if !same(exe, Path::new(r)) => Some(format!("{} is not the installed jazari ({r})", exe.display())),
        _ => None,
    }
}

/// Whether `exe` sits in a cargo build folder: under a folder named `target` that cargo marked with
/// its `CACHEDIR.TAG`, or at `target/<profile>/jazari`.
fn in_cargo_target(exe: &Path) -> bool {
    let tagged = exe.ancestors().skip(1).any(|d| d.file_name().is_some_and(|n| n == "target") && d.join("CACHEDIR.TAG").is_file());
    let shaped = exe.parent().and_then(Path::parent).and_then(Path::file_name).is_some_and(|n| n == "target");
    tagged || shaped
}

/// Refreshes the hooks in the settings.json at `path` (see [`refresh_settings`]), with the record
/// at `record_at`, unless [`skip_reason`] says this binary must not. It only writes when something
/// changed, so running it again is safe.
fn refresh_at(path: &Path, record_at: &Path, exe: &str, installed: Option<&Path>, scratch_home: bool) -> anyhow::Result<Refresh> {
    let record = load_record(record_at);
    if let Some(why) = skip_reason(Path::new(exe), record.as_ref().and_then(|r| r.exe.as_deref()), installed, scratch_home) {
        return Ok(Refresh::Skipped(why));
    }
    let mut settings = load_at(path)?;
    let (outcome, hooks) = refresh_settings(&mut settings, exe, record.as_ref().map(|r| &r.hooks))?;
    if outcome == Refresh::Updated {
        save_at(path, settings)?;
    }
    let next = Installed { exe: Some(exe.to_string()), hooks };
    if outcome != Refresh::NotInstalled && record.as_ref() != Some(&next) {
        save_record(record_at, &next)?;
    }
    Ok(outcome)
}

/// Brings an installed jazari's hooks and usage skill up to date with this binary. `jazari update`
/// runs it with the new binary, and the SessionStart hook runs it for users who never update.
/// Prints nothing, so a hook can call it.
pub fn refresh(installed: Option<&Path>) -> anyhow::Result<Refresh> {
    let exe = std::env::current_exe()?.canonicalize()?.display().to_string();
    let outcome = refresh_at(&settings_path(), &record_path(), &exe, installed, crate::home_override().is_some())?;
    if matches!(outcome, Refresh::Unchanged | Refresh::Updated) {
        let skill = SKILL_MD.replace("{{JAZARI_EXE}}", &exe);
        let file = skill_dir().join("SKILL.md");
        if std::fs::read_to_string(&file).ok().as_deref() != Some(skill.as_str()) {
            std::fs::create_dir_all(skill_dir())?;
            std::fs::write(&file, skill)?;
        }
    }
    Ok(outcome)
}

/// `jazari install --refresh`: one line saying what changed.
pub fn refresh_cli(installed: Option<&Path>) -> anyhow::Result<()> {
    match refresh(installed)? {
        Refresh::Skipped(why) => println!("Left Claude Code's settings as they are: {why}."),
        Refresh::NotInstalled => println!("jazari's hooks aren't installed, so Claude Code's settings were left as they are (`jazari install` adds them)."),
        Refresh::Unchanged => println!("Claude Code hooks are up to date."),
        Refresh::Updated => println!(
            "Updated jazari's Claude Code hooks in {}. Running sessions use them after a restart (or /hooks reload).",
            settings_path().display()
        ),
    }
    Ok(())
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
    let hooks: Record = wanted_hooks(&exe.display().to_string())?.into_iter().map(|(_, key, _)| key).collect();
    save_record(&record_path(), &Installed { exe: Some(exe.display().to_string()), hooks })?;
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
    let _ = std::fs::remove_file(record_path());
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
        let permission = s["hooks"]["PermissionRequest"].as_array().unwrap();
        let answer = &permission.last().unwrap()["hooks"][0];
        assert_eq!((permission.len(), answer["command"].as_str(), answer["timeout"].as_u64()), (2, Some("\"/x/jazari\" watch answer --hook"), Some(730)));
        assert!(answer.get("async").is_none(), "the dashboard's answer must reach Claude Code");
        assert_eq!(kind_of(&permission[1]), Some("answer"));
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1, "other events get only the async hook");
        strip(&mut s);
        assert_eq!(s["hooks"]["UserPromptSubmit"], json!([foreign]), "uninstall removes all three");
        assert!(s["hooks"].get("PreToolUse").is_none());
        assert!(s["hooks"].get("PostToolUse").is_none());
    }

    /// A settings.json from before the context and guard hooks: only the async hook, next to the
    /// user's own rtk hook, a permission list and jazari's status line.
    fn old_install(exe: &str) -> Value {
        let ours = json!({ "hooks": [{ "type": "command", "command": format!("\"{exe}\" hook"), "async": true, "timeout": 10 }] });
        let rtk = json!({ "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] });
        json!({
            "permissions": { "allow": ["Bash(ls:*)"] },
            "statusLine": { "type": "command", "command": format!("\"{exe}\" statusline"), "padding": 0 },
            "hooks": { "SessionStart": [ours.clone()], "PreToolUse": [ours.clone(), rtk], "PostToolUse": [ours] }
        })
    }

    fn temp_settings(name: &str, v: &Value) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jazari-refresh-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(v).unwrap()).unwrap();
        path
    }

    fn commands(s: &Map<String, Value>, event: &str) -> Vec<String> {
        s["hooks"][event].as_array().unwrap().iter().map(|e| e["hooks"][0]["command"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn refresh_adds_missing_hooks_and_keeps_the_users_own() {
        let path = temp_settings("adds", &old_install("/x/jazari"));
        assert_eq!(refresh_at(&path, &path.with_file_name("record.json"), "/x/jazari", None, false).unwrap(), Refresh::Updated);
        let s = load_at(&path).unwrap();
        assert_eq!(commands(&s, "PreToolUse"), ["rtk hook claude", "\"/x/jazari\" hook", "\"/x/jazari\" guard --hook"]);
        assert_eq!(s["hooks"]["PreToolUse"][0]["matcher"], "Bash", "the rtk entry is kept whole");
        assert_eq!(commands(&s, "PostToolUse"), ["\"/x/jazari\" hook", "\"/x/jazari\" watch context --hook"]);
        assert_eq!(s["hooks"].as_object().unwrap().len(), EVENTS.len());
        assert_eq!(s["permissions"], json!({ "allow": ["Bash(ls:*)"] }));
        assert_eq!(s["statusLine"], old_install("/x/jazari")["statusLine"], "the status line is left as it was");
        let backup = path.with_extension("json.jazari-backup");
        assert_eq!(load_at(&backup).unwrap(), settings(old_install("/x/jazari")), "the first write keeps the user's file");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refresh_twice_changes_nothing_the_second_time() {
        let path = temp_settings("twice", &old_install("/x/jazari"));
        refresh_at(&path, &path.with_file_name("record.json"), "/x/jazari", None, false).unwrap();
        let first = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(refresh_at(&path, &path.with_file_name("record.json"), "/x/jazari", None, false).unwrap(), Refresh::Unchanged);
        assert_eq!(std::fs::read(&path).unwrap(), first);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified, "an unchanged file isn't rewritten");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("tmp")).collect();
        assert!(leftovers.is_empty(), "no temporary file is left");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refresh_leaves_an_uninstalled_jazari_uninstalled() {
        let rtk = json!({ "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "rtk hook claude" }] }] } });
        let path = temp_settings("none", &rtk);
        let before = std::fs::read(&path).unwrap();
        assert_eq!(refresh_at(&path, &path.with_file_name("record.json"), "/x/jazari", None, false).unwrap(), Refresh::NotInstalled);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("json.jazari-backup").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refresh_points_the_hooks_and_our_status_line_at_a_moved_binary() {
        let mut s = settings(old_install("/old/jazari"));
        assert_eq!(refresh_settings(&mut s, "/new/jazari", None).unwrap().0, Refresh::Updated);
        assert!(commands(&s, "SessionStart").iter().all(|c| c.starts_with("\"/new/jazari\"")));
        assert_eq!(s["statusLine"]["command"], "\"/new/jazari\" statusline");
        assert_eq!(s["statusLine"]["padding"], 0, "the status line's other fields are kept");
        let mut foreign = settings(json!({ "statusLine": { "type": "command", "command": "my-line" }, "hooks": old_install("/x/jazari")["hooks"] }));
        refresh_settings(&mut foreign, "/x/jazari", None).unwrap();
        assert_eq!(foreign["statusLine"]["command"], "my-line", "a foreign status line is not wrapped by a refresh");
    }

    #[cfg(unix)]
    #[test]
    fn refresh_writes_through_a_linked_settings_file() {
        let path = temp_settings("link", &old_install("/x/jazari"));
        let link = path.with_file_name("linked.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(refresh_at(&link, &path.with_file_name("record.json"), "/x/jazari", None, false).unwrap(), Refresh::Updated);
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link stays a link");
        assert_eq!(commands(&load_at(&path).unwrap(), "PostToolUse").len(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refresh_never_adds_back_a_hook_the_user_removed() {
        let path = temp_settings("removed", &old_install("/x/jazari"));
        let record = path.with_file_name("record.json");
        refresh_at(&path, &record, "/x/jazari", None, false).unwrap();
        assert!(load_record(&record).unwrap().hooks.contains("PreToolUse guard"), "the first refresh writes the record");
        // The user removes the guard hook from PreToolUse by hand.
        let mut s = load_at(&path).unwrap();
        s["hooks"]["PreToolUse"].as_array_mut().unwrap().retain(|e| kind_of(e) != Some("guard"));
        save_at(&path, s).unwrap();
        assert_eq!(refresh_at(&path, &record, "/x/jazari", None, false).unwrap(), Refresh::Unchanged);
        assert_eq!(commands(&load_at(&path).unwrap(), "PreToolUse"), ["rtk hook claude", "\"/x/jazari\" hook"]);
        // A newer binary in another place keeps it out too, and moves the rest.
        assert_eq!(refresh_at(&path, &record, "/new/jazari", Some(Path::new("/new/jazari")), false).unwrap(), Refresh::Updated);
        assert_eq!(commands(&load_at(&path).unwrap(), "PreToolUse"), ["rtk hook claude", "\"/new/jazari\" hook"]);
        assert_eq!(commands(&load_at(&path).unwrap(), "UserPromptSubmit").len(), 3, "the guard on prompts was not removed, so it stays");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refresh_adds_a_hook_that_is_new_in_this_version() {
        let mut s = settings(old_install("/x/jazari"));
        // The record of an older version that had only the async hook on these three events.
        let record: Record = ["SessionStart hook", "PreToolUse hook", "PostToolUse hook"].map(String::from).into();
        let (outcome, next) = refresh_settings(&mut s, "/x/jazari", Some(&record)).unwrap();
        assert_eq!(outcome, Refresh::Updated);
        assert_eq!(commands(&s, "PreToolUse"), ["rtk hook claude", "\"/x/jazari\" hook", "\"/x/jazari\" guard --hook"]);
        assert_eq!(s["hooks"].as_object().unwrap().len(), EVENTS.len(), "events new to the record get their hooks");
        assert!(next.contains("PostToolUse context") && next.contains("Stop hook"));
    }

    #[test]
    fn a_development_build_or_a_scratch_run_never_writes() {
        let path = temp_settings("dev", &old_install("/x/jazari"));
        let before = std::fs::read(&path).unwrap();
        let record = path.with_file_name("record.json");
        // A cargo build folder, marked as cargo marks it.
        let target = path.parent().unwrap().join("repo/target");
        std::fs::create_dir_all(target.join("debug")).unwrap();
        std::fs::write(target.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        let debug = target.join("debug/jazari");
        std::fs::write(&debug, b"").unwrap();
        let out = refresh_at(&path, &record, &debug.display().to_string(), None, false).unwrap();
        assert!(matches!(&out, Refresh::Skipped(why) if why.contains("development build")), "{out:?}");
        let out = refresh_at(&path, &record, "/x/jazari", None, true).unwrap();
        assert!(matches!(&out, Refresh::Skipped(why) if why.contains("JAZARI_HOME")), "{out:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "settings.json is untouched");
        assert!(!record.exists(), "the record is untouched");
        assert!(in_cargo_target(Path::new("/w/repo/target/release/jazari")), "the shape alone counts");
        assert!(!in_cargo_target(Path::new("/Users/me/.jazari/bin/jazari")));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn only_the_installed_binary_refreshes() {
        let path = temp_settings("owner", &old_install("/x/jazari"));
        let record = path.with_file_name("record.json");
        refresh_at(&path, &record, "/x/jazari", None, false).unwrap();
        assert_eq!(load_record(&record).unwrap().exe.as_deref(), Some("/x/jazari"), "a missing exe is recorded");
        let before = std::fs::read(&path).unwrap();
        let out = refresh_at(&path, &record, "/other/jazari", None, false).unwrap();
        assert!(matches!(&out, Refresh::Skipped(why) if why.contains("not the installed jazari")), "{out:?}");
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // `jazari update` names the binary it installed, which may be in a new place.
        assert_eq!(refresh_at(&path, &record, "/new/jazari", Some(Path::new("/new/jazari")), false).unwrap(), Refresh::Updated);
        assert_eq!(load_record(&record).unwrap().exe.as_deref(), Some("/new/jazari"));
        let out = refresh_at(&path, &record, "/x/jazari", Some(Path::new("/new/jazari")), false).unwrap();
        assert!(matches!(out, Refresh::Skipped(_)), "only the binary that was just installed");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_record_from_before_holds_only_the_hooks() {
        let path = std::env::temp_dir().join(format!("jazari-old-record-{}.json", std::process::id()));
        std::fs::write(&path, r#"["PreToolUse guard","Stop hook"]"#).unwrap();
        let r = load_record(&path).unwrap();
        assert_eq!(r.exe, None);
        assert!(r.hooks.contains("PreToolUse guard"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_real_refresh_replaces_stray_and_unknown_jazari_entries() {
        let mut v = old_install("/x/jazari");
        let stray = |cmd: &str| json!({ "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }] });
        v["hooks"]["PermissionRequest"] = json!([stray("/w/jazari-answer-prompts/target/debug/jazari watch answer --hook")]);
        v["hooks"]["PreToolUse"].as_array_mut().unwrap().push(stray("\"/w/repo/target/debug/jazari\" guard --hook"));
        v["hooks"]["Stop"] = json!([stray("/opt/jazari-next/jazari watch future --hook"), stray("/usr/local/bin/jazari.sh hook"), stray("my-jazari-notes hook")]);
        let path = temp_settings("stray", &v);
        let record = path.with_file_name("record.json");
        assert_eq!(refresh_at(&path, &record, "/x/jazari", None, false).unwrap(), Refresh::Updated);
        let s = load_at(&path).unwrap();
        let all: Vec<String> = s["hooks"].as_object().unwrap().values().flat_map(|l| l.as_array().unwrap().iter().map(|e| e["hooks"][0]["command"].as_str().unwrap().to_string())).collect();
        assert!(all.iter().all(|c| !c.contains("target/debug") && !c.contains("watch future")), "{all:?}");
        assert_eq!(commands(&s, "PreToolUse"), ["rtk hook claude", "\"/x/jazari\" hook", "\"/x/jazari\" guard --hook"]);
        assert_eq!(commands(&s, "PermissionRequest"), ["\"/x/jazari\" hook", "\"/x/jazari\" watch answer --hook"], "the debug answer hook now runs the installed binary");
        assert_eq!(commands(&s, "Stop"), ["/usr/local/bin/jazari.sh hook", "my-jazari-notes hook", "\"/x/jazari\" hook"], "files not named jazari are the user's own");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ownership_goes_by_the_program_file_name() {
        assert_eq!(jazari_args("\"/Users/me/.jazari/bin/jazari\" guard --hook"), Some("guard --hook"));
        assert_eq!(jazari_args("/a b/../jazari hook"), None, "an unquoted path with a space is cut at the space");
        assert_eq!(jazari_args("jazari statusline"), Some("statusline"));
        assert_eq!(jazari_args("\"C:\\Tools\\jazari.exe\" hook"), Some("hook"));
        for not in ["jazari.sh hook", "/bin/jazari-dev hook", "echo jazari hook", "\"/x/jazari", ""] {
            assert_eq!(jazari_args(not), None, "{not}");
        }
        assert_eq!(kind_of(&json!({ "hooks": [{ "command": "/w/target/debug/jazari watch answer --hook" }] })), Some("answer"));
        assert_eq!(kind_of(&json!({ "hooks": [{ "command": "/w/target/debug/jazari watch future --hook" }] })), Some("unknown"));
    }

    #[test]
    fn a_refresh_keeps_the_answer_hook() {
        let mut v = old_install("/x/jazari");
        let mut installed = Map::new();
        add_hooks(&mut installed, "/x/jazari").unwrap();
        v["hooks"]["PermissionRequest"] = installed["hooks"]["PermissionRequest"].clone();
        let answer = |s: &Map<String, Value>| s["hooks"]["PermissionRequest"].as_array().unwrap().iter().find(|e| kind_of(e) == Some("answer")).cloned();
        let before = answer(&settings(v.clone())).expect("the fixture has the answer hook");
        let path = temp_settings("answer", &v);
        let record = path.with_file_name("record.json");
        refresh_at(&path, &record, "/x/jazari", None, false).unwrap();
        assert!(load_record(&record).unwrap().hooks.contains("PermissionRequest answer"));
        assert_eq!(answer(&load_at(&path).unwrap()), Some(before.clone()), "kept as it was, timeout included");
        assert_eq!(refresh_at(&path, &record, "/x/jazari", None, false).unwrap(), Refresh::Unchanged);
        assert_eq!(answer(&load_at(&path).unwrap()), Some(before));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

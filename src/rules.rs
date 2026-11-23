//! The prompts a Claude Code session can start with: the ones marsad ships (coordination, output
//! hygiene) and the user's own `.md` files in `<data dir>/prompts/`.
//!
//! `claude` reads only the last `--append-system-prompt-file` it is given, so a chosen set is merged
//! into one file, written when its command is copied. The file is named `<role>-<keys>-<hash>.md`:
//! its role and prompt keys, so a person recognizes it, and a hash of its text, so one set always
//! maps to one file and an edited prompt gets a new one. A session's or lead's file
//! whose command hasn't been copied for `KEEP_FOR` is pruned: a conversation keeps the system
//! prompt it started with, even on resume. A teammates' file is never pruned, since a lead reads
//! its path from that kept prompt each time it spawns a teammate, however long after.
//!
//! A set can start a plain session or the lead of an agent team. A teammate starts with the
//! project's CLAUDE.md, skills and MCP servers and the lead's spawn prompt, never the lead's
//! system prompt (checked in Claude Code 2.1.330, in-process and split-pane), so a team gets two
//! files: the lead's, which also tells it to start every spawn prompt with "read the teammates'
//! file", and the teammates' file. Coordination goes to the lead only: it represents the team on
//! the agent board, and teammates coordinate through the team's task list and messages.

use anyhow::Context;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// One prompt in the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The name on the command line and in the settings: a built-in key, or the file's stem.
    pub key: String,
    pub title: String,
    pub summary: String,
    body: String,
    /// The user's own prompt from the prompts dir, not one marsad ships.
    pub own: bool,
}

impl Rule {
    /// The prompt's text, as it goes into the launch file.
    pub fn body(&self) -> &str {
        &self.body
    }
}

struct Builtin {
    key: &'static str,
    title: &'static str,
    summary: &'static str,
    body: &'static str,
}

/// The prompts marsad ships, listed and merged before the user's own. Add new ones at the end.
const BUILTIN: &[Builtin] = &[
    Builtin {
        key: COORDINATION,
        title: "Coordination",
        summary: "sessions in one repo share AGENT-BOARD.md and claim files before editing",
        body: include_str!("../assets/COORDINATION.md"),
    },
    Builtin {
        key: "output-hygiene",
        title: "Output hygiene",
        summary: "keeps the request, the requester and the process out of code, commits and docs",
        body: include_str!("../assets/OUTPUT-HYGIENE.md"),
    },
];

/// The coordination prompt's key, the default choice before the user picks one.
pub const COORDINATION: &str = "coordination";

/// How long a session's or lead's file stays after its command was last copied.
const KEEP_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The name prefix of teammates' files, which are never pruned.
const TEAMMATE_PREFIX: &str = "teammate-";

/// The longest run of prompt keys in a launch file's name.
const MAX_NAME_KEYS: usize = 60;

/// Files at paths earlier versions printed (in the legacy dir), with the built-in keys each holds.
/// Kept current while they exist, so saved commands and aliases keep working. `marsad
/// coordination` prints the first, in the data dir. To be dropped in a later release.
const LEGACY_FILES: &[(&str, &[&str])] = &[
    ("coordination.md", &[COORDINATION]),
    ("output-hygiene.md", &["output-hygiene"]),
    ("coordination+output-hygiene.md", &[COORDINATION, "output-hygiene"]),
];

/// Whether the command starts a plain session or the lead of an agent team.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Session,
    Team,
}

impl Mode {
    pub fn key(self) -> &'static str {
        match self {
            Mode::Session => "session",
            Mode::Team => "team",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        [Mode::Session, Mode::Team].into_iter().find(|m| m.key() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Mode::Session => "Normal session",
            Mode::Team => "Agent team",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            Mode::Session => "one Claude Code session follows the prompts",
            Mode::Team => "the lead follows them and passes them on to every teammate it spawns",
        }
    }
}

/// Where the user's own prompts live: one `.md` file each.
pub fn prompts_dir() -> PathBuf {
    crate::data_dir().join("prompts")
}

fn launch_dir(data: &Path) -> PathBuf {
    data.join("launch")
}

/// The built-in prompts, then the user's own in file-name order.
pub fn library() -> Vec<Rule> {
    library_in(&prompts_dir())
}

fn builtin_library() -> Vec<Rule> {
    BUILTIN
        .iter()
        .map(|b| Rule { key: b.key.into(), title: b.title.into(), summary: b.summary.into(), body: b.body.into(), own: false })
        .collect()
}

pub(crate) fn library_in(dir: &Path) -> Vec<Rule> {
    let mut rules = builtin_library();
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).collect())
        .unwrap_or_default();
    files.sort();
    for path in files {
        let Some(key) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        // `-x` would read as a flag in `marsad rules -x`.
        if key == "all" || key.starts_with('-') || rules.iter().any(|r| r.key == key) {
            continue;
        }
        if let Some(rule) = std::fs::read_to_string(&path).ok().and_then(|text| own_rule(key, text)) {
            rules.push(rule);
        }
    }
    rules
}

/// A user's prompt: titled by its first `# ` heading (or its key), summarised by its first line of
/// text. None for an empty file.
fn own_rule(key: String, text: String) -> Option<Rule> {
    let (title, summary) = title_and_summary(&key, &text)?;
    Some(Rule { key, title, summary, body: text, own: true })
}

/// The title and summary the library reads from a prompt file's `text`. None for an empty file.
pub fn title_and_summary(key: &str, text: &str) -> Option<(String, String)> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    let title = first.strip_prefix("# ").map(str::trim).filter(|t| !t.is_empty()).unwrap_or(key).to_string();
    let rest = if first.starts_with("# ") { lines.find(|l| !l.starts_with('#')) } else { Some(first) };
    Some((title, rest.map(|l| shorten(l, 80)).unwrap_or_default()))
}

/// The longest key a new prompt may have.
const MAX_KEY: usize = 41;

/// Why `key` can't name a new or renamed prompt, or None when it can. `replacing` is the key of
/// the user's prompt being edited, which may keep its name.
pub fn key_problem(lib: &[Rule], key: &str, replacing: Option<&str>) -> Option<String> {
    if Some(key) == replacing {
        return None;
    }
    if !well_formed(key) {
        return Some(format!("a name is 2-{MAX_KEY} lowercase letters, digits and hyphens, starting with a letter or digit"));
    }
    if key == "all" {
        return Some("\"all\" stands for every prompt".into());
    }
    match lib.iter().find(|r| r.key == key) {
        Some(r) if r.own => Some(format!("you already have a prompt named {key}")),
        Some(_) => Some(format!("{key} is a built-in prompt")),
        None => None,
    }
}

/// 2 to `MAX_KEY` lowercase letters, digits and hyphens, starting with a letter or digit.
fn well_formed(key: &str) -> bool {
    (2..=MAX_KEY).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `title` as a key: lowercase ASCII letters and digits, with hyphens between words.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out: String = out.chars().take(MAX_KEY).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// A free key for a prompt titled `title`, made from `preferred` when that is valid: its slug,
/// with a number added when that name is taken.
pub fn suggest_key(lib: &[Rule], preferred: &str, title: &str) -> String {
    let base = [preferred, &slug(preferred), &slug(title)]
        .into_iter()
        .find(|k| well_formed(k) && *k != "all")
        .unwrap_or("my-prompt")
        .to_string();
    if key_problem(lib, &base, None).is_none() {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            let stem: String = base.chars().take(MAX_KEY - suffix.len()).collect();
            format!("{}{suffix}", stem.trim_end_matches('-'))
        })
        .find(|k| key_problem(lib, k, None).is_none())
        .unwrap_or_default()
}

/// Why `rule` can't be copied into the user's library, or None when it can. Agent teams keep the
/// coordination rules with the lead by their key, so a copy under another name would reach every
/// teammate.
pub fn copy_problem(rule: &Rule) -> Option<String> {
    (rule.key == COORDINATION).then(|| {
        "the coordination rules can't be copied: an agent team recognises them by name to keep them with the lead".to_string()
    })
}

/// Saves `body` as the user's prompt `key`, replacing the file in one step. When `replacing` names
/// another of the user's prompts (a rename), that file is removed once the new one is written.
/// Returns the new file's path.
pub fn save_own(key: &str, body: &str, replacing: Option<&str>) -> anyhow::Result<PathBuf> {
    save_in(&prompts_dir(), &library(), key, body, replacing)
}

fn save_in(dir: &Path, lib: &[Rule], key: &str, body: &str, replacing: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(why) = key_problem(lib, key, replacing) {
        anyhow::bail!("can't save as {key:?}: {why}");
    }
    if let Some(old) = replacing {
        anyhow::ensure!(lib.iter().any(|r| r.key == old && r.own), "{old} is not one of your prompts");
    }
    anyhow::ensure!(!body.trim().is_empty(), "the prompt is empty");
    let path = dir.join(format!("{key}.md"));
    write_if_changed(&path, &format!("{}\n", body.trim_end()))?;
    if let Some(old) = replacing.filter(|old| *old != key) {
        let old = dir.join(format!("{old}.md"));
        std::fs::remove_file(&old).with_context(|| format!("removing {}", old.display()))?;
    }
    Ok(path)
}

/// Deletes the user's prompt `key`. Built-in prompts can't be deleted. Returns the removed path.
pub fn remove_own(key: &str) -> anyhow::Result<PathBuf> {
    remove_in(&prompts_dir(), &library(), key)
}

fn remove_in(dir: &Path, lib: &[Rule], key: &str) -> anyhow::Result<PathBuf> {
    match lib.iter().find(|r| r.key == key) {
        None => anyhow::bail!("no prompt named {key:?}"),
        Some(r) if !r.own => anyhow::bail!("{key} is built in and can't be deleted"),
        Some(_) => {
            let path = dir.join(format!("{key}.md"));
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            Ok(path)
        }
    }
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max - 1).collect::<String>().trim_end()) }
}

/// The rules named by `keys`, in library order; `all` stands for every rule.
pub fn parse<'a, S: AsRef<str>>(lib: &'a [Rule], keys: &[S]) -> anyhow::Result<Vec<&'a Rule>> {
    for key in keys {
        let key = key.as_ref();
        if key != "all" && !lib.iter().any(|r| r.key == key) {
            let names: Vec<_> = lib.iter().map(|r| r.key.as_str()).collect();
            anyhow::bail!("no prompt named {key:?}; choose from {} or all", names.join(", "));
        }
    }
    Ok(known(lib, keys))
}

/// The rules named by `keys`, dropping any not in the library (a removed prompt, or a newer version's).
pub fn known<'a, S: AsRef<str>>(lib: &'a [Rule], keys: &[S]) -> Vec<&'a Rule> {
    let all = keys.iter().any(|k| k.as_ref() == "all");
    lib.iter().filter(|r| all || keys.iter().any(|k| k.as_ref() == r.key)).collect()
}

/// The titles joined with " + ", for messages.
pub fn titles(chosen: &[&Rule]) -> String {
    chosen.iter().map(|r| r.title.to_lowercase()).collect::<Vec<_>>().join(" + ")
}

/// Writes the file(s) for `chosen` started as `mode`, marks them as just used, and returns the path
/// the command loads (the lead's, for a team).
pub fn ensure(chosen: &[&Rule], mode: Mode) -> anyhow::Result<PathBuf> {
    ensure_in(&crate::data_dir(), chosen, mode)
}

fn ensure_in(data: &Path, chosen: &[&Rule], mode: Mode) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!chosen.is_empty(), "tick at least one prompt");
    let files = files_for(data, chosen, mode);
    // The teammates' file first, so the lead's never points at a file that isn't there yet.
    for (path, text) in files.iter().rev() {
        write_if_changed(path, text)?;
        // Copying a command counts as using its files, so pruning keeps them.
        std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(SystemTime::now()))
            .with_context(|| format!("marking {} as used", path.display()))?;
    }
    Ok(files[0].0.clone())
}

/// The files for `chosen` started as `mode`: the one the command loads, then for a team the
/// teammates' file when any prompt reaches them.
fn files_for(data: &Path, chosen: &[&Rule], mode: Mode) -> Vec<(PathBuf, String)> {
    let named = |role: &str, rules: &[&Rule], text: &str| {
        let path = launch_dir(data).join(launch_name(role, rules, text));
        std::path::absolute(&path).unwrap_or(path)
    };
    let merged = merge(chosen);
    if mode == Mode::Session {
        return vec![(named("session", chosen, &merged), merged)];
    }
    let shared: Vec<&Rule> = chosen.iter().copied().filter(|r| r.key != COORDINATION).collect();
    let coordination = shared.len() < chosen.len();
    let teammates = (!shared.is_empty()).then(|| {
        let text = merge(&shared);
        (named("teammate", &shared, &text), text)
    });
    let lead = format!("{merged}\n{}", team_section(teammates.as_ref().map(|(p, _)| p.as_path()), coordination));
    let mut files = vec![(named("lead", chosen, &lead), lead)];
    files.extend(teammates);
    files
}

/// `<role>-<keys>-<hash>.md`: the keys joined with `+` and cut to `MAX_NAME_KEYS` characters, and
/// the first 8 hex digits of the hash of the role and text. `teammate` starts with
/// `TEAMMATE_PREFIX`.
fn launch_name(role: &str, rules: &[&Rule], text: &str) -> String {
    // Own prompts are named after any file stem: keep what is safe in a file name on every platform.
    let safe = |c: char| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' };
    let keys: String = rules.iter().map(|r| r.key.chars().map(safe).collect::<String>()).collect::<Vec<_>>().join("+");
    let keys: String = keys.chars().take(MAX_NAME_KEYS).collect();
    let hash = format!("{:016x}", fnv1a(format!("{role}\n{text}").as_bytes()));
    format!("{role}-{}-{}.md", keys.trim_end_matches(['+', '-', '.']), &hash[..8])
}

fn merge(rules: &[&Rule]) -> String {
    rules.iter().map(|r| r.body.trim_end()).collect::<Vec<_>>().join("\n\n") + "\n"
}

/// The lead's instructions: hand the teammates' file to each teammate it spawns, and with
/// coordination, stand in for the whole team on the agent board.
fn team_section(teammates: Option<&Path>, coordination: bool) -> String {
    let mut out = String::from(
        "# Agent team\n\
         \n\
         You lead an agent team. A teammate starts with the project's CLAUDE.md, skills and MCP servers \
         and the spawn prompt you write. It never sees this system prompt or your conversation.\n",
    );
    if let Some(path) = teammates {
        out.push_str(&format!(
            "\nEach time you spawn a teammate, start its spawn prompt with this line, then give its task:\n\
             \n\
             > Before any other work, read `{}` and follow it for the rest of your work.\n",
            path.display()
        ));
    }
    if coordination {
        out.push_str(
            "\nThe coordination rules above are yours alone. Teammates don't use the agent board; they \
             coordinate through the team's task list and messages. You represent the whole team on the \
             board: before you assign a task, claim the paths that teammate will edit, name those paths \
             in its spawn prompt or task, and release them when the task is done.\n",
        );
    }
    out
}

/// 64-bit FNV-1a: stable across versions and platforms, which `DefaultHasher` doesn't promise.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

pub(crate) fn write_if_changed(path: &Path, content: &str) -> anyhow::Result<()> {
    if std::fs::read(path).is_ok_and(|current| current == content.as_bytes()) {
        return Ok(());
    }
    let dir = path.parent().context("rules path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().context("rules path has no file name")?.to_string_lossy();
    // Write beside the target and rename, so a launching `claude` never reads a torn file.
    let tmp = dir.join(format!("{name}.tmp-{}", std::process::id()));
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// Removes session and lead files unused for `KEEP_FOR`, and rewrites the files at paths earlier
/// versions printed with this build's text. Returns how many files were removed. One file that
/// can't be removed doesn't stop the rest.
/// Nothing in the legacy dir is removed.
pub fn prune() -> anyhow::Result<usize> {
    prune_in(&crate::data_dir(), &crate::legacy_dir(), SystemTime::now())
}

fn prune_in(data: &Path, legacy: &Path, now: SystemTime) -> anyhow::Result<usize> {
    let refreshed = refresh_legacy(data).and(refresh_legacy(legacy));
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(launch_dir(data)) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || !name.ends_with(".md") || name.starts_with(TEAMMATE_PREFIX) {
                continue;
            }
            if meta.modified().is_ok_and(|t| now.duration_since(t).unwrap_or_default() > KEEP_FOR)
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    refreshed.map(|()| removed)
}

/// Rewrites each legacy file that exists with this build's text.
fn refresh_legacy(data: &Path) -> anyhow::Result<()> {
    let lib = builtin_library();
    for (name, keys) in LEGACY_FILES {
        let path = data.join(name);
        if path.exists() {
            write_if_changed(&path, &merge(&known(&lib, keys)))?;
        }
    }
    Ok(())
}

fn remove_legacy(data: &Path) -> anyhow::Result<usize> {
    let mut removed = 0;
    for (name, _) in LEGACY_FILES {
        let path = data.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
    Ok(removed)
}

/// Removes every generated file in the data dir, keeping the user's own prompts. Returns how many
/// were there. The legacy dir is left as it is.
pub fn remove_all() -> anyhow::Result<usize> {
    remove_all_in(&crate::data_dir())
}

fn remove_all_in(data: &Path) -> anyhow::Result<usize> {
    let mut removed = remove_legacy(data)?;
    let dir = launch_dir(data);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        removed += entries.count();
        std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(removed)
}

/// Quotes `s` for the current platform's shell.
pub fn quote_arg(s: &str) -> String {
    quote_for(s, cfg!(windows))
}

fn quote_for(s: &str, windows: bool) -> String {
    let bare = |c: char| c.is_ascii_alphanumeric() || "_./:@%+=,-".contains(c);
    if !s.is_empty() && s.chars().all(bare) {
        s.to_string()
    } else if windows {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// The command that starts a Claude Code session with the rules file at `path`, without a trailing newline.
pub fn launch_command(path: &Path) -> String {
    format!("claude --append-system-prompt-file {}", quote_arg(&path.to_string_lossy()))
}

/// The `marsad rules …` command that prints the same launch command.
pub fn cli_command(chosen: &[&Rule], mode: Mode) -> String {
    // Keys never start with `-` (see `library_in`), so they can't read as flags.
    let mut parts = vec!["marsad rules".to_string()];
    parts.extend(chosen.iter().map(|r| quote_arg(&r.key)));
    if mode == Mode::Team {
        parts.push("--team".into());
    }
    parts.join(" ")
}

#[derive(clap::Args)]
pub struct RulesArgs {
    /// Prompts to start with, e.g. `coordination output-hygiene`, or `all`. Without any, lists them.
    keys: Vec<String>,
    /// Start the lead of an agent team, which has every teammate it spawns read the same prompts.
    #[arg(long)]
    team: bool,
    #[arg(long, value_enum, default_value = "table")]
    format: crate::report::Format,
}

#[derive(clap::Args)]
pub struct CoordinationArgs {
    #[arg(long, value_enum, default_value = "table")]
    format: crate::report::Format,
}

/// `marsad rules [keys…] [--team]`: lists the prompts, or writes the chosen set's file and prints the command that loads it.
pub fn run(args: RulesArgs) -> anyhow::Result<()> {
    let _ = prune();
    let lib = library();
    if args.keys.is_empty() {
        println!("{}", render_list(args.format, &lib, &prompts_dir()));
        return Ok(());
    }
    let mode = if args.team { Mode::Team } else { Mode::Session };
    print_command(&parse(&lib, &args.keys)?, mode, args.format)
}

/// `marsad coordination`: the command for the coordination rules alone, at a stable path, so a
/// saved command or alias keeps working across upgrades.
pub fn run_coordination(args: CoordinationArgs) -> anyhow::Result<()> {
    let _ = prune();
    let path = crate::data_dir().join(LEGACY_FILES[0].0);
    let path = std::path::absolute(&path).unwrap_or(path);
    let lib = builtin_library();
    let chosen = known(&lib, &[COORDINATION]);
    write_if_changed(&path, &merge(&chosen))?;
    println!("{}", render(args.format, &chosen, Mode::Session, &launch_command(&path), &path));
    Ok(())
}

fn print_command(chosen: &[&Rule], mode: Mode, format: crate::report::Format) -> anyhow::Result<()> {
    // Written before anything is printed: a command pointing at a missing file is worse than none.
    let path = ensure(chosen, mode)?;
    println!("{}", render(format, chosen, mode, &launch_command(&path), &path));
    Ok(())
}

fn render_list(format: crate::report::Format, lib: &[Rule], dir: &Path) -> String {
    use crate::report::Format;
    match format {
        Format::Table => {
            let width = lib.iter().map(|r| r.key.chars().count()).max().unwrap_or(0);
            let mut out: Vec<String> =
                lib.iter().map(|r| format!("{:<width$}  {}{}", r.key, r.summary, if r.own { "  (yours)" } else { "" })).collect();
            out.push(String::new());
            out.push("`marsad rules <name>…` prints the command that starts Claude Code with them, e.g.".into());
            out.push(format!("marsad rules {}", lib.iter().map(|r| quote_arg(&r.key)).collect::<Vec<_>>().join(" ")));
            out.push("Add --team to start the lead of an agent team that passes them to its teammates.".into());
            out.push("Write your own with `marsad prompt new \"<rough idea>\"`, or add .md files to".into());
            out.push(dir.display().to_string());
            out.join("\n")
        }
        Format::Json => {
            let list: Vec<_> =
                lib.iter().map(|r| serde_json::json!({ "name": r.key, "title": r.title, "summary": r.summary, "own": r.own })).collect();
            serde_json::to_string_pretty(&list).unwrap_or_default()
        }
        Format::Csv => {
            let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            let mut out = vec!["name,title,summary,own".to_string()];
            out.extend(lib.iter().map(|r| format!("{},{},{},{}", quote(&r.key), quote(&r.title), quote(&r.summary), r.own)));
            out.join("\n")
        }
    }
}

fn render(format: crate::report::Format, chosen: &[&Rule], mode: Mode, command: &str, path: &Path) -> String {
    use crate::report::Format;
    let path = path.display().to_string();
    let keys: Vec<&str> = chosen.iter().map(|r| r.key.as_str()).collect();
    match format {
        // The command alone on the first line, so it can be copied or piped as is.
        Format::Table => {
            let team = if mode == Mode::Team {
                "\nAgent team: the lead passes the prompts on to every teammate it spawns (coordination stays with the lead)."
            } else {
                ""
            };
            format!(
                "{command}\nRules: {path}{team}\nStart new Claude Code sessions with this command; resumed conversations keep their old prompt."
            )
        }
        Format::Json => serde_json::to_string_pretty(
            &serde_json::json!({ "command": command, "protocol_path": path, "rules": keys, "mode": mode.key() }),
        )
        .unwrap_or_default(),
        Format::Csv => {
            let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            format!("command,protocol_path,rules,mode\n{},{},{},{}", quote(command), quote(&path), quote(&keys.join(" ")), mode.key())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marsad-rules-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    fn builtins() -> Vec<Rule> {
        builtin_library()
    }

    #[test]
    fn cli_output_puts_the_command_first() {
        use crate::report::Format;
        let lib = builtins();
        let chosen = known(&lib, &[COORDINATION]);
        let path = Path::new("/d/launch/abc.md");
        let cmd = launch_command(path);
        let table = render(Format::Table, &chosen, Mode::Session, &cmd, path);
        assert_eq!(table.lines().next(), Some("claude --append-system-prompt-file /d/launch/abc.md"));
        assert!(table.contains("Rules: /d/launch/abc.md"));
        assert!(!table.contains("Agent team"));
        assert!(render(Format::Table, &chosen, Mode::Team, &cmd, path).contains("Agent team"));
        let json: serde_json::Value = serde_json::from_str(&render(Format::Json, &chosen, Mode::Session, &cmd, path)).unwrap();
        assert_eq!(json["command"], cmd.as_str());
        assert_eq!(json["protocol_path"], "/d/launch/abc.md");
        assert_eq!(json["rules"], serde_json::json!(["coordination"]));
        assert_eq!(json["mode"], "session");
        assert_eq!(
            render(Format::Csv, &chosen, Mode::Session, &cmd, path),
            "command,protocol_path,rules,mode\n\"claude --append-system-prompt-file /d/launch/abc.md\",\"/d/launch/abc.md\",\"coordination\",session"
        );
    }

    #[test]
    fn own_prompts_follow_the_builtins_and_never_replace_them() {
        let dir = temp_dir("library");
        std::fs::write(dir.join("review-style.md"), "# Review style\n\nKeep reviews short and kind.\nMore text.\n").unwrap();
        std::fs::write(dir.join("a-plain.md"), "No heading here, just rules.\n").unwrap();
        std::fs::write(dir.join("coordination.md"), "# Mine\nshadow attempt\n").unwrap();
        std::fs::write(dir.join("all.md"), "# All\n").unwrap();
        std::fs::write(dir.join("empty.md"), "\n  \n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a prompt").unwrap();
        let lib = library_in(&dir);
        let keys: Vec<_> = lib.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["coordination", "output-hygiene", "a-plain", "review-style"]);
        assert!(!lib[0].own && lib[2].own);
        assert_eq!(lib[0].title, "Coordination", "the built-in wins a name clash");
        assert_eq!((lib[3].title.as_str(), lib[3].summary.as_str()), ("Review style", "Keep reviews short and kind."));
        assert_eq!((lib[2].title.as_str(), lib[2].summary.as_str()), ("a-plain", "No heading here, just rules."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_resolve_in_library_order() {
        let lib = builtins();
        let keys = |v: Vec<&Rule>| v.iter().map(|r| r.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(parse(&lib, &["output-hygiene", "coordination"]).unwrap()), ["coordination", "output-hygiene"]);
        assert_eq!(keys(parse(&lib, &["all"]).unwrap()).len(), 2);
        let err = parse(&lib, &["nope"]).unwrap_err().to_string();
        assert!(err.contains("coordination, output-hygiene"), "{err}");
        assert_eq!(keys(known(&lib, &["retired", "output-hygiene"])), ["output-hygiene"]);
        assert_eq!(titles(&known(&lib, &["all"])), "coordination + output hygiene");
    }

    #[test]
    fn an_empty_set_is_refused() {
        let data = temp_dir("empty");
        assert!(ensure_in(&data, &[], Mode::Session).is_err());
        assert!(ensure_in(&data, &[], Mode::Team).is_err());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_session_set_has_one_stable_file() {
        let data = temp_dir("session");
        let lib = builtins();
        let both = known(&lib, &["all"]);
        let a = ensure_in(&data, &both, Mode::Session).unwrap();
        assert_eq!(ensure_in(&data, &both, Mode::Session).unwrap(), a, "the same set reuses its file");
        assert!(a.is_absolute() && a.starts_with(launch_dir(&data)));
        let text = std::fs::read_to_string(&a).unwrap();
        let (c, h) = (text.find("# Multi-session coordination").unwrap(), text.find("# Output hygiene").unwrap());
        assert!(c < h, "merged in library order");
        assert!(!text.contains("# Agent team"));
        assert_eq!(names(&launch_dir(&data)).len(), 1, "no temp file left behind");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_team_gets_a_lead_file_pointing_at_a_teammates_file() {
        let data = temp_dir("team");
        let lib = builtins();
        let lead = ensure_in(&data, &known(&lib, &["output-hygiene"]), Mode::Team).unwrap();
        let text = std::fs::read_to_string(&lead).unwrap();
        assert!(text.starts_with("# Output hygiene") && text.contains("# Agent team"));
        assert!(!text.contains("agent board"), "no coordination, no board duties");
        let mate = names(&launch_dir(&data)).into_iter().map(|n| launch_dir(&data).join(n)).find(|p| *p != lead).unwrap();
        assert!(text.contains(&format!("read `{}`", mate.display())));
        let mate_text = std::fs::read_to_string(&mate).unwrap();
        assert!(mate_text.starts_with("# Output hygiene") && !mate_text.contains("# Agent team"), "teammates get only the prompts");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn coordination_stays_with_the_lead() {
        let data = temp_dir("team-coord");
        let lib = builtins();
        let lead = ensure_in(&data, &known(&lib, &["all"]), Mode::Team).unwrap();
        let text = std::fs::read_to_string(&lead).unwrap();
        assert!(text.contains("# Multi-session coordination") && text.contains("You represent the whole team on the board"));
        let files = names(&launch_dir(&data));
        assert_eq!(files.len(), 2);
        let mate = files.into_iter().map(|n| launch_dir(&data).join(n)).find(|p| *p != lead).unwrap();
        let mate_text = std::fs::read_to_string(&mate).unwrap();
        assert!(!mate_text.contains("coordination") && mate_text.starts_with("# Output hygiene"));

        let only = ensure_in(&data, &known(&lib, &[COORDINATION]), Mode::Team).unwrap();
        let text = std::fs::read_to_string(&only).unwrap();
        assert!(!text.contains("Before any other work, read"), "nothing to hand teammates");
        assert!(text.contains("You represent the whole team on the board"));
        assert_eq!(names(&launch_dir(&data)).len(), 3, "a coordination-only team writes just the lead's file");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn pruning_drops_unused_session_and_lead_files_only() {
        let data = temp_dir("prune");
        let lib = builtins();
        let session = ensure_in(&data, &known(&lib, &["output-hygiene"]), Mode::Session).unwrap();
        let lead = ensure_in(&data, &known(&lib, &["output-hygiene"]), Mode::Team).unwrap();
        std::fs::create_dir_all(data.join("prompts")).unwrap();
        std::fs::write(data.join("prompts").join("mine.md"), "# Mine\n").unwrap();
        std::fs::write(data.join("events.jsonl"), "{}").unwrap();
        std::fs::create_dir_all(launch_dir(&data).join("stray.md")).unwrap();

        let soon = SystemTime::now() + Duration::from_secs(60);
        assert_eq!(prune_in(&data, &data, soon).unwrap(), 0, "all recently copied");
        assert_eq!(prune_in(&data, &data, soon + KEEP_FOR).unwrap(), 2, "a week unused, the session's and lead's go");
        assert!(!session.exists() && !lead.exists());
        let left = names(&launch_dir(&data));
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.iter().any(|n| n.starts_with(TEAMMATE_PREFIX)), "a resumed lead still points at the teammates' file");
        assert!(left.contains(&"stray.md".to_string()), "a directory is skipped, not fatal");
        assert_eq!(names(&data), ["events.jsonl", "launch", "prompts"]);
        assert_eq!(names(&data.join("prompts")), ["mine.md"]);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn legacy_paths_are_kept_current_not_deleted() {
        let data = temp_dir("legacy");
        std::fs::write(data.join("coordination+output-hygiene.md"), "old").unwrap();
        prune_in(&data, &data, SystemTime::now()).unwrap();
        let lib = builtins();
        assert_eq!(std::fs::read_to_string(data.join("coordination+output-hygiene.md")).unwrap(), merge(&known(&lib, &["all"])));
        assert!(!data.join("coordination.md").exists(), "only files that exist are rewritten");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn prompt_names_that_read_as_flags_are_skipped() {
        let dir = temp_dir("flags");
        std::fs::write(dir.join("-wip.md"), "# Wip\n").unwrap();
        std::fs::write(dir.join("--team.md"), "# Team\n").unwrap();
        assert_eq!(library_in(&dir).len(), BUILTIN.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removal_deletes_only_generated_files() {
        let data = temp_dir("remove");
        let lib = builtins();
        std::fs::write(data.join("events.jsonl"), "{}").unwrap();
        std::fs::write(data.join("output-hygiene.md"), "old").unwrap();
        std::fs::create_dir_all(data.join("prompts")).unwrap();
        std::fs::write(data.join("prompts").join("mine.md"), "# Mine\n").unwrap();
        ensure_in(&data, &known(&lib, &["all"]), Mode::Session).unwrap();
        ensure_in(&data, &known(&lib, &["output-hygiene"]), Mode::Team).unwrap();

        assert_eq!(remove_all_in(&data).unwrap(), 4, "a legacy file, a session file, a lead's and a teammates' file");
        assert_eq!(names(&data), ["events.jsonl", "prompts"]);
        assert_eq!(remove_all_in(&data).unwrap(), 0, "nothing left to remove");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn quoting_follows_the_platform_shell() {
        assert_eq!(quote_for("/Users/me/.marsad/launch/session-coordination+output-hygiene-0123abcd.md", false), "/Users/me/.marsad/launch/session-coordination+output-hygiene-0123abcd.md");
        assert_eq!(quote_for("/Users/Jane Doe/x.md", false), "'/Users/Jane Doe/x.md'");
        assert_eq!(quote_for("/Users/o'neil/x.md", false), "'/Users/o'\\''neil/x.md'");
        assert_eq!(quote_for("C:/Users/me/x.md", true), "C:/Users/me/x.md");
        assert_eq!(quote_for(r"C:\Users\Jane Doe\x.md", true), r#""C:\Users\Jane Doe\x.md""#);
        assert_eq!(quote_for("", false), "''", "an empty argument is still an argument");
    }

    #[test]
    fn commands_quote_their_arguments() {
        let cmd = launch_command(Path::new("/srv/marsad/launch/a.md"));
        assert_eq!(cmd, "claude --append-system-prompt-file /srv/marsad/launch/a.md");
        assert!(!cmd.ends_with('\n'));
        let lib = builtins();
        let mine = Rule { key: "my rules".into(), title: "x".into(), summary: String::new(), body: "x".into(), own: true };
        let chosen = vec![&lib[1], &mine];
        assert_eq!(cli_command(&chosen, Mode::Team), format!("marsad rules output-hygiene {} --team", quote_arg("my rules")));
    }

    #[test]
    fn keys_are_slugs_and_clashes_are_refused() {
        assert_eq!(slug("Short PRs, please!"), "short-prs-please");
        assert_eq!(slug("  --Tone & voice--  "), "tone-voice");
        assert_eq!(slug("ছোট পিআর"), "", "no ASCII letters, no slug");
        assert_eq!(slug(&"a".repeat(60)).len(), MAX_KEY);
        let dir = temp_dir("keys");
        std::fs::write(dir.join("tone.md"), "# Tone\n").unwrap();
        let lib = library_in(&dir);
        assert_eq!(key_problem(&lib, "short-prs", None), None);
        for bad in ["-wip", "--team", "Tone", "a", "has space", "x_y", &"a".repeat(MAX_KEY + 1)] {
            assert!(key_problem(&lib, bad, None).unwrap().contains("lowercase letters"), "{bad}");
        }
        assert!(key_problem(&lib, "all", None).unwrap().contains("every prompt"));
        assert!(key_problem(&lib, "coordination", None).unwrap().contains("built-in"));
        assert!(key_problem(&lib, "tone", None).unwrap().contains("already have"));
        assert_eq!(key_problem(&lib, "tone", Some("tone")), None, "an edit keeps its own name");
        assert_eq!(suggest_key(&lib, "", "Tone"), "tone-2");
        assert_eq!(suggest_key(&lib, "Short PRs", "ignored"), "short-prs", "the preferred name wins");
        assert_eq!(suggest_key(&lib, "", "ছোট"), "my-prompt");
        assert_eq!(suggest_key(&lib, "coordination-copy", "Coordination"), "coordination-copy");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_writes_atomically_and_reads_back_into_the_library() {
        let dir = temp_dir("save");
        let lib = library_in(&dir);
        let path = save_in(&dir, &lib, "short-prs", "# Short PRs\n\nKeep them short.\n\n- Under 150 words.", None).unwrap();
        assert_eq!(path, dir.join("short-prs.md"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Short PRs\n\nKeep them short.\n\n- Under 150 words.\n");
        assert_eq!(names(&dir), ["short-prs.md"], "no temp file left behind");
        let lib = library_in(&dir);
        let mine = lib.iter().find(|r| r.key == "short-prs").unwrap();
        assert!(mine.own && mine.title == "Short PRs" && mine.summary == "Keep them short.");

        assert!(save_in(&dir, &lib, "short-prs", "# Again\n", None).is_err(), "a new prompt can't take a used name");
        save_in(&dir, &lib, "short-prs", "# Short PRs\n\nShorter.\n", Some("short-prs")).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("Shorter."), "an edit replaces the file");
        save_in(&dir, &lib, "brief-prs", "# Brief PRs\n", Some("short-prs")).unwrap();
        assert_eq!(names(&dir), ["brief-prs.md"], "a rename removes the old file");
        let lib = library_in(&dir);
        assert!(save_in(&dir, &lib, "x-copy", "  \n", None).is_err(), "an empty prompt isn't saved");
        assert!(save_in(&dir, &lib, "mine", "# Mine\n", Some("coordination")).is_err(), "a built-in is never replaced");
        assert!(save_in(&dir, &lib, "-flag", "# Flag\n", None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn coordination_is_never_copied_under_another_name() {
        let lib = builtins();
        assert!(copy_problem(&lib[0]).unwrap().contains("agent team"));
        assert_eq!(copy_problem(&lib[1]), None, "output hygiene copies fine");
    }

    #[test]
    fn only_your_own_prompts_can_be_deleted() {
        let dir = temp_dir("remove-own");
        std::fs::write(dir.join("tone.md"), "# Tone\n").unwrap();
        let lib = library_in(&dir);
        assert!(remove_in(&dir, &lib, "coordination").unwrap_err().to_string().contains("built in"));
        assert!(remove_in(&dir, &lib, "nope").unwrap_err().to_string().contains("no prompt named"));
        assert_eq!(remove_in(&dir, &lib, "tone").unwrap(), dir.join("tone.md"));
        assert!(names(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_protocol_is_project_agnostic_and_has_the_template() {
        let protocol = BUILTIN[0].body;
        for banned in ["Prisma", "package.json", "sleep 300", "BOARD ="] {
            assert!(!protocol.contains(banned), "protocol mentions {banned}");
        }
        for heading in ["## Agents", "## Claims", "## Decisions", "## Messages", "## Conflicts", "## Handoff notes"] {
            assert!(protocol.contains(heading), "protocol lacks {heading}");
        }
        assert!(protocol.contains("--git-common-dir"));
        assert!(protocol.contains("sleep 60"));
    }

    fn own(key: &str, body: &str) -> Rule {
        Rule { key: key.into(), title: key.into(), summary: String::new(), body: body.into(), own: true }
    }

    #[test]
    fn launch_files_are_named_role_keys_hash() {
        let data = temp_dir("names");
        let lib = builtins();
        let session = ensure_in(&data, &known(&lib, &["all"]), Mode::Session).unwrap();
        let name = session.file_name().unwrap().to_string_lossy().into_owned();
        let hash = name.strip_prefix("session-coordination+output-hygiene-").and_then(|r| r.strip_suffix(".md")).unwrap();
        assert!(hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()), "{name}");

        let lead = ensure_in(&data, &known(&lib, &["all"]), Mode::Team).unwrap();
        assert!(lead.file_name().unwrap().to_string_lossy().starts_with("lead-coordination+output-hygiene-"));
        let mut files = names(&launch_dir(&data));
        files.retain(|n| n.starts_with(TEAMMATE_PREFIX));
        assert_eq!(files.len(), 1);
        assert!(files[0].starts_with("teammate-output-hygiene-"), "the teammates' file names only the prompts it holds: {}", files[0]);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn launch_names_follow_the_text() {
        let (a, b) = (own("mine", "one\n"), own("mine", "two\n"));
        assert_eq!(launch_name("session", &[&a], "one\n"), launch_name("session", &[&a], "one\n"), "the same text, the same name");
        assert_ne!(launch_name("session", &[&a], "one\n"), launch_name("session", &[&b], "two\n"), "an edited prompt, a new name");
        assert_ne!(launch_name("session", &[&a], "one\n"), launch_name("lead", &[&a], "one\n"));
    }

    #[test]
    fn long_or_odd_keys_are_cut_on_a_character_boundary() {
        let long: Vec<Rule> = (0..9).map(|i| own(&format!("prompt-number-{i}"), "x")).collect();
        let refs: Vec<&Rule> = long.iter().collect();
        let name = launch_name("session", &refs, "x");
        let keys = name.strip_prefix("session-").unwrap().rsplit_once('-').unwrap().0;
        assert!(keys.chars().count() <= MAX_NAME_KEYS && keys.starts_with("prompt-number-0+prompt-number-1+"), "{name}");
        assert!(!keys.ends_with(['+', '-']), "{name}");

        let wide = own(&"é".repeat(70), "x");
        let name = launch_name("session", &[&wide], "x");
        assert!(name.starts_with(&format!("session-{}-", "é".repeat(60))), "{name}");
        let odd = own("my notes: v2?", "x");
        assert!(launch_name("session", &[&odd], "x").starts_with("session-my-notes--v2-"));
    }

    #[test]
    fn the_legacy_dir_is_kept_current_and_never_pruned() {
        let data = temp_dir("legacy-data");
        let legacy = temp_dir("legacy-old");
        std::fs::write(legacy.join("coordination+output-hygiene.md"), "old").unwrap();
        std::fs::create_dir_all(launch_dir(&legacy)).unwrap();
        let stale = launch_dir(&legacy).join("0123456789abcdef.md");
        std::fs::write(&stale, "a session file from 0.10").unwrap();
        let later = SystemTime::now() + KEEP_FOR + KEEP_FOR;
        prune_in(&data, &legacy, later).unwrap();
        let lib = builtins();
        assert_eq!(std::fs::read_to_string(legacy.join("coordination+output-hygiene.md")).unwrap(), merge(&known(&lib, &["all"])));
        assert!(stale.exists(), "nothing in the legacy dir is removed");
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&legacy);
    }
}

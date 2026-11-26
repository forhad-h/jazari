//! The skills a Claude Code session can be told to use: the ones jazari ships (Easy English) and
//! the user's own, one folder each in `<data dir>/skills/`.
//!
//! A skill is a folder with a `SKILL.md`, whose frontmatter `name` and `description` say what it is
//! and when Claude loads it, plus any scripts and data it needs. Claude Code loads skills only from
//! `~/.claude/skills/` (and a project's `.claude/skills/`), so a ticked skill is installed there
//! when its command is copied, and again at the start of every session when it is missing. jazari
//! writes `MARKER` into each folder it installs, with a hash of the files and the skill's version,
//! and updates such a folder while its files still match that hash.
//!
//! A skill's `SKILL.md` can carry a `version:` in its frontmatter. When the library's version is
//! newer than an installed copy's, the copy is updated even if it was changed by hand or installed
//! by someone else: the old folder is kept in `<data dir>/skill-backups/` first. Without a newer
//! version, jazari never overwrites a copy it didn't write or a copy changed by hand.
//!
//! Every edit, rename, restore and delete keeps the folder it replaces under
//! `.versions/<key>/<number>-<UTC time>/`, as the prompt library keeps prompt files.

use crate::rules::{Kind, Rule, Version};
use anyhow::Context;
use std::path::{Path, PathBuf};

/// One file of a skill: its path inside the folder, with `/` between parts, and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: String,
    pub bytes: Vec<u8>,
}

impl File {
    pub fn new(path: &str, bytes: impl Into<Vec<u8>>) -> Self {
        File { path: path.into(), bytes: bytes.into() }
    }

    /// The file as text, or None for binary data.
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok().filter(|t| !t.contains('\0'))
    }
}

/// One skill: its key (the folder's name), what its `SKILL.md` says about it, and its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub key: String,
    pub name: String,
    pub description: String,
    pub own: bool,
    /// The frontmatter's `version`, when it has one.
    pub version: Option<String>,
    /// Sorted by path; `SKILL.md` is one of them.
    pub files: Vec<File>,
}

impl Skill {
    pub fn skill_md(&self) -> &str {
        self.files.iter().find(|f| f.path == SKILL_MD).and_then(File::text).unwrap_or("")
    }
}

pub const SKILL_MD: &str = "SKILL.md";

/// The file jazari writes into each skill folder it installs in Claude Code's skills folder.
pub const MARKER: &str = ".jazari-skill.json";

/// Files a system adds to a folder by itself, which are no change to a skill.
const IGNORED: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// Overrides Claude Code's skills folder, for a scratch folder during development.
const SKILLS_ENV: &str = "JAZARI_CLAUDE_SKILLS";

struct Builtin {
    key: &'static str,
    files: &'static [(&'static str, &'static [u8])],
}

/// The skills jazari ships. Add new ones at the end.
const BUILTIN: &[Builtin] = &[Builtin {
    key: "easy-english",
    files: &[
        ("SKILL.md", include_bytes!("../assets/skills/easy-english/SKILL.md")),
        ("data/SOURCES.md", include_bytes!("../assets/skills/easy-english/data/SOURCES.md")),
        ("data/cefrj-vocabulary.csv", include_bytes!("../assets/skills/easy-english/data/cefrj-vocabulary.csv")),
        ("data/ngsl-forms.csv", include_bytes!("../assets/skills/easy-english/data/ngsl-forms.csv")),
        ("data/tech-words.txt", include_bytes!("../assets/skills/easy-english/data/tech-words.txt")),
        ("scripts/check.py", include_bytes!("../assets/skills/easy-english/scripts/check.py")),
    ],
}];

/// Where the user's own skills live: one folder each.
pub fn skills_dir() -> PathBuf {
    crate::data_dir().join("skills")
}

/// Claude Code's personal skills folder, `~/.claude/skills`, or `JAZARI_CLAUDE_SKILLS`.
pub fn claude_skills_dir() -> PathBuf {
    std::env::var_os(SKILLS_ENV).filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| crate::claude_dir().join("skills"))
}

/// The frontmatter's `name` and `description` in a `SKILL.md`; empty when missing.
pub fn front(text: &str) -> (String, String) {
    let (mut name, mut description) = (String::new(), String::new());
    let Some(block) = frontmatter(text) else { return (name, description) };
    let lines: Vec<&str> = block.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let Some((field, value)) = line.split_once(':') else { continue };
        if field.starts_with(char::is_whitespace) {
            continue;
        }
        let value = value.trim();
        // A folded (`>`) or literal (`|`) value goes on in the indented lines after it.
        let value = if value.starts_with(['>', '|']) {
            let mut parts = Vec::new();
            while i < lines.len() && (lines[i].starts_with(char::is_whitespace) || lines[i].trim().is_empty()) {
                parts.push(lines[i].trim());
                i += 1;
            }
            parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
        } else {
            unquote(value)
        };
        match field.trim() {
            "name" => name = value,
            "description" => description = value,
            _ => {}
        }
    }
    (name, description)
}

/// The frontmatter's top-level `version`, when it has a non-empty one.
pub fn version_of(text: &str) -> Option<String> {
    frontmatter(text)?
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(field, _)| *field == "version")
        .map(|(_, value)| unquote(value.trim()))
        .filter(|v| !v.is_empty())
}

/// Whether version `a` is newer than `b`, comparing dot-separated numbers (`1.10` > `1.9`). A
/// missing version is older than any version; a part that is not a number counts as 0.
pub fn newer(a: Option<&str>, b: Option<&str>) -> bool {
    let parts = |v: &str| v.trim().trim_start_matches('v').split('.').map(|p| p.trim().parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    match (a, b) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(a), Some(b)) => {
            let (mut a, mut b) = (parts(a), parts(b));
            let len = a.len().max(b.len());
            a.resize(len, 0);
            b.resize(len, 0);
            a > b
        }
    }
}

/// The text between a leading `---` line and the next one.
fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?.strip_prefix('\n').or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---").map(|i| i + 1).or_else(|| rest.starts_with("---").then_some(0))?;
    Some(&rest[..end])
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\")
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value.to_string()
    }
}

/// `text` with its frontmatter `name` set to `key`, adding a frontmatter when there is none.
pub fn with_name(text: &str, key: &str) -> String {
    let Some(block) = frontmatter(text) else {
        return format!("---\nname: {key}\ndescription: \"\"\n---\n\n{}", text.trim_start());
    };
    let mut found = false;
    let lines: Vec<String> = block
        .lines()
        .map(|l| {
            if !found && l.starts_with("name:") {
                found = true;
                format!("name: {key}")
            } else {
                l.to_string()
            }
        })
        .collect();
    let mut new_block = lines.join("\n");
    if !found {
        new_block = format!("name: {key}\n{new_block}");
    }
    let start = text.find(block).unwrap_or(0);
    format!("{}{}\n{}", &text[..start], new_block.trim_end_matches('\n'), &text[start + block.len()..])
}

/// The files with every `skills/<from>/` path in their text pointing at `skills/<to>/`, and the
/// `SKILL.md` named `to`: what a copy or a rename needs, so its scripts are run from its own folder.
pub fn renamed(files: &[File], from: Option<&str>, to: &str) -> Vec<File> {
    files
        .iter()
        .map(|f| {
            let Some(text) = f.text() else { return f.clone() };
            let mut text = match from.filter(|from| *from != to) {
                Some(from) => text.replace(&format!("skills/{from}/"), &format!("skills/{to}/")),
                None => text.to_string(),
            };
            if f.path == SKILL_MD {
                text = with_name(&text, to);
            }
            File::new(&f.path, text)
        })
        .collect()
}

fn builtin(b: &Builtin) -> Skill {
    let files: Vec<File> = b.files.iter().map(|(p, bytes)| File::new(p, bytes.to_vec())).collect();
    skill_of(b.key.to_string(), files, false)
}

fn skill_of(key: String, mut files: Vec<File>, own: bool) -> Skill {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let text = files.iter().find(|f| f.path == SKILL_MD).and_then(File::text).unwrap_or("");
    let (name, description) = front(text);
    let version = version_of(text);
    Skill { key, name, description, own, version, files }
}

/// The built-in skills, then the user's own in folder-name order. An own skill named like a
/// built-in one, `all`, or starting with `-` or `.` is skipped.
pub fn library() -> Vec<Skill> {
    library_in(&skills_dir())
}

pub(crate) fn library_in(dir: &Path) -> Vec<Skill> {
    let mut skills: Vec<Skill> = BUILTIN.iter().map(builtin).collect();
    let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join(SKILL_MD).is_file()).collect())
        .unwrap_or_default();
    folders.sort();
    for path in folders {
        let Some(key) = path.file_name().map(|s| s.to_string_lossy().into_owned()) else { continue };
        if key == "all" || key.starts_with(['-', '.']) || skills.iter().any(|s| s.key == key) {
            continue;
        }
        if let Ok(files) = read_folder(&path) {
            skills.push(skill_of(key, files, true));
        }
    }
    skills
}

/// One skill by key, built-in or the user's own.
pub fn find(key: &str) -> Option<Skill> {
    library().into_iter().find(|s| s.key == key)
}

/// The skills as library entries for the picker and `jazari build rules`: the body is the `SKILL.md`.
pub fn rules(skills: &[Skill]) -> Vec<Rule> {
    skills
        .iter()
        .map(|s| {
            let title = if s.name.trim().is_empty() { s.key.clone() } else { s.name.trim().to_string() };
            Rule::new(s.key.clone(), title, first_sentence(&s.description), s.skill_md().to_string(), s.own, Kind::Skill)
        })
        .collect()
}

/// The first sentence of a description: what the skill does, in full, for a list. The rest says
/// when Claude loads it.
pub fn first_sentence(description: &str) -> String {
    let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = text.char_indices().find(|&(i, c)| matches!(c, '.' | '!' | '?') && text[i + 1..].starts_with(' ')).map(|(i, _)| i + 1);
    end.map_or(text.clone(), |i| text[..i].to_string())
}

/// The section of a launch file that makes a session use `chosen` skills.
pub fn launch_section(chosen: &[&Rule]) -> String {
    let mut out = String::from(
        "# Skills\n\
         \n\
         This session must use these skills. Load each one with the Skill tool when you reach a step that its \
         description names, not earlier (not at the start of a task that has no such step), and follow it from \
         then on:\n\n",
    );
    for rule in chosen {
        let (_, description) = front(rule.body());
        let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!("- **{}**: {}\n", rule.key, if description.is_empty() { rule.summary.as_str() } else { &description }));
    }
    out.push_str(
        "\nIf one of these skills is not in your list of available skills when you need it, run `jazari build skill sync` \
         once. It installs the missing skill, and Claude Code makes it available a moment later.\n",
    );
    out
}

/// Every file under `dir`, with paths relative to it. Hidden files and folders, and the files a
/// system adds by itself, are left out: a skill's own files never start with a dot.
fn read_folder(dir: &Path) -> anyhow::Result<Vec<File>> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files, false)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Every file under `dir` but `MARKER` and system files, hidden ones too: what an installed copy
/// holds, to tell whether someone changed it.
fn read_installed(dir: &Path) -> anyhow::Result<Vec<File>> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files, true)?;
    files.retain(|f| f.path != MARKER);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<File>, hidden: bool) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if IGNORED.contains(&name.as_str()) || (!hidden && name.starts_with('.')) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &path, out, hidden)?;
        } else if kind.is_file() || path.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path).components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            out.push(File { path: rel, bytes: std::fs::read(&path).with_context(|| format!("reading {}", path.display()))? });
        }
    }
    Ok(())
}

/// A path inside a skill folder that stays inside it: relative, with no `..` and no empty parts.
pub fn safe_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.contains('\\') && !path.contains(':') && path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Replaces the folder `target` with `files` in one step: they are written to a hidden folder
/// beside it, which then takes its place, so Claude Code never loads half a skill.
fn write_folder(target: &Path, files: &[File], marker: Option<&str>) -> anyhow::Result<()> {
    let parent = target.parent().context("skill folder has no parent")?;
    let name = target.file_name().context("skill folder has no name")?.to_string_lossy().into_owned();
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let pid = std::process::id();
    let (tmp, old) = (parent.join(format!(".{name}.tmp-{pid}")), parent.join(format!(".{name}.old-{pid}")));
    let _ = std::fs::remove_dir_all(&tmp);
    let written = (|| -> anyhow::Result<()> {
        for f in files {
            anyhow::ensure!(safe_path(&f.path), "{:?} is not a path inside the skill's folder", f.path);
            let path = tmp.join(&f.path);
            std::fs::create_dir_all(path.parent().unwrap_or(&tmp))?;
            std::fs::write(&path, &f.bytes).with_context(|| format!("writing {}", path.display()))?;
        }
        if let Some(marker) = marker {
            std::fs::write(tmp.join(MARKER), marker)?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&old);
    let had = target.exists();
    if had {
        std::fs::rename(target, &old).with_context(|| format!("moving {} aside", target.display()))?;
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        if had {
            let _ = std::fs::rename(&old, target);
        }
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e).with_context(|| format!("replacing {}", target.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// A stable hash of a skill's files, paths and bytes, as 16 hex digits.
pub fn hash(files: &[File]) -> String {
    let mut sorted: Vec<&File> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut bytes = Vec::new();
    for f in sorted {
        bytes.extend(f.path.as_bytes());
        bytes.push(0);
        bytes.extend((f.bytes.len() as u64).to_le_bytes());
        bytes.extend(&f.bytes);
    }
    format!("{:016x}", crate::rules::fnv1a(&bytes))
}

// ---------- saving, versions, deleting ----------

/// The files a draft becomes: `base` with the draft's files written over it, `removed` taken out,
/// and `skill_md` as the `SKILL.md`.
pub fn apply(base: &[File], skill_md: &str, changed: &[File], removed: &[String]) -> Vec<File> {
    let mut files: Vec<File> = base.iter().filter(|f| f.path != SKILL_MD && !removed.contains(&f.path)).cloned().collect();
    for f in changed.iter().filter(|f| f.path != SKILL_MD) {
        files.retain(|old| old.path != f.path);
        files.push(f.clone());
    }
    files.push(File::new(SKILL_MD, format!("{}\n", skill_md.trim_end())));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// Saves `files` as the user's skill `key`, replacing its folder in one step. When `replacing` names
/// another of the user's skills (a rename), that folder is removed once the new one is written.
/// The folder being replaced is kept as a version first. Returns the new folder's path.
pub fn save_own(key: &str, files: &[File], replacing: Option<&str>) -> anyhow::Result<PathBuf> {
    save_in(&skills_dir(), &crate::rules::library(), key, files, replacing)
}

fn save_in(dir: &Path, lib: &[Rule], key: &str, files: &[File], replacing: Option<&str>) -> anyhow::Result<PathBuf> {
    if let Some(why) = crate::rules::key_problem(lib, key, replacing) {
        anyhow::bail!("can't save as {key:?}: {why}");
    }
    if let Some(old) = replacing {
        anyhow::ensure!(lib.iter().any(|r| r.key == old && r.own && r.kind == Kind::Skill), "{old} is not one of your skills");
    }
    anyhow::ensure!(files.iter().any(|f| f.path == SKILL_MD && f.text().is_some_and(|t| !t.trim().is_empty())), "the skill has no SKILL.md");
    let target = dir.join(key);
    if let Some(old) = replacing {
        if old != key {
            move_versions(dir, old, key)?;
        }
        if let Ok(current) = read_folder(&dir.join(old))
            && hash(&current) != hash(files)
        {
            keep_version(dir, key, &current)?;
        }
    }
    write_folder(&target, files, None)?;
    if let Some(old) = replacing.filter(|old| *old != key) {
        let old = dir.join(old);
        std::fs::remove_dir_all(&old).with_context(|| format!("removing {}", old.display()))?;
    }
    Ok(target)
}

/// Saves a reviewed draft as the user's skill `key`: the library copy it revises (`replacing`) or
/// copies (`copy_of`) with the draft's changes, its paths and name moved to `key`.
pub fn save_draft(key: &str, draft: &crate::prompt_gen::Draft, replacing: Option<&str>, copy_of: Option<&str>) -> anyhow::Result<PathBuf> {
    let base_key = replacing.or(copy_of);
    let base = base_key.and_then(find).map(|s| s.files).unwrap_or_default();
    let changed: Vec<File> = draft.files.iter().map(|f| File::new(&f.path, f.content.clone())).collect();
    let files = renamed(&apply(&base, &draft.body, &changed, &draft.removed), base_key, key);
    save_own(key, &files, replacing)
}

/// How many earlier versions are kept per skill; older ones are dropped. Lower than for prompts,
/// because a skill folder can hold data files.
const MAX_VERSIONS: usize = 20;

fn versions_dir(dir: &Path, key: &str) -> PathBuf {
    dir.join(".versions").join(key)
}

/// The version folders of `key`, oldest first: (number, path, when it was replaced).
fn version_folders(dir: &Path, key: &str) -> Vec<(u32, PathBuf, Option<chrono::DateTime<chrono::Utc>>)> {
    let mut list: Vec<_> = std::fs::read_dir(versions_dir(dir, key))
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let (number, stamp) = name.split_once('-').unwrap_or((&name, ""));
            let when = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%SZ").ok().map(|t| t.and_utc());
            Some((number.parse().ok()?, p, when))
        })
        .collect();
    list.sort_by_key(|v| v.0);
    list
}

fn keep_version(dir: &Path, key: &str, files: &[File]) -> anyhow::Result<()> {
    let list = version_folders(dir, key);
    if list.last().is_some_and(|(_, p, _)| read_folder(p).is_ok_and(|old| hash(&old) == hash(files))) {
        return Ok(());
    }
    let number = list.last().map_or(1, |v| v.0 + 1);
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    write_folder(&versions_dir(dir, key).join(format!("{number:04}-{stamp}")), files, None)?;
    for (_, old, _) in list.iter().take((list.len() + 1).saturating_sub(MAX_VERSIONS)) {
        let _ = std::fs::remove_dir_all(old);
    }
    Ok(())
}

fn move_versions(dir: &Path, from: &str, to: &str) -> anyhow::Result<()> {
    let list = version_folders(dir, from);
    if list.is_empty() {
        return Ok(());
    }
    let target = versions_dir(dir, to);
    std::fs::create_dir_all(&target).with_context(|| format!("creating {}", target.display()))?;
    let first = version_folders(dir, to).last().map_or(1, |v| v.0 + 1);
    for (next, (_, path, _)) in (first..).zip(list) {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let stamp = name.split_once('-').map_or("", |(_, rest)| rest);
        let dest = target.join(format!("{next:04}-{stamp}"));
        std::fs::rename(&path, &dest).with_context(|| format!("moving {}", path.display()))?;
    }
    let _ = std::fs::remove_dir(versions_dir(dir, from));
    Ok(())
}

/// The earlier versions of the user's skill `key`, newest first. A version's body is its `SKILL.md`.
pub fn versions(key: &str) -> Vec<Version> {
    versions_in(&skills_dir(), key)
}

fn versions_in(dir: &Path, key: &str) -> Vec<Version> {
    let mut list: Vec<Version> = version_folders(dir, key)
        .into_iter()
        .filter_map(|(number, path, replaced)| {
            let body = std::fs::read_to_string(path.join(SKILL_MD)).ok()?;
            let (name, description) = front(&body);
            let title = if name.trim().is_empty() { key.to_string() } else { name };
            Some(Version { number, replaced, title, summary: crate::rules::shorten(description.trim(), 80), body })
        })
        .collect();
    list.reverse();
    list
}

/// The keys of deleted skills that still have versions to restore.
pub fn deleted() -> Vec<String> {
    deleted_in(&skills_dir())
}

fn deleted_in(dir: &Path) -> Vec<String> {
    let mut keys: Vec<String> = std::fs::read_dir(dir.join(".versions"))
        .map(|entries| entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    keys.retain(|k| !dir.join(k).join(SKILL_MD).exists() && !version_folders(dir, k).is_empty());
    keys.sort();
    keys
}

/// Makes version `number` of `key` (the newest when None) the skill again, bringing back a deleted
/// one. The folder it replaces is kept as a version. Returns the version restored.
pub fn restore_own(key: &str, number: Option<u32>) -> anyhow::Result<Version> {
    restore_in(&skills_dir(), &crate::rules::library(), key, number)
}

fn restore_in(dir: &Path, lib: &[Rule], key: &str, number: Option<u32>) -> anyhow::Result<Version> {
    if lib.iter().any(|r| r.key == key && !r.own) {
        anyhow::bail!("{key} is built in and has no versions");
    }
    let list = version_folders(dir, key);
    anyhow::ensure!(!list.is_empty(), "{key} has no earlier versions");
    let (n, path, _) = match number {
        None => list.last().cloned(),
        Some(n) => list.iter().find(|v| v.0 == n).cloned(),
    }
    .ok_or_else(|| anyhow::anyhow!("{key} has no version {}", number.unwrap_or_default()))?;
    let files = read_folder(&path)?;
    let own = lib.iter().any(|r| r.key == key && r.own && r.kind == Kind::Skill);
    save_in(dir, lib, key, &files, own.then_some(key))?;
    // The restored version stays in the list; the folder it replaced is kept after it.
    versions_in(dir, key).into_iter().find(|v| v.number == n).with_context(|| format!("{key} has no version {n}"))
}

/// Deletes the user's skill `key`, keeping its folder as a version so it can be restored. A copy
/// installed in Claude Code's skills folder is left where it is. Returns the removed path.
pub fn remove_own(key: &str) -> anyhow::Result<PathBuf> {
    remove_in(&skills_dir(), &crate::rules::library(), key)
}

fn remove_in(dir: &Path, lib: &[Rule], key: &str) -> anyhow::Result<PathBuf> {
    match lib.iter().find(|r| r.key == key && r.kind == Kind::Skill) {
        None => anyhow::bail!("no skill named {key:?}"),
        Some(r) if !r.own => anyhow::bail!("{key} is built in and can't be deleted"),
        Some(_) => {
            let path = dir.join(key);
            if let Ok(files) = read_folder(&path) {
                keep_version(dir, key, &files)?;
            }
            std::fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
            Ok(path)
        }
    }
}

// ---------- installing into Claude Code's skills folder ----------

/// What happened to a skill in Claude Code's skills folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// It wasn't there, so jazari's library copy was written.
    New,
    /// jazari wrote it earlier and nobody changed it since; it now holds the library's version.
    Updated,
    /// jazari wrote it earlier, nobody changed it, and it is the library's version.
    Current,
    /// A copy jazari didn't write, used as it is.
    Theirs,
    /// jazari wrote it, but its files were changed since, so it is used as it is.
    Changed,
    /// A copy changed by hand or not written by jazari, with an older version: it was kept in the
    /// backup folder named here and replaced by the library's version.
    Replaced(PathBuf),
}

/// One skill's place in Claude Code's skills folder and what happened there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub key: String,
    pub path: PathBuf,
    pub outcome: Installed,
}

impl Report {
    /// A short sentence for the footer or the terminal.
    pub fn message(&self) -> String {
        let at = home_relative(&self.path);
        match &self.outcome {
            Installed::New => format!("{}: installed to {at}", self.key),
            Installed::Updated => format!("{}: updated in {at}", self.key),
            Installed::Current => format!("{}: {at} is up to date", self.key),
            Installed::Theirs => format!("{}: your own copy in {at} is used as it is", self.key),
            Installed::Changed => format!("{}: {at} was changed by hand, so it is used as it is and not updated", self.key),
            Installed::Replaced(backup) => format!("{}: {at} had an older version; updated, and the old copy is kept in {}", self.key, home_relative(backup)),
        }
    }
}

/// `path` with the home folder shown as `~`.
pub fn home_relative(path: &Path) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    path.strip_prefix(&home).map(|rest| format!("~/{}", rest.display())).unwrap_or_else(|_| path.display().to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Marker {
    installed_by: String,
    key: String,
    /// The hash of the files jazari wrote.
    hash: String,
    #[serde(default)]
    jazari: String,
    /// The skill's own version (its frontmatter `version`), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

fn marker_text(skill: &Skill) -> String {
    let m = Marker {
        installed_by: "jazari".into(),
        key: skill.key.clone(),
        hash: hash(&skill.files),
        jazari: env!("CARGO_PKG_VERSION").into(),
        version: skill.version.clone(),
    };
    serde_json::to_string_pretty(&m).unwrap_or_default() + "\n"
}

/// Where a copy is kept before jazari replaces it with a newer version.
fn backups_dir() -> PathBuf {
    crate::data_dir().join("skill-backups")
}

/// The version of the copy installed at `path`: its `SKILL.md`'s, else the one jazari recorded.
fn installed_version(path: &Path, marker: Option<&Marker>) -> Option<String> {
    std::fs::read_to_string(path.join(SKILL_MD)).ok().and_then(|t| version_of(&t)).or_else(|| marker.and_then(|m| m.version.clone()))
}

/// Makes sure Claude Code can load `skill`: installs it into `root` when it is missing, updates a
/// copy jazari installed that nobody changed since, and replaces any other copy only when the
/// library's version is newer than the copy's (keeping the copy in the backup folder first).
pub fn provide_in(root: &Path, skill: &Skill) -> anyhow::Result<Report> {
    provide_at(root, skill, &backups_dir())
}

fn provide_at(root: &Path, skill: &Skill, backups: &Path) -> anyhow::Result<Report> {
    let path = root.join(&skill.key);
    let report = |outcome| Ok(Report { key: skill.key.clone(), path: path.clone(), outcome });
    if !path.join(SKILL_MD).exists() {
        write_folder(&path, &skill.files, Some(&marker_text(skill)))?;
        return report(Installed::New);
    }
    let marker = std::fs::read_to_string(path.join(MARKER)).ok().and_then(|t| serde_json::from_str::<Marker>(&t).ok());
    let current = read_installed(&path)?;
    let untouched = marker.as_ref().is_some_and(|m| hash(&current) == m.hash);
    if untouched {
        if hash(&current) == hash(&skill.files) {
            return report(Installed::Current);
        }
        write_folder(&path, &skill.files, Some(&marker_text(skill)))?;
        return report(Installed::Updated);
    }
    if newer(skill.version.as_deref(), installed_version(&path, marker.as_ref()).as_deref()) {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let backup = backups.join(&skill.key).join(format!("{stamp}-{}", std::process::id()));
        write_folder(&backup, &current, None).with_context(|| format!("keeping {} before replacing it", path.display()))?;
        write_folder(&path, &skill.files, Some(&marker_text(skill)))?;
        return report(Installed::Replaced(backup));
    }
    report(if marker.is_some() { Installed::Changed } else { Installed::Theirs })
}

/// `provide_in` Claude Code's skills folder for each skill among `chosen`.
pub fn provide(chosen: &[&Rule]) -> anyhow::Result<Vec<Report>> {
    let skills = library();
    let root = claude_skills_dir();
    chosen
        .iter()
        .filter(|r| r.kind == Kind::Skill)
        .filter_map(|r| skills.iter().find(|s| s.key == r.key))
        .map(|s| provide_in(&root, s).with_context(|| format!("installing the skill {}", s.key)))
        .collect()
}

/// At the start of a session in `cwd`: brings every installed copy of a library skill up to date
/// (in `~/.claude/skills` and in each `.claude/skills` from `cwd` up to the home folder), and
/// installs each skill in `ticked` into `~/.claude/skills` when no copy is found anywhere.
/// Returns what was done; copies left as they were are not in the list.
pub fn sync_on_start(cwd: Option<&Path>, ticked: &[String]) -> Vec<anyhow::Result<Report>> {
    sync_roots(&claude_skills_dir(), &project_roots(cwd), &library(), ticked, &backups_dir())
}

/// Each `.claude/skills` folder in `cwd` and the folders above it, stopping before the home folder,
/// whose `.claude/skills` is the personal one.
fn project_roots(cwd: Option<&Path>) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let Some(cwd) = cwd else { return Vec::new() };
    cwd.ancestors()
        .take_while(|dir| home.as_deref() != Some(*dir) && dir.parent().is_some())
        .map(|dir| dir.join(".claude").join("skills"))
        .filter(|p| p.is_dir())
        .collect()
}

fn sync_roots(personal: &Path, projects: &[PathBuf], skills: &[Skill], ticked: &[String], backups: &Path) -> Vec<anyhow::Result<Report>> {
    let mut out = Vec::new();
    for skill in skills {
        let mut found = false;
        for root in std::iter::once(personal).chain(projects.iter().map(PathBuf::as_path)) {
            if root.join(&skill.key).join(SKILL_MD).exists() {
                found = true;
                out.push(provide_at(root, skill, backups).with_context(|| format!("updating the skill {}", skill.key)));
            }
        }
        if !found && ticked.contains(&skill.key) {
            out.push(provide_at(personal, skill, backups).with_context(|| format!("installing the skill {}", skill.key)));
        }
    }
    out.retain(|r| !matches!(r, Ok(Report { outcome: Installed::Current | Installed::Theirs | Installed::Changed, .. })));
    out
}

/// After an edit in the library: updates the installed copy of `key` only when jazari installed it
/// and nobody changed it. None when no copy is installed.
pub fn sync_installed(key: &str) -> Option<anyhow::Result<Report>> {
    sync_in(&claude_skills_dir(), find(key).as_ref()?)
}

fn sync_in(root: &Path, skill: &Skill) -> Option<anyhow::Result<Report>> {
    root.join(&skill.key).join(SKILL_MD).exists().then(|| provide_in(root, skill))
}

/// The library copy's text files, for Claude to revise: each one under `MAX_SENT` bytes. The rest
/// are named, so Claude knows they exist and leaves them as they are.
pub fn for_revision(files: &[File]) -> (Vec<File>, Vec<String>) {
    let (mut sent, mut named) = (Vec::new(), Vec::new());
    for f in files.iter().filter(|f| f.path != SKILL_MD) {
        if f.text().is_some() && f.bytes.len() <= MAX_SENT {
            sent.push(f.clone());
        } else {
            named.push(f.path.clone());
        }
    }
    (sent, named)
}

/// The largest file sent to Claude when a skill is revised.
const MAX_SENT: usize = 48 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jazari-skills-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn skill_md(name: &str, description: &str) -> String {
        format!("---\nname: {name}\ndescription: \"{description}\"\n---\n\n# {name}\n\nRun `python3 ~/.claude/skills/{name}/scripts/x.py`.\n")
    }

    fn files(name: &str) -> Vec<File> {
        vec![File::new(SKILL_MD, skill_md(name, "Use it before you write a commit message.")), File::new("scripts/x.py", "print(1)\n")]
    }

    fn lib_with(dir: &Path) -> Vec<Rule> {
        let mut lib = crate::rules::builtin_rules();
        lib.extend(rules(&library_in(dir)));
        lib
    }

    #[test]
    fn frontmatter_is_read_quoted_plain_and_folded() {
        assert_eq!(front(&skill_md("a", "say \\\"hi\\\"")), ("a".into(), "say \"hi\"".into()));
        assert_eq!(front("---\nname: b\ndescription: plain words\n---\n"), ("b".into(), "plain words".into()));
        assert_eq!(front("---\nname: c\ndescription: >\n  one\n  two\nother: x\n---\nbody"), ("c".into(), "one two".into()));
        assert_eq!(front("no frontmatter"), (String::new(), String::new()));
    }

    #[test]
    fn the_builtin_skill_reads_back_with_its_description() {
        let lib = library_in(Path::new("/nonexistent"));
        let ee = lib.iter().find(|s| s.key == "easy-english").unwrap();
        assert_eq!(ee.name, "easy-english");
        assert!(ee.description.contains("before git commit"), "{}", ee.description);
        assert!(!ee.own && ee.files.iter().any(|f| f.path == "data/ngsl-forms.csv"));
        assert!(!ee.skill_md().contains("Bengali"), "written for every reader");
        let section = launch_section(&rules(&lib).iter().collect::<Vec<_>>());
        assert!(section.contains("must use these skills") && section.contains("- **easy-english**: Write technical English"));
    }

    #[test]
    fn a_list_shows_the_first_sentence_in_full() {
        assert_eq!(first_sentence("Does one thing. Load it  before you commit."), "Does one thing.");
        assert_eq!(first_sentence("No full stop here"), "No full stop here");
        assert_eq!(first_sentence("Uses v1.2 files. Then more."), "Uses v1.2 files.");
        let lib = rules(&library_in(Path::new("/nonexistent")));
        assert!(lib[0].summary.ends_with("whatever their first language."), "{}", lib[0].summary);
    }

    #[test]
    fn a_copy_or_rename_points_its_paths_and_name_at_the_new_key() {
        let out = renamed(&files("plain"), Some("plain"), "plainer");
        let md = out[0].text().unwrap();
        assert!(md.contains("name: plainer\n") && md.contains("skills/plainer/scripts"), "{md}");
        assert_eq!(front(md).1, "Use it before you write a commit message.");
        assert_eq!(with_name("body only", "k"), "---\nname: k\ndescription: \"\"\n---\n\nbody only");
    }

    #[test]
    fn own_skills_save_version_rename_delete_and_restore() {
        let dir = temp_dir("own");
        let first = files("mine");
        save_in(&dir, &lib_with(&dir), "mine", &first, None).unwrap();
        assert!(dir.join("mine/scripts/x.py").is_file());
        assert!(save_in(&dir, &lib_with(&dir), "easy-english", &first, None).is_err(), "a built-in name is taken");
        assert!(save_in(&dir, &lib_with(&dir), "coordination", &first, None).is_err(), "so is a prompt's");
        let second = apply(&first, &skill_md("mine", "Use it before you write docs."), &[File::new("data/w.txt", "w")], &["scripts/x.py".into()]);
        save_in(&dir, &lib_with(&dir), "mine", &second, Some("mine")).unwrap();
        assert!(!dir.join("mine/scripts/x.py").exists() && dir.join("mine/data/w.txt").is_file());
        let v = versions_in(&dir, "mine");
        assert_eq!(v.len(), 1);
        assert!(v[0].summary.contains("commit message"), "the version is the whole folder before the edit");
        save_in(&dir, &lib_with(&dir), "ours", &renamed(&second, Some("mine"), "ours"), Some("mine")).unwrap();
        assert!(!dir.join("mine").exists() && versions_in(&dir, "ours").len() == 2, "a rename takes the history along");
        remove_in(&dir, &lib_with(&dir), "ours").unwrap();
        assert_eq!(deleted_in(&dir), ["ours"]);
        assert!(remove_in(&dir, &lib_with(&dir), "easy-english").is_err());
        restore_in(&dir, &lib_with(&dir), "ours", None).unwrap();
        assert!(dir.join("ours/SKILL.md").is_file() && deleted_in(&dir).is_empty());
        restore_in(&dir, &lib_with(&dir), "ours", Some(1)).unwrap();
        assert!(dir.join("ours/scripts/x.py").is_file(), "version 1 had the script");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsafe_paths_are_refused() {
        for bad in ["../x", "/etc/x", "a/../../b", "", "a//b", "C:/x", "a\\b"] {
            assert!(!safe_path(bad), "{bad}");
        }
        assert!(safe_path("scripts/check.py"));
        let dir = temp_dir("unsafe");
        assert!(write_folder(&dir.join("s"), &[File::new("../escape", "x")], None).is_err());
        assert!(!dir.join("escape").exists() && !dir.join("s").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_never_overwrites_a_copy_jazari_did_not_write() {
        let root = temp_dir("install");
        let skill = skill_of("mine".into(), files("mine"), true);
        let ours = provide_in(&root, &skill).unwrap();
        assert_eq!(ours.outcome, Installed::New);
        assert!(root.join("mine").join(MARKER).is_file() && root.join("mine/scripts/x.py").is_file());
        assert_eq!(provide_in(&root, &skill).unwrap().outcome, Installed::Current);
        std::fs::write(root.join("mine/.DS_Store"), "x").unwrap();
        let newer = skill_of("mine".into(), apply(&skill.files, &skill_md("mine", "newer"), &[], &[]), true);
        assert_eq!(provide_in(&root, &newer).unwrap().outcome, Installed::Updated, "an untouched copy follows the library");
        assert!(std::fs::read_to_string(root.join("mine/SKILL.md")).unwrap().contains("newer"));
        std::fs::write(root.join("mine/SKILL.md"), "edited by hand").unwrap();
        assert_eq!(provide_in(&root, &skill).unwrap().outcome, Installed::Changed);
        assert_eq!(std::fs::read_to_string(root.join("mine/SKILL.md")).unwrap(), "edited by hand");
        // A copy with no marker is the user's: never touched.
        std::fs::create_dir_all(root.join("theirs")).unwrap();
        std::fs::write(root.join("theirs/SKILL.md"), "mine").unwrap();
        let theirs = skill_of("theirs".into(), files("theirs"), false);
        assert_eq!(provide_in(&root, &theirs).unwrap().outcome, Installed::Theirs);
        assert_eq!(std::fs::read_to_string(root.join("theirs/SKILL.md")).unwrap(), "mine");
        assert!(!root.join("theirs").join(MARKER).exists());
        assert!(sync_in(&root, &skill_of("absent".into(), files("absent"), true)).is_none(), "an edit installs nothing new");
        assert!(Report { key: "k".into(), path: root.join("k"), outcome: Installed::Theirs }.message().contains("used as it is"));
        let _ = std::fs::remove_dir_all(&root);
    }

    fn versioned(name: &str, version: &str) -> Skill {
        let md = format!("---\nname: {name}\ndescription: \"d\"\nversion: {version}\n---\n\nbody {version}\n");
        skill_of(name.into(), vec![File::new(SKILL_MD, md)], false)
    }

    #[test]
    fn versions_are_read_and_compared_by_number() {
        assert_eq!(version_of("---\nname: a\nversion: \"1.2.0\"\n---\n"), Some("1.2.0".into()));
        assert_eq!(version_of("---\nname: a\n---\nversion: 9\n"), None, "only the frontmatter counts");
        assert!(newer(Some("1.10"), Some("1.9")) && newer(Some("1.1.0"), None) && newer(Some("v2"), Some("1.9.9")));
        assert!(!newer(Some("1.0"), Some("1.0.0")) && !newer(None, Some("1")) && !newer(None, None));
        let ee = &library_in(Path::new("/nonexistent"))[0];
        assert!(ee.version.is_some(), "the built-in skill carries a version");
    }

    #[test]
    fn a_newer_version_replaces_a_changed_or_foreign_copy_and_keeps_it() {
        let (root, backups) = (temp_dir("ver-root"), temp_dir("ver-backups"));
        let old = versioned("s", "1.0.0");
        provide_at(&root, &old, &backups).unwrap();
        std::fs::write(root.join("s/SKILL.md"), "---\nname: s\nversion: 1.0.0\n---\nedited by hand\n").unwrap();
        assert_eq!(provide_at(&root, &old, &backups).unwrap().outcome, Installed::Changed, "same version: the edit stays");
        let new = versioned("s", "1.1.0");
        let Installed::Replaced(backup) = provide_at(&root, &new, &backups).unwrap().outcome else { panic!("not replaced") };
        assert!(std::fs::read_to_string(backup.join(SKILL_MD)).unwrap().contains("edited by hand"));
        assert!(std::fs::read_to_string(root.join("s/SKILL.md")).unwrap().contains("body 1.1.0"));
        assert!(root.join("s").join(MARKER).is_file());
        // A copy jazari never wrote, with no version, is older than any version.
        std::fs::create_dir_all(root.join("t")).unwrap();
        std::fs::write(root.join("t/SKILL.md"), "legacy").unwrap();
        assert!(matches!(provide_at(&root, &versioned("t", "1.0.0"), &backups).unwrap().outcome, Installed::Replaced(_)));
        // A newer installed copy is never taken back to an older one.
        std::fs::write(root.join("t/SKILL.md"), "---\nname: t\nversion: 2.0.0\n---\nmine\n").unwrap();
        assert_eq!(provide_at(&root, &versioned("t", "1.0.0"), &backups).unwrap().outcome, Installed::Changed);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&backups);
    }

    #[test]
    fn session_start_installs_missing_ticked_skills_and_updates_project_copies() {
        let (personal, project, backups) = (temp_dir("sync-personal"), temp_dir("sync-project"), temp_dir("sync-backups"));
        let (a, b) = (versioned("a", "1.0.0"), versioned("b", "1.0.0"));
        let lib = vec![a.clone(), b.clone()];
        let done = sync_roots(&personal, std::slice::from_ref(&project), &lib, &["a".into()], &backups);
        assert_eq!(done.len(), 1, "only the ticked skill is installed");
        assert!(personal.join("a/SKILL.md").is_file() && !personal.join("b").exists());
        assert!(sync_roots(&personal, std::slice::from_ref(&project), &lib, &["a".into()], &backups).is_empty(), "nothing to do twice");
        // A project copy jazari wrote follows the library, and then the personal copy is not added.
        provide_at(&project, &b, &backups).unwrap();
        let lib = vec![versioned("a", "1.1.0"), versioned("b", "1.1.0")];
        let done: Vec<Report> = sync_roots(&personal, std::slice::from_ref(&project), &lib, &["a".into(), "b".into()], &backups).into_iter().map(Result::unwrap).collect();
        assert_eq!(done.iter().map(|r| (r.key.as_str(), r.outcome.clone())).collect::<Vec<_>>(), [("a", Installed::Updated), ("b", Installed::Updated)]);
        assert!(done[1].path.starts_with(&project) && !personal.join("b").exists());
        // Deleted by hand: the next session start puts it back.
        std::fs::remove_dir_all(personal.join("a")).unwrap();
        let done = sync_roots(&personal, &[], &lib, &["a".into()], &backups);
        assert_eq!(done[0].as_ref().unwrap().outcome, Installed::New);
        for d in [personal, project, backups] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn project_roots_stop_below_the_home_folder() {
        let home = dirs::home_dir().unwrap();
        assert!(project_roots(Some(&home)).is_empty());
        assert!(project_roots(None).is_empty());
    }

    #[test]
    fn revision_sends_small_text_files_and_names_the_rest() {
        let skill = &library_in(Path::new("/nonexistent"))[0];
        let (sent, named) = for_revision(&skill.files);
        assert!(sent.iter().any(|f| f.path == "scripts/check.py"));
        assert!(named.contains(&"data/cefrj-vocabulary.csv".to_string()));
        assert!(!sent.iter().chain(std::iter::empty()).any(|f| f.path == SKILL_MD));
    }
}

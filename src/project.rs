//! The prompts and skills saved for one project, and `jazari build project`.
//!
//! A project is the main checkout of a git repo (all its worktrees share it), found with
//! `git rev-parse --git-common-dir`; outside git it is the folder itself. Its set is stored in
//! jazari's data folder, never in the repo, as `projects/<name>-<hash>.json`. A project without a
//! saved set starts with the global ticked defaults (`settings::rules`).

use crate::rules::{ALWAYS, COORDINATION, Rule};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The longest project name kept in a file name.
const MAX_NAME: usize = 40;

/// Runs `git rev-parse --git-common-dir` in a folder; None outside git or without git.
pub type Git<'a> = &'a dyn Fn(&Path) -> Option<String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// The canonical path of the main checkout, or of the folder outside git.
    pub root: PathBuf,
    /// The folder's name, for people to read.
    pub name: String,
}

/// What is saved for a project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub path: String,
    #[serde(default)]
    pub keys: Vec<String>,
    /// The teammates' set for `jazari claude --team`. None: the lead's set without coordination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teammates: Option<Vec<String>>,
    #[serde(default)]
    pub updated: i64,
}

/// Where a session's set comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Saved for the project.
    Project,
    /// The global ticked defaults: the project has no saved set.
    Defaults,
}

/// The set a session of a project starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub keys: Vec<String>,
    pub from: Source,
    /// The teammates' saved set, cleaned like `keys`; None when none is saved.
    pub teammates: Option<Vec<String>>,
    /// Saved keys that are no longer in the library, left out.
    pub gone: Vec<String>,
}

/// The real `git rev-parse --git-common-dir`.
pub fn git_common_dir(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git").args(["rev-parse", "--git-common-dir"]).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn canonical(p: &Path) -> PathBuf {
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    strip_verbatim(p)
}

/// Windows' canonical paths start with `\\?\`, which git and people don't write.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

/// The project that `dir` belongs to.
pub fn find(dir: &Path, git: Git) -> Project {
    let dir = canonical(dir);
    let root = match git(&dir) {
        Some(common) => {
            let common = Path::new(&common);
            let common = canonical(&if common.is_absolute() { common.to_path_buf() } else { dir.join(common) });
            // `<repo>/.git` for a normal repo and its worktrees; a bare repo is its own folder.
            match common.file_name() {
                Some(n) if n == ".git" => common.parent().map(Path::to_path_buf).unwrap_or(common),
                _ => common,
            }
        }
        None => dir,
    };
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    Project { root, name }
}

/// The folder that holds every project's file.
pub fn dir_in(data: &Path) -> PathBuf {
    data.join("projects")
}

/// `<data>/projects/<name>-<hash>.json`: the name for people, the hash of the path to keep it unique.
pub fn file_in(data: &Path, p: &Project) -> PathBuf {
    let safe: String = p.name.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '-' }).take(MAX_NAME).collect();
    let safe = safe.trim_matches(['-', '.']);
    let safe = if safe.is_empty() { "project" } else { safe };
    let hash = format!("{:016x}", crate::rules::fnv1a(p.root.to_string_lossy().as_bytes()));
    dir_in(data).join(format!("{safe}-{}.json", &hash[..8]))
}

/// The project's saved set; None when it has none, or its file can't be read.
pub fn load_in(data: &Path, p: &Project) -> Option<Saved> {
    let text = std::fs::read(file_in(data, p)).ok()?;
    serde_json::from_slice(&text).ok()
}

pub fn save_in(data: &Path, p: &Project, saved: &Saved) -> anyhow::Result<PathBuf> {
    let path = file_in(data, p);
    let dir = path.parent().context("projects folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let saved = Saved { path: p.root.to_string_lossy().into_owned(), updated: chrono::Utc::now().timestamp(), ..saved.clone() };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Deletes the project's set; true when there was one.
pub fn clear_in(data: &Path, p: &Project) -> anyhow::Result<bool> {
    let path = file_in(data, p);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("deleting {}", path.display())),
    }
}

/// Every saved project, by name.
pub fn list_in(data: &Path) -> Vec<Saved> {
    let mut out: Vec<Saved> = std::fs::read_dir(dir_in(data))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by(|a: &Saved, b: &Saved| a.path.cmp(&b.path));
    out
}

/// `keys` in the library, in order, once each, without "Working with jazari" (every launch file
/// starts with it anyway); the rest are returned as gone.
fn clean(lib: &[Rule], keys: &[String], gone: &mut Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in keys {
        if k == ALWAYS || out.contains(k) {
            continue;
        }
        if lib.iter().any(|r| &r.key == k) {
            out.push(k.clone());
        } else if !gone.contains(k) {
            gone.push(k.clone());
        }
    }
    out
}

/// The set a session of the project starts with: its saved set, else the global defaults.
pub fn resolve(lib: &[Rule], saved: Option<&Saved>, defaults: &[String]) -> Resolved {
    let mut gone = Vec::new();
    match saved {
        Some(s) => {
            let keys = clean(lib, &s.keys, &mut gone);
            let teammates = s.teammates.as_ref().map(|t| clean(lib, t, &mut gone));
            Resolved { keys, from: Source::Project, teammates, gone }
        }
        // The defaults drop their unknown keys quietly, as `c` does.
        None => Resolved { keys: clean(lib, defaults, &mut Vec::new()), from: Source::Defaults, teammates: None, gone },
    }
}

/// The project of `dir` and the set its sessions start with: what `jazari claude`, the handoff and
/// the `c` picker (also in its copy-path mode) start from.
pub fn set_for(data: &Path, dir: &Path, git: Git, lib: &[Rule], defaults: &[String]) -> (Project, Option<Saved>, Resolved) {
    let p = find(dir, git);
    let saved = load_in(data, &p);
    let res = resolve(lib, saved.as_ref(), defaults);
    (p, saved, res)
}

/// The one warning line for saved keys that are no longer in the library.
pub fn gone_warning(gone: &[String], teammates: bool) -> Option<String> {
    let flag = if teammates { " --teammates" } else { "" };
    let first = gone.first()?;
    let quoted: Vec<String> = gone.iter().map(|k| format!("\"{k}\"")).collect();
    Some(format!(
        "jazari: left out {} (no longer in your library); `jazari build project remove{flag} {first}` cleans it up",
        quoted.join(", ")
    ))
}

/// The lines of the set, "Working with jazari" first.
pub fn set_lines(keys: &[String]) -> Vec<String> {
    let mut out = vec![format!("  {ALWAYS} (always)")];
    out.extend(keys.iter().map(|k| format!("  {k}")));
    out
}

#[derive(clap::Args)]
pub struct ProjectArgs {
    #[command(subcommand)]
    cmd: Option<ProjectCmd>,
    /// Work on the project of this folder instead of the current one.
    #[arg(long, global = true)]
    path: Option<PathBuf>,
    /// Work on the teammates' set (for `jazari claude --team`) instead of the session's.
    #[arg(long, global = true)]
    teammates: bool,
}

#[derive(clap::Subcommand)]
enum ProjectCmd {
    /// Choose the set in the picker; saving writes it for the project.
    Edit,
    /// Add prompts or skills to the project's set.
    Add { keys: Vec<String> },
    /// Remove prompts or skills from the project's set.
    Remove { keys: Vec<String> },
    /// Delete the project's set: the global defaults apply again.
    Clear,
    /// Every project with a saved set.
    List,
}

/// What `jazari build project` reads and writes, so tests use a temp folder, a fake git and no
/// terminal.
pub struct Env<'a> {
    pub data: PathBuf,
    pub cwd: PathBuf,
    pub git: Git<'a>,
    pub library: Vec<Rule>,
    pub defaults: Vec<String>,
    /// Opens the picker with the given keys ticked; None when it was closed without a choice.
    pub pick: &'a dyn Fn(crate::pick_term::Picker) -> anyhow::Result<Option<crate::pick_term::Picked>>,
}

pub fn run(a: ProjectArgs) -> anyhow::Result<()> {
    let pick = |p: crate::pick_term::Picker| crate::pick_term::run(p);
    let env = Env {
        data: crate::data_dir(),
        cwd: std::env::current_dir().context("reading the current folder")?,
        git: &git_common_dir,
        library: crate::rules::pickable(),
        defaults: crate::settings::rules(),
        pick: &pick,
    };
    println!("{}", run_in(&env, a)?);
    Ok(())
}

pub fn run_in(env: &Env, a: ProjectArgs) -> anyhow::Result<String> {
    if matches!(a.cmd, Some(ProjectCmd::List)) {
        return Ok(list_text(&list_in(&env.data)));
    }
    let dir = a.path.as_deref().map(|p| if p.is_absolute() { p.to_path_buf() } else { env.cwd.join(p) }).unwrap_or_else(|| env.cwd.clone());
    if !dir.is_dir() {
        anyhow::bail!("{} is not a folder", dir.display());
    }
    let (p, saved, res) = set_for(&env.data, &dir, env.git, &env.library, &env.defaults);
    let current = if a.teammates { teammates_of(&res) } else { res.keys.clone() };
    let set = |keys: Vec<String>| -> anyhow::Result<String> {
        let mut s = saved.clone().unwrap_or_else(|| Saved { keys: res.keys.clone(), ..Saved::default() });
        if a.teammates { s.teammates = Some(keys) } else { s.keys = keys }
        save_in(&env.data, &p, &s)?;
        Ok(show(env, &p))
    };
    match a.cmd {
        None => Ok(show(env, &p)),
        Some(ProjectCmd::List) => unreachable!(),
        Some(ProjectCmd::Clear) => {
            if a.teammates {
                let Some(mut s) = saved.filter(|s| s.teammates.is_some()) else {
                    return Ok(format!("{} has no teammates' set saved.\n\n{}", p.name, show(env, &p)));
                };
                s.teammates = None;
                save_in(&env.data, &p, &s)?;
                return Ok(show(env, &p));
            }
            let note = if clear_in(&env.data, &p)? { "Deleted the set" } else { "No set was saved" };
            Ok(format!("{note} for {}; your global defaults apply.\n\n{}", p.name, show(env, &p)))
        }
        Some(ProjectCmd::Add { keys }) => {
            let unknown: Vec<&String> = keys.iter().filter(|k| !env.library.iter().any(|r| &r.key == *k)).collect();
            if keys.is_empty() || !unknown.is_empty() {
                let names: Vec<&str> = env.library.iter().map(|r| r.key.as_str()).collect();
                let what = if keys.is_empty() { "name at least one".to_string() } else { format!("not in your library: {}", unknown.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")) };
                anyhow::bail!("{what}\nChoose from: {}", names.join(", "));
            }
            let mut next = current;
            next.extend(keys.into_iter().filter(|k| k != ALWAYS));
            let mut seen = Vec::new();
            next.retain(|k| if seen.contains(k) { false } else { seen.push(k.clone()); true });
            set(next)
        }
        Some(ProjectCmd::Remove { keys }) => {
            let missing: Vec<&String> = keys.iter().filter(|k| !current.contains(k) && !res.gone.contains(k)).collect();
            if keys.is_empty() || !missing.is_empty() {
                let what = if keys.is_empty() { "name at least one".to_string() } else { format!("not in the set: {}", missing.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")) };
                anyhow::bail!("{what}");
            }
            let mut s = saved.clone().unwrap_or_else(|| Saved { keys: res.keys.clone(), ..Saved::default() });
            let list = if a.teammates { s.teammates.get_or_insert_with(|| teammates_of(&res)) } else { &mut s.keys };
            list.retain(|k| !keys.contains(k));
            save_in(&env.data, &p, &s)?;
            Ok(show(env, &p))
        }
        Some(ProjectCmd::Edit) => {
            let title = if a.teammates { format!("Teammates' prompts and skills for {}", p.name) } else { format!("Prompts and skills for {}", p.name) };
            let picker = crate::pick_term::Picker::new(env.library.clone(), &current, title, false);
            match (env.pick)(picker)? {
                Some(picked) => set(picked.keys),
                None => Ok("Closed without saving.".into()),
            }
        }
    }
}

/// The teammates' set: the saved one, else the lead's set without coordination.
pub fn teammates_of(res: &Resolved) -> Vec<String> {
    res.teammates.clone().unwrap_or_else(|| res.keys.iter().filter(|k| k.as_str() != COORDINATION).cloned().collect())
}

fn show(env: &Env, p: &Project) -> String {
    let saved = load_in(&env.data, p);
    let res = resolve(&env.library, saved.as_ref(), &env.defaults);
    let mut out = vec![format!("Project: {} ({})", p.name, p.root.display())];
    out.push(match res.from {
        Source::Project => "Set: saved for this project".into(),
        Source::Defaults => "Set: your global defaults (none saved for this project)".into(),
    });
    out.extend(set_lines(&res.keys));
    out.push(match &res.teammates {
        Some(_) => "Teammates (jazari claude --team): saved for this project".into(),
        None => "Teammates (jazari claude --team): the set above without coordination".into(),
    });
    out.extend(set_lines(&teammates_of(&res)));
    out.extend(gone_warning(&res.gone, false));
    out.push(String::new());
    out.push(match res.from {
        Source::Project => format!("Saved in {}", file_in(&env.data, p).display()),
        Source::Defaults => "`jazari build project edit` or `add <name>` saves a set for this project.".into(),
    });
    out.push("`jazari claude` starts Claude Code here with it.".into());
    out.join("\n")
}

fn list_text(all: &[Saved]) -> String {
    if all.is_empty() {
        return "No project has a saved set yet. `jazari build project edit` saves one for the current folder's project.".into();
    }
    let mut out: Vec<String> = all.iter().map(|s| format!("{}  {}", s.path, if s.keys.is_empty() { "(only jazari)".into() } else { s.keys.join(", ") })).collect();
    out.push(String::new());
    out.push("Every set also starts with \"Working with jazari\".".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::Kind;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jazari-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        canonical(&d)
    }

    fn lib() -> Vec<Rule> {
        ["coordination", "output-hygiene", "easy-english"]
            .iter()
            .map(|k| Rule::new(k.to_string(), format!("{k} title"), format!("{k} summary"), "body".into(), false, Kind::Prompt))
            .collect()
    }

    fn no_git(_: &Path) -> Option<String> {
        None
    }

    fn no_pick(_: crate::pick_term::Picker) -> anyhow::Result<Option<crate::pick_term::Picked>> {
        panic!("no picker in this test")
    }

    fn env<'a>(data: &Path, cwd: &Path, git: Git<'a>) -> Env<'a> {
        Env { data: data.into(), cwd: cwd.into(), git, library: lib(), defaults: vec!["coordination".into()], pick: &no_pick }
    }

    fn args(cmd: Option<ProjectCmd>, teammates: bool) -> ProjectArgs {
        ProjectArgs { cmd, path: None, teammates }
    }

    #[test]
    fn a_subfolder_and_a_worktree_belong_to_the_main_checkout() {
        let root = temp("find");
        let repo = root.join("shop");
        let sub = repo.join("src/deep");
        let wt = root.join("shop-wt");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        // git prints the common dir relative to the folder it runs in, or absolute.
        let git = |d: &Path| -> Option<String> {
            if d.starts_with(&repo) {
                Some(if d == repo { ".git".into() } else { "../../.git".into() })
            } else if d == wt {
                Some(repo.join(".git").display().to_string())
            } else {
                None
            }
        };
        let main = find(&repo, &git);
        assert_eq!(main, Project { root: repo.clone(), name: "shop".into() });
        assert_eq!(find(&sub, &git), main, "a subfolder");
        assert_eq!(find(&wt, &git), main, "a worktree");
        let plain = find(&root, &git);
        assert_eq!(plain.root, root, "outside git, the folder itself");
        #[cfg(unix)]
        {
            let link = root.join("link");
            std::os::unix::fs::symlink(&repo, &link).unwrap();
            assert_eq!(find(&link, &git), main, "two paths to one folder are one project");
        }
        assert_ne!(file_in(&root, &main), file_in(&root, &plain));
        assert!(file_in(&root, &main).file_name().unwrap().to_string_lossy().starts_with("shop-"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn falls_back_to_the_defaults_until_a_set_is_saved_and_after_clear() {
        let data = temp("fallback");
        let cwd = temp("fallback-cwd");
        let e = env(&data, &cwd, &no_git);
        let p = find(&cwd, &no_git);
        assert_eq!(resolve(&e.library, load_in(&data, &p).as_ref(), &e.defaults).from, Source::Defaults);
        let out = run_in(&e, args(Some(ProjectCmd::Add { keys: vec!["easy-english".into()] }), false)).unwrap();
        assert!(out.contains("saved for this project"), "{out}");
        let res = resolve(&e.library, load_in(&data, &p).as_ref(), &e.defaults);
        assert_eq!((res.keys, res.from), (vec!["coordination".to_string(), "easy-english".into()], Source::Project), "add starts from the defaults");
        run_in(&e, args(Some(ProjectCmd::Clear), false)).unwrap();
        assert_eq!(resolve(&e.library, load_in(&data, &p).as_ref(), &e.defaults).from, Source::Defaults);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_folder_of_a_configured_project_defaults_to_its_set() {
        // The `c` picker's copy-path mode ticks this set for a session that is already running.
        let data = temp("copy-path");
        let cwd = temp("copy-path-cwd");
        let sub = cwd.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let git = |_: &Path| Some(cwd.join(".git").display().to_string());
        let defaults = ["coordination".to_string()];
        assert_eq!(set_for(&data, &sub, &git, &lib(), &defaults).2.keys, defaults, "no set saved: the defaults");
        save_in(&data, &find(&cwd, &git), &Saved { keys: vec!["easy-english".into()], ..Saved::default() }).unwrap();
        let (p, _, res) = set_for(&data, &sub, &git, &lib(), &defaults);
        assert_eq!((p.root, res.keys, res.from), (cwd.clone(), vec!["easy-english".to_string()], Source::Project));
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn keys_are_cleaned_once_each_and_unknown_ones_reported() {
        let saved = Saved { keys: vec!["jazari".into(), "easy-english".into(), "old".into(), "easy-english".into()], ..Saved::default() };
        let res = resolve(&lib(), Some(&saved), &[]);
        assert_eq!(res.keys, ["easy-english"]);
        assert_eq!(res.gone, ["old"]);
        let w = gone_warning(&res.gone, false).unwrap();
        assert!(w.contains("\"old\"") && w.contains("jazari build project remove old"), "{w}");
        assert_eq!(gone_warning(&[], false), None);
        assert_eq!(set_lines(&res.keys)[0], "  jazari (always)", "jazari first");
    }

    #[test]
    fn add_refuses_unknown_keys_and_remove_names_missing_ones() {
        let data = temp("add");
        let cwd = temp("add-cwd");
        let e = env(&data, &cwd, &no_git);
        let err = run_in(&e, args(Some(ProjectCmd::Add { keys: vec!["nope".into()] }), false)).unwrap_err().to_string();
        assert!(err.contains("not in your library: nope") && err.contains("easy-english"), "{err}");
        let err = run_in(&e, args(Some(ProjectCmd::Remove { keys: vec!["easy-english".into()] }), false)).unwrap_err().to_string();
        assert!(err.contains("not in the set: easy-english"), "{err}");
        run_in(&e, args(Some(ProjectCmd::Remove { keys: vec!["coordination".into()] }), false)).unwrap();
        let p = find(&cwd, &no_git);
        assert_eq!(load_in(&data, &p).unwrap().keys, Vec::<String>::new(), "an empty set is saved, not the defaults");
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn the_teammates_set_is_its_own_list() {
        let data = temp("mates");
        let cwd = temp("mates-cwd");
        let e = env(&data, &cwd, &no_git);
        let p = find(&cwd, &no_git);
        let res = resolve(&e.library, None, &["coordination".into(), "easy-english".into()]);
        assert_eq!(teammates_of(&res), ["easy-english"], "without a saved list: the lead's set without coordination");
        run_in(&e, args(Some(ProjectCmd::Add { keys: vec!["output-hygiene".into()] }), true)).unwrap();
        let s = load_in(&data, &p).unwrap();
        assert_eq!(s.keys, ["coordination"], "the session's set is kept");
        assert_eq!(s.teammates.as_deref(), Some(&["output-hygiene".to_string()][..]));
        run_in(&e, args(Some(ProjectCmd::Clear), true)).unwrap();
        assert_eq!(load_in(&data, &p).unwrap().teammates, None, "clear --teammates keeps the session's set");
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}

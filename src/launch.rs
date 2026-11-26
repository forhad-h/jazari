//! `jazari claude`: starts Claude Code in the current folder with the project's prompts and
//! skills (or the global ticked defaults), "Working with jazari" always first.
//!
//! The launch files are written with `rules::ensure_in` / `rules::ensure_team_in`, the skills are
//! installed as `c` installs them, and the set is logged under the new session's id so the dashboard
//! shows it from the first second. The start itself is an injected function, so tests never run
//! `claude`.

use crate::project::{self, Source};
use crate::rules::{Mode, Rule};
use anyhow::{Context, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(clap::Args)]
pub struct ClaudeArgs {
    /// Choose the prompts and skills for this start in a picker; it can also save them for the project.
    #[arg(long)]
    pick: bool,
    /// Start the lead of an agent team; each teammate it spawns reads the teammates' set.
    #[arg(long)]
    team: bool,
    /// The teammates' prompts and skills for this start, comma-separated (with --team).
    #[arg(long, value_delimiter = ',', requires = "team")]
    teammates: Option<Vec<String>>,
    /// Arguments for claude, after `--`, passed on unchanged.
    #[arg(last = true, value_parser = clap::builder::OsStringValueParser::new())]
    args: Vec<OsString>,
}

/// What `jazari claude` reads and does, so tests use temp folders, a fake git and no `claude`.
pub struct Env<'a> {
    pub data: PathBuf,
    pub cwd: PathBuf,
    pub git: project::Git<'a>,
    pub library: Vec<Rule>,
    pub defaults: Vec<String>,
    pub pick: &'a dyn Fn(crate::pick_term::Picker) -> anyhow::Result<Option<crate::pick_term::Picked>>,
    /// Installs the chosen skills where Claude Code loads them; returns the notes to print.
    pub provide: &'a dyn Fn(&[&Rule]) -> anyhow::Result<Vec<String>>,
    pub new_id: &'a dyn Fn() -> String,
}

/// A start, ready to run.
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    /// Warnings and notes, for stderr.
    pub notes: Vec<String>,
    /// The set in use, printed first on stdout.
    pub first_line: String,
    /// The new session's id; None when the arguments resume a session, which keeps its id.
    pub session_id: Option<String>,
    /// The set to log for the dashboard: key and where it came from.
    pub set: Vec<(String, &'static str)>,
    /// The launch file (the lead's, for a team).
    pub file: PathBuf,
    /// The arguments for `claude`.
    pub args: Vec<OsString>,
}

/// The flags jazari sets itself and refuses in the passed arguments.
const OWN_FLAGS: [&str; 2] = ["--session-id", "--append-system-prompt-file"];
/// The flags that resume a session, which keeps its id.
const RESUME_FLAGS: [&str; 4] = ["--resume", "-r", "--continue", "-c"];

fn has_flag(args: &[OsString], flags: &[&str]) -> Option<String> {
    args.iter().filter_map(|a| a.to_str()).find_map(|a| {
        flags.iter().find(|f| a == **f || (f.starts_with("--") && a.starts_with(&format!("{f}=")))).map(|f| f.to_string())
    })
}

pub fn prepare(env: &Env, a: &ClaudeArgs) -> anyhow::Result<Option<Launch>> {
    if let Some(flag) = has_flag(&a.args, &OWN_FLAGS) {
        bail!("jazari claude sets {flag} itself; leave it out of the arguments after --");
    }
    let (p, saved, res) = project::set_for(&env.data, &env.cwd, env.git, &env.library, &env.defaults);
    let mut notes: Vec<String> = project::gone_warning(&res.gone, false).into_iter().collect();
    let (keys, source) = if a.pick {
        let picker = crate::pick_term::Picker::new(env.library.clone(), &res.keys, format!("Prompts and skills for {}", p.name), true);
        let Some(picked) = (env.pick)(picker)? else { return Ok(None) };
        if picked.save {
            let mut s = saved.clone().unwrap_or_default();
            s.keys = picked.keys.clone();
            let file = project::save_in(&env.data, &p, &s)?;
            notes.push(format!("jazari: saved the set for {} in {}", p.name, file.display()));
        }
        (picked.keys, if picked.save { "project" } else { "picked" })
    } else {
        (res.keys.clone(), if res.from == Source::Project { "project" } else { "default" })
    };
    let chosen = crate::rules::known(&env.library, &keys);
    let teammates: Option<Vec<String>> = match &a.teammates {
        Some(t) => {
            let unknown: Vec<&str> = t.iter().map(String::as_str).filter(|k| !env.library.iter().any(|r| r.key == *k)).collect();
            if !unknown.is_empty() {
                notes.push(format!("jazari: left out {} from --teammates (not in your library)", unknown.join(", ")));
            }
            Some(t.clone())
        }
        None if a.team => res.teammates.clone(),
        None => None,
    };
    let mates = teammates.as_ref().map(|t| crate::rules::known(&env.library, t));
    let mut all: Vec<&Rule> = chosen.clone();
    for r in mates.iter().flatten() {
        if !all.iter().any(|c| c.key == r.key) {
            all.push(r);
        }
    }
    notes.extend((env.provide)(&all)?);
    let file = if a.team {
        crate::rules::ensure_team_in(&env.data, &chosen, mates.as_deref())?
    } else {
        crate::rules::ensure_in(&env.data, &chosen, Mode::Session)?
    };
    let resumed = has_flag(&a.args, &RESUME_FLAGS).is_some();
    let session_id = (!resumed).then(|| (env.new_id)());
    let mut args: Vec<OsString> = Vec::new();
    if let Some(id) = &session_id {
        args.extend(["--session-id".into(), id.into()]);
    }
    args.extend(["--append-system-prompt-file".into(), file.clone().into_os_string()]);
    args.extend(a.args.iter().cloned());

    let names: Vec<String> = std::iter::once(format!("{} (always)", crate::rules::ALWAYS)).chain(chosen.iter().map(|r| r.key.clone())).collect();
    let whence = match source {
        "project" => format!("project {}", p.name),
        "picked" => "picked for this start".to_string(),
        _ => format!("your defaults; nothing saved for project {}", p.name),
    };
    let team = match &mates {
        _ if !a.team => String::new(),
        Some(m) => format!("; teammates: {}", m.iter().filter(|r| r.key != crate::rules::COORDINATION).map(|r| r.key.as_str()).collect::<Vec<_>>().join(", ")),
        None => "; teammates: the same without coordination".into(),
    };
    let lead = if a.team { " as an agent team lead" } else { "" };
    let first_line = format!("jazari: starting{lead} with {} ({whence}){team}", names.join(", "));
    let set = chosen.iter().map(|r| (r.key.clone(), source)).collect();
    Ok(Some(Launch { notes, first_line, session_id, set, file, args }))
}

pub fn run(a: ClaudeArgs) -> anyhow::Result<()> {
    let pick = |p: crate::pick_term::Picker| crate::pick_term::run(p);
    let provide = |chosen: &[&Rule]| -> anyhow::Result<Vec<String>> { Ok(crate::skills::provide(chosen)?.iter().map(|r| r.message()).collect()) };
    let env = Env {
        data: crate::data_dir(),
        cwd: std::env::current_dir().context("reading the current folder")?,
        git: &project::git_common_dir,
        library: crate::rules::pickable(),
        defaults: crate::settings::rules(),
        pick: &pick,
        provide: &provide,
        new_id: &crate::handoff_pick::new_session_id,
    };
    let Some(launch) = prepare(&env, &a)? else {
        eprintln!("jazari: closed the picker; Claude Code was not started.");
        return Ok(());
    };
    for n in &launch.notes {
        eprintln!("{n}");
    }
    println!("{}", launch.first_line);
    if let Some(id) = &launch.session_id {
        let set: Vec<(String, String, String)> = launch.set.iter().map(|(k, from)| (k.clone(), from.to_string(), String::new())).collect();
        crate::handoff::log_launch_set(id, &set);
    }
    let code = start(&launch.args, &env.cwd)?;
    std::process::exit(code);
}

/// Starts `claude`: in place of jazari on macOS and Linux, so it owns the terminal and its
/// signals; on Windows as a child whose exit code is passed through.
fn start(args: &[OsString], cwd: &Path) -> anyhow::Result<i32> {
    let program = crate::handoff::claude();
    let by_hand = || {
        let words: Vec<String> = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args.iter().map(|a| crate::rules::quote_arg(&a.to_string_lossy())))
            .collect();
        words.join(" ")
    };
    let mut cmd = std::process::Command::new(&program);
    cmd.args(args).current_dir(cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec();
        if e.kind() == std::io::ErrorKind::NotFound {
            bail!("claude was not found on your PATH. Start it by hand:\n{}", by_hand());
        }
        bail!("couldn't start claude: {e}");
    }
    #[cfg(not(unix))]
    {
        let status = match cmd.status() {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::process::Command::new("claude.cmd").args(args).current_dir(cwd).status(),
            other => other,
        };
        match status {
            Ok(s) => Ok(s.code().unwrap_or(1)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("claude was not found on your PATH. Start it by hand:\n{}", by_hand()),
            Err(e) => Err(e).context("couldn't start claude"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pick_term::{Picked, Picker};
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        a: ClaudeArgs,
    }

    fn parse(line: &[&str]) -> ClaudeArgs {
        Cli::try_parse_from(std::iter::once("claude").chain(line.iter().copied())).unwrap().a
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jazari-launch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(&d).unwrap()
    }

    fn lib() -> Vec<Rule> {
        let mut l = crate::rules::builtin_library();
        l.retain(|r| r.key != crate::rules::ALWAYS);
        l
    }

    fn no_git(_: &Path) -> Option<String> {
        None
    }

    fn no_pick(_: Picker) -> anyhow::Result<Option<Picked>> {
        panic!("no picker in this test")
    }

    fn no_skills(_: &[&Rule]) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }

    fn id() -> String {
        "11111111-2222-4333-8444-555555555555".into()
    }

    fn env<'a>(data: &Path, cwd: &Path) -> Env<'a> {
        Env {
            data: data.into(),
            cwd: cwd.into(),
            git: &no_git,
            library: lib(),
            defaults: vec!["coordination".into()],
            pick: &no_pick,
            provide: &no_skills,
            new_id: &id,
        }
    }

    fn text(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn starts_with_the_defaults_then_the_project_set() {
        let (data, cwd) = (temp("defaults"), temp("defaults-cwd"));
        let e = env(&data, &cwd);
        let l = prepare(&e, &parse(&["--", "--model", "opus"])).unwrap().unwrap();
        assert!(l.first_line.starts_with("jazari: starting with jazari (always), coordination (your defaults"), "{}", l.first_line);
        assert_eq!(l.session_id.as_deref(), Some(id().as_str()));
        let args: Vec<String> = l.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args[..3], ["--session-id".to_string(), id(), "--append-system-prompt-file".into()]);
        assert_eq!(args[4..], ["--model".to_string(), "opus".into()], "the args after -- pass on unchanged");
        assert!(text(&l.file).starts_with("# Working with jazari"));
        let p = project::find(&cwd, &no_git);
        project::save_in(&data, &p, &project::Saved { keys: vec!["output-hygiene".into(), "jazari".into(), "gone".into()], ..Default::default() }).unwrap();
        let l = prepare(&e, &parse(&[])).unwrap().unwrap();
        assert!(l.first_line.contains("jazari (always), output-hygiene (project "), "{}", l.first_line);
        assert_eq!(l.notes.len(), 1, "one warning for the unknown key: {:?}", l.notes);
        assert!(l.notes[0].contains("\"gone\""));
        let t = text(&l.file);
        assert_eq!(t.matches("# Working with jazari").count(), 1, "jazari once, though the set names it");
        assert_eq!(l.set, [("output-hygiene".to_string(), "project")]);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn refuses_its_own_flags_and_keeps_a_resumed_id() {
        let (data, cwd) = (temp("flags"), temp("flags-cwd"));
        let e = env(&data, &cwd);
        for bad in [&["--", "--session-id", "x"][..], &["--", "--append-system-prompt-file=f"][..]] {
            let err = prepare(&e, &parse(bad)).unwrap_err().to_string();
            assert!(err.contains("sets --"), "{err}");
        }
        let l = prepare(&e, &parse(&["--", "--resume", "abc"])).unwrap().unwrap();
        assert_eq!(l.session_id, None);
        assert_eq!(l.args[0], OsString::from("--append-system-prompt-file"));
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn a_team_writes_both_files_with_jazari_first() {
        let (data, cwd) = (temp("team"), temp("team-cwd"));
        let e = env(&data, &cwd);
        let p = project::find(&cwd, &no_git);
        project::save_in(&data, &p, &project::Saved { keys: vec!["coordination".into()], teammates: Some(vec!["output-hygiene".into()]), ..Default::default() }).unwrap();
        let l = prepare(&e, &parse(&["--team"])).unwrap().unwrap();
        assert!(l.first_line.contains("as an agent team lead") && l.first_line.ends_with("teammates: output-hygiene"), "{}", l.first_line);
        let lead = text(&l.file);
        let mate = std::fs::read_dir(data.join("launch")).unwrap().map(|e| e.unwrap().path()).find(|f| f != &l.file).unwrap();
        assert!(lead.contains(&mate.display().to_string()), "the lead's file names the teammates' file");
        assert!(lead.starts_with("# Working with jazari") && text(&mate).starts_with("# Working with jazari"));
        assert!(text(&mate).contains("# Output hygiene"));
        let l = prepare(&e, &parse(&["--team", "--teammates", "easy-english,nope"])).unwrap().unwrap();
        assert!(l.notes.iter().any(|n| n.contains("nope")), "{:?}", l.notes);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn pick_uses_once_or_saves_for_the_project() {
        let (data, cwd) = (temp("pick"), temp("pick-cwd"));
        let once = |_: Picker| -> anyhow::Result<Option<Picked>> { Ok(Some(Picked { keys: vec!["output-hygiene".into()], save: false })) };
        let save = |_: Picker| -> anyhow::Result<Option<Picked>> { Ok(Some(Picked { keys: vec!["output-hygiene".into()], save: true })) };
        let closed = |_: Picker| -> anyhow::Result<Option<Picked>> { Ok(None) };
        let p = project::find(&cwd, &no_git);
        let l = prepare(&Env { pick: &once, ..env(&data, &cwd) }, &parse(&["--pick"])).unwrap().unwrap();
        assert!(l.first_line.contains("(picked for this start)"), "{}", l.first_line);
        assert_eq!(project::load_in(&data, &p), None, "nothing saved");
        prepare(&Env { pick: &save, ..env(&data, &cwd) }, &parse(&["--pick"])).unwrap().unwrap();
        assert_eq!(project::load_in(&data, &p).unwrap().keys, ["output-hygiene"]);
        assert_eq!(prepare(&Env { pick: &closed, ..env(&data, &cwd) }, &parse(&["--pick"])).unwrap(), None);
        let _ = std::fs::remove_dir_all(&data);
        let _ = std::fs::remove_dir_all(&cwd);
    }
}

//! Opening a new terminal that runs a command: first in the terminal the caller runs in, then in
//! the system's default terminal, then in a detached tmux session.
//!
//! Every launch is a list of `share::Step`s, so tests check the plans without running anything.

use crate::share::Step;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Windows,
    /// Linux and the BSDs.
    Unix,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Unix
        }
    }
}

/// The terminals marsad knows how to open a tab or window in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Kind {
    Tmux,
    Warp,
    Iterm,
    /// macOS Terminal.app.
    Terminal,
    Ghostty,
    Wezterm,
    Kitty,
    WindowsTerminal,
    /// The platform's default: Terminal.app, `x-terminal-emulator` and friends, or a console window.
    Default,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tmux => "tmux",
            Kind::Warp => "Warp",
            Kind::Iterm => "iTerm2",
            Kind::Terminal => "Terminal",
            Kind::Ghostty => "Ghostty",
            Kind::Wezterm => "WezTerm",
            Kind::Kitty => "kitty",
            Kind::WindowsTerminal => "Windows Terminal",
            Kind::Default => "the default terminal",
        }
    }
}

/// What the caller's environment says about the terminal it runs in.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub term_program: Option<String>,
    pub term: Option<String>,
    pub tmux: bool,
    pub kitty: bool,
    pub wezterm: bool,
    pub windows_terminal: bool,
}

impl Env {
    pub fn current() -> Self {
        let var = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
        Env {
            term_program: var("TERM_PROGRAM"),
            term: var("TERM"),
            tmux: var("TMUX").is_some(),
            kitty: var("KITTY_WINDOW_ID").is_some(),
            wezterm: var("WEZTERM_PANE").is_some(),
            windows_terminal: var("WT_SESSION").is_some(),
        }
    }

    /// The terminal the caller runs in, when marsad can open a tab in it. tmux comes first: a
    /// session inside tmux inside Warp gets its new window in the same tmux.
    pub fn current_kind(&self) -> Option<Kind> {
        if self.tmux {
            return Some(Kind::Tmux);
        }
        let program = self.term_program.as_deref().unwrap_or("").to_lowercase();
        match program.as_str() {
            "warpterminal" => Some(Kind::Warp),
            "iterm.app" => Some(Kind::Iterm),
            "apple_terminal" => Some(Kind::Terminal),
            "ghostty" => Some(Kind::Ghostty),
            "wezterm" => Some(Kind::Wezterm),
            _ if self.wezterm => Some(Kind::Wezterm),
            _ if self.kitty || self.term.as_deref() == Some("xterm-kitty") => Some(Kind::Kitty),
            _ if self.windows_terminal => Some(Kind::WindowsTerminal),
            _ => None,
        }
    }
}

/// The command to run in the new terminal, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Program and arguments; no shell is involved unless a terminal needs one.
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// A short name for the tab, window or tmux session.
    pub title: String,
}

impl Job {
    /// The job as one POSIX shell line, for terminals that take a line of text.
    fn shell_line(&self) -> String {
        let cmd: Vec<String> = self.argv.iter().map(|a| sh_quote(a)).collect();
        format!("cd {} && {}", sh_quote(&self.cwd.to_string_lossy()), cmd.join(" "))
    }

    fn cwd(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }
}

/// One way to open the job, with the steps tried in order until one works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    pub steps: Vec<Step>,
    /// A file the plan needs written first (Warp's launch configuration).
    pub file: Option<(PathBuf, String)>,
}

fn step(program: &'static str, args: Vec<String>, wait: bool) -> Step {
    Step { program, args, wait }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The plans to try, in order: `only` alone when given; otherwise the caller's own terminal, the
/// platform default, then tmux.
pub fn plans(env: &Env, platform: Platform, job: &Job, only: Option<Kind>, warp_dir: &Path) -> Vec<Plan> {
    let mut kinds = Vec::new();
    match only {
        Some(k) => kinds.push(k),
        None => {
            kinds.extend(env.current_kind());
            kinds.push(Kind::Default);
            kinds.push(Kind::Tmux);
        }
    }
    let mut seen = Vec::new();
    kinds.retain(|k| {
        // Terminal.app is the macOS default, so it isn't tried twice.
        let k = if *k == Kind::Default && platform == Platform::Macos { Kind::Terminal } else { *k };
        let new = !seen.contains(&k);
        seen.push(k);
        new
    });
    kinds.into_iter().filter_map(|k| plan(k, env, platform, job, warp_dir)).collect()
}

fn plan(kind: Kind, env: &Env, platform: Platform, job: &Job, warp_dir: &Path) -> Option<Plan> {
    let argv = || job.argv.clone();
    let with = |head: &[&str], tail: Vec<String>| [strs(head), tail].concat();
    let mut file = None;
    let steps = match (kind, platform) {
        (Kind::Tmux, Platform::Windows) => return None,
        (Kind::Tmux, _) if env.tmux => {
            vec![step("tmux", with(&["new-window", "-n", &job.title, "-c", &job.cwd()], argv()), true)]
        }
        (Kind::Tmux, _) => {
            vec![step("tmux", with(&["new-session", "-d", "-s", &job.title, "-c", &job.cwd()], argv()), true)]
        }
        (Kind::Warp, Platform::Windows) => return None,
        (Kind::Warp, _) => {
            // One file, rewritten each time, so Warp's list of launch configurations keeps one entry.
            let path = warp_dir.join(WARP_FILE);
            file = Some((path.clone(), warp_config(job)));
            let uri = format!("warp://launch/{}", path.to_string_lossy());
            let opener = if platform == Platform::Macos { "open" } else { "xdg-open" };
            vec![step(opener, vec![uri], true)]
        }
        (Kind::Iterm, Platform::Macos) => vec![osascript(&iterm_script(&job.shell_line()))],
        (Kind::Terminal | Kind::Default, Platform::Macos) => vec![osascript(&terminal_script(&job.shell_line()))],
        (Kind::Iterm | Kind::Terminal, _) => return None,
        (Kind::Ghostty, Platform::Macos) => {
            vec![step("open", with(&["-na", "Ghostty", "--args", &format!("--working-directory={}", job.cwd()), "-e"], argv()), true)]
        }
        (Kind::Ghostty, _) => vec![step("ghostty", with(&[&format!("--working-directory={}", job.cwd()), "-e"], argv()), false)],
        (Kind::Wezterm, _) => vec![
            step("wezterm", with(&["cli", "spawn", "--cwd", &job.cwd(), "--"], argv()), true),
            step("wezterm", with(&["start", "--cwd", &job.cwd(), "--"], argv()), false),
        ],
        (Kind::Kitty, _) => vec![
            step("kitty", with(&["@", "launch", "--type=tab", "--tab-title", &job.title, "--cwd", &job.cwd()], argv()), true),
            step("kitty", with(&["--directory", &job.cwd()], argv()), false),
        ],
        (Kind::WindowsTerminal, Platform::Windows) => {
            vec![step("wt", with(&["-w", "0", "new-tab", "--title", &job.title, "-d", &job.cwd()], argv()), false)]
        }
        (Kind::WindowsTerminal, _) => return None,
        (Kind::Default, Platform::Windows) => vec![step("powershell", strs(&["-NoProfile", "-NonInteractive", "-Command", &start_process(job)]), true)],
        (Kind::Default, Platform::Unix) => {
            let sh = |program: &'static str, head: &[&str]| step(program, with(head, strs(&["sh", "-c", &job.shell_line()])), false);
            vec![
                sh("x-terminal-emulator", &["-e"]),
                sh("gnome-terminal", &["--"]),
                sh("konsole", &["-e"]),
                sh("xfce4-terminal", &["-x"]),
                sh("xterm", &["-e"]),
            ]
        }
    };
    Some(Plan { kind, steps, file })
}

fn osascript(script: &str) -> Step {
    step("osascript", strs(&["-e", script]), true)
}

const WARP_FILE: &str = "marsad-handoff.yaml";

/// A new tab in iTerm2's front window (or a new window when none is open) that types `line`.
fn iterm_script(line: &str) -> String {
    format!(
        "tell application \"iTerm\"\n\
         activate\n\
         if (count of windows) = 0 then\n\
         create window with default profile\n\
         else\n\
         tell current window to create tab with default profile\n\
         end if\n\
         tell current session of current window to write text \"{}\"\n\
         end tell",
        applescript_escape(line)
    )
}

/// A new Terminal.app window running `line`.
fn terminal_script(line: &str) -> String {
    format!("tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell", applescript_escape(line))
}

/// A Warp launch configuration with one tab that runs the job.
fn warp_config(job: &Job) -> String {
    let cmd: Vec<String> = job.argv.iter().map(|a| sh_quote(a)).collect();
    format!(
        "---\nname: {title}\nwindows:\n  - tabs:\n      - title: {title}\n        layout:\n          cwd: {cwd}\n          commands:\n            - exec: {exec}\n",
        title = yaml_quote(&job.title),
        cwd = yaml_quote(&job.cwd()),
        exec = yaml_quote(&cmd.join(" ")),
    )
}

/// PowerShell's Start-Process opens a console program in a new console window.
fn start_process(job: &Job) -> String {
    let (program, args) = job.argv.split_first().map_or(("", &[][..]), |(p, a)| (p.as_str(), a));
    let args: Vec<String> = args.iter().map(|a| ps_quote(a)).collect();
    let mut cmd = format!("Start-Process -FilePath {} -WorkingDirectory {}", ps_quote(program), ps_quote(&job.cwd()));
    if !args.is_empty() {
        cmd.push_str(&format!(" -ArgumentList {}", args.join(",")));
    }
    cmd
}

pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// PowerShell's own quoting: a single-quoted string with each `'` doubled. Argument lists are
/// also wrapped in double quotes, so Start-Process keeps an argument with spaces whole.
fn ps_quote(s: &str) -> String {
    if s.contains(' ') {
        format!("'\"{}\"'", s.replace('\'', "''"))
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

/// Where Warp looks for launch configurations.
pub fn warp_dir(platform: Platform) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    match platform {
        Platform::Unix => std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map_or_else(|| home.join(".local/share"), PathBuf::from)
            .join("warp-terminal/launch_configurations"),
        _ => home.join(".warp/launch_configurations"),
    }
}

/// What opening the job did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// A tab or window opened in this terminal.
    In(Kind),
    /// A detached tmux session started; the user attaches to it with this name.
    TmuxDetached(String),
}

/// Tries each plan until a step works. `write` writes a plan's file and `run` runs a step; the
/// errors of every failed try are returned when nothing worked.
pub fn open(
    plans: &[Plan],
    env: &Env,
    write: &mut dyn FnMut(&Path, &str) -> std::io::Result<()>,
    run: &mut dyn FnMut(&Step) -> Result<(), String>,
) -> Result<Opened, Vec<String>> {
    let mut errors = Vec::new();
    for plan in plans {
        if let Some((path, text)) = &plan.file
            && let Err(e) = write(path, text)
        {
            errors.push(format!("{}: couldn't write {}: {e}", plan.kind.label(), path.display()));
            continue;
        }
        for s in &plan.steps {
            match run(s) {
                Ok(()) if plan.kind == Kind::Tmux && !env.tmux => {
                    let name = s.args.iter().skip_while(|a| *a != "-s").nth(1).cloned().unwrap_or_default();
                    return Ok(Opened::TmuxDetached(name));
                }
                Ok(()) => return Ok(Opened::In(plan.kind)),
                Err(e) => errors.push(format!("{}: {e}", plan.kind.label())),
            }
        }
    }
    Err(errors)
}

/// Runs `open` for real.
pub fn open_now(plans: &[Plan], env: &Env) -> Result<Opened, Vec<String>> {
    let mut write = |path: &Path, text: &str| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, text)
    };
    open(plans, env, &mut write, &mut |s| crate::share::launch(s))
}

/// Whether `program` is on PATH.
pub fn on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into()).split(';').map(str::to_lowercase).collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&paths).any(|dir| exts.iter().any(|ext| dir.join(format!("{program}{ext}")).is_file()))
}

/// The command that installs tmux, for the package manager `has` finds; None on Windows, where
/// tmux needs WSL, or when no known package manager is there.
pub fn tmux_install_command(platform: Platform, has: impl Fn(&str) -> bool) -> Option<Vec<&'static str>> {
    match platform {
        Platform::Windows => None,
        Platform::Macos => has("brew").then(|| vec!["brew", "install", "tmux"]).or_else(|| has("port").then(|| vec!["sudo", "port", "install", "tmux"])),
        Platform::Unix => [
            ("apt-get", vec!["sudo", "apt-get", "install", "-y", "tmux"]),
            ("dnf", vec!["sudo", "dnf", "install", "-y", "tmux"]),
            ("pacman", vec!["sudo", "pacman", "-S", "--noconfirm", "tmux"]),
            ("zypper", vec!["sudo", "zypper", "install", "-y", "tmux"]),
            ("apk", vec!["sudo", "apk", "add", "tmux"]),
            ("brew", vec!["brew", "install", "tmux"]),
        ]
        .into_iter()
        .find(|(pm, _)| has(pm))
        .map(|(_, cmd)| cmd),
    }
}

/// Offers to install tmux, the last fallback for opening a session, when it's missing. Asks only
/// on an interactive terminal; otherwise prints the command.
pub fn offer_tmux() {
    use std::io::{BufRead, IsTerminal, Write};
    let platform = Platform::current();
    if platform == Platform::Windows || on_path("tmux") {
        return;
    }
    let Some(cmd) = tmux_install_command(platform, on_path) else {
        println!("Install tmux so a handoff can always start its new session (no package manager found to do it).");
        return;
    };
    let line = cmd.join(" ");
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        println!("Install tmux so a handoff can always start its new session: {line}");
        return;
    }
    print!("A handoff opens its new session in your terminal, and uses tmux when it can't. Install tmux now ({line})? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return;
    }
    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Skipped. Install it later with: {line}");
        return;
    }
    match std::process::Command::new(cmd[0]).args(&cmd[1..]).status() {
        Ok(s) if s.success() => println!("Installed tmux."),
        Ok(_) | Err(_) => eprintln!("warning: installing tmux failed; try it yourself: {line}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job { argv: strs(&["/opt/my tools/marsad", "handoff", "resume", "h1"]), cwd: PathBuf::from("/w/it's"), title: "marsad-h1".into() }
    }

    fn env(term_program: &str) -> Env {
        Env { term_program: Some(term_program.into()), ..Default::default() }
    }

    fn kinds(plans: &[Plan]) -> Vec<Kind> {
        plans.iter().map(|p| p.kind).collect()
    }

    #[test]
    fn finds_the_current_terminal() {
        assert_eq!(env("WarpTerminal").current_kind(), Some(Kind::Warp));
        assert_eq!(env("iTerm.app").current_kind(), Some(Kind::Iterm));
        assert_eq!(env("Apple_Terminal").current_kind(), Some(Kind::Terminal));
        assert_eq!(env("ghostty").current_kind(), Some(Kind::Ghostty));
        assert_eq!(Env { term: Some("xterm-kitty".into()), ..Default::default() }.current_kind(), Some(Kind::Kitty));
        assert_eq!(Env { windows_terminal: true, ..Default::default() }.current_kind(), Some(Kind::WindowsTerminal));
        assert_eq!(env("vscode").current_kind(), None, "an editor's terminal can't be driven");
        assert_eq!(Env { tmux: true, ..env("WarpTerminal") }.current_kind(), Some(Kind::Tmux), "tmux inside Warp opens a tmux window");
    }

    #[test]
    fn tries_current_then_default_then_tmux() {
        let dir = Path::new("/warp");
        assert_eq!(kinds(&plans(&env("WarpTerminal"), Platform::Macos, &job(), None, dir)), [Kind::Warp, Kind::Default, Kind::Tmux]);
        assert_eq!(kinds(&plans(&env("Apple_Terminal"), Platform::Macos, &job(), None, dir)), [Kind::Terminal, Kind::Tmux], "Terminal.app is the macOS default");
        assert_eq!(kinds(&plans(&env("vscode"), Platform::Unix, &job(), None, dir)), [Kind::Default, Kind::Tmux]);
        assert_eq!(kinds(&plans(&env("vscode"), Platform::Windows, &job(), None, dir)), [Kind::Default], "no tmux on Windows");
        assert_eq!(kinds(&plans(&env("WarpTerminal"), Platform::Macos, &job(), Some(Kind::Tmux), dir)), [Kind::Tmux]);
    }

    #[test]
    fn warp_gets_a_launch_configuration() {
        let p = &plans(&env("WarpTerminal"), Platform::Macos, &job(), None, Path::new("/home/.warp/launch_configurations"))[0];
        let (path, text) = p.file.as_ref().unwrap();
        assert_eq!(path, Path::new("/home/.warp/launch_configurations/marsad-handoff.yaml"));
        assert!(text.contains("cwd: \"/w/it's\""), "{text}");
        assert!(text.contains("- exec: \"'/opt/my tools/marsad' handoff resume h1\""), "{text}");
        // The URI carries the path as the platform joins it.
        let uri = format!("warp://launch/{}", Path::new("/home/.warp/launch_configurations").join(WARP_FILE).to_string_lossy());
        assert_eq!(p.steps, [step("open", vec![uri], true)]);
    }

    #[test]
    fn applescript_runs_a_quoted_shell_line() {
        let line = job().shell_line();
        assert_eq!(line, "cd '/w/it'\\''s' && '/opt/my tools/marsad' handoff resume h1");
        let p = &plans(&env("iTerm.app"), Platform::Macos, &job(), None, Path::new("/"))[0];
        let script = &p.steps[0].args[1];
        assert!(script.contains("write text \"cd '/w/it'\\\\''s' && '/opt/my tools/marsad' handoff resume h1\""), "{script}");
        let t = &plans(&env("vscode"), Platform::Macos, &job(), None, Path::new("/"))[0];
        assert_eq!(t.kind, Kind::Default);
        assert!(t.steps[0].args[1].contains("do script"), "the macOS default is Terminal.app");
    }

    #[test]
    fn tmux_opens_a_window_inside_tmux_and_a_detached_session_outside() {
        let inside = plans(&Env { tmux: true, ..Default::default() }, Platform::Unix, &job(), None, Path::new("/"));
        assert_eq!(inside[0].steps[0].args[..5], strs(&["new-window", "-n", "marsad-h1", "-c", "/w/it's"]));
        let outside = plans(&env("vscode"), Platform::Unix, &job(), Some(Kind::Tmux), Path::new("/"));
        assert_eq!(outside[0].steps[0].args[..3], strs(&["new-session", "-d", "-s"]));
        assert_eq!(outside[0].steps[0].args[6..], job().argv[..], "the command runs without a shell");
    }

    #[test]
    fn windows_default_uses_start_process() {
        let p = &plans(&env("vscode"), Platform::Windows, &job(), None, Path::new("/"))[0];
        assert_eq!(
            p.steps[0].args[3],
            "Start-Process -FilePath '\"/opt/my tools/marsad\"' -WorkingDirectory '/w/it''s' -ArgumentList 'handoff','resume','h1'"
        );
    }

    #[test]
    fn open_falls_through_until_one_works() {
        let e = env("WarpTerminal");
        let ps = plans(&e, Platform::Macos, &job(), None, Path::new("/warp"));
        let mut written = Vec::new();
        let mut tried = Vec::new();
        let got = open(
            &ps,
            &e,
            &mut |p, _| {
                written.push(p.to_path_buf());
                Ok(())
            },
            &mut |s| {
                tried.push(s.program);
                if s.program == "tmux" { Ok(()) } else { Err("no".into()) }
            },
        );
        assert_eq!(got, Ok(Opened::TmuxDetached("marsad-h1".into())));
        assert_eq!(tried, ["open", "osascript", "tmux"]);
        assert_eq!(written, [PathBuf::from("/warp/marsad-handoff.yaml")]);

        let all_fail = open(&ps, &e, &mut |_, _| Ok(()), &mut |_| Err("nope".into())).unwrap_err();
        assert_eq!(all_fail.len(), 3, "{all_fail:?}");
        let first = open(&ps, &e, &mut |_, _| Ok(()), &mut |_| Ok(()));
        assert_eq!(first, Ok(Opened::In(Kind::Warp)));
    }

    #[test]
    fn picks_a_package_manager_for_tmux() {
        let only = |name: &'static str| move |p: &str| p == name;
        assert_eq!(tmux_install_command(Platform::Macos, only("brew")), Some(vec!["brew", "install", "tmux"]));
        assert_eq!(tmux_install_command(Platform::Unix, only("dnf")), Some(vec!["sudo", "dnf", "install", "-y", "tmux"]));
        assert_eq!(tmux_install_command(Platform::Macos, |_| false), None);
        assert_eq!(tmux_install_command(Platform::Windows, |_| true), None);
    }
}

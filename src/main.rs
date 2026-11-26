mod act;
mod alerts;
mod app;
mod badge_art;
mod badges;
mod board;
mod card;
mod channel;
mod clean;
mod clean_judge;
mod clean_view;
mod clipboard;
mod context;
mod disk;
mod events;
mod handoff;
mod hook;
mod install;
mod jazari;
mod jsonl;
mod live;
mod model;
mod pin;
mod portable;
mod prompt_gen;
mod pressure;
mod prompt_view;
mod report;
mod risk;
mod rules;
mod settings;
mod share;
mod skills;
mod share_history;
mod share_view;
mod shot;
mod statusline;
mod store;
mod system;
mod term;
mod terminal;
mod transcript;
mod ui;
mod update;
mod usage;
mod watch;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// jazari runs an AI software company on your Claude subscription, and watches every Claude Code
/// session on this machine. `jazari` alone opens the dashboard.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    top: Option<Top>,
}

#[derive(Subcommand)]
enum Top {
    /// Watch and protect: the dashboard (alone), sessions, usage, disk, memory and risky commands.
    Watch {
        #[command(subcommand)]
        cmd: Option<Cmd>,
    },
    /// Build: products and tasks, Telegram, handoff, prompts and skills.
    #[command(subcommand)]
    Build(jazari::JazariCmd),
    /// Register jazari's hooks (and status line) in ~/.claude/settings.json.
    Install {
        /// Don't install the status line; context size is then estimated from transcripts.
        #[arg(long)]
        no_statusline: bool,
    },
    /// Remove jazari's hooks from ~/.claude/settings.json.
    Uninstall,
    /// Update jazari to the latest release (or turn automatic updates on/off with --auto).
    Update(update::UpdateArgs),
    /// Hook entrypoint called by Claude Code (reads hook JSON on stdin).
    #[command(hide = true)]
    Hook,
    /// Status line entrypoint called by Claude Code (reads status JSON on stdin).
    #[command(hide = true)]
    Statusline,
    /// Background disk watcher (started by the SessionStart hook), or with --hook the synchronous
    /// hook that checks the disk and risky commands before each tool call and prompt.
    #[command(hide = true)]
    Guard {
        #[arg(long)]
        hook: bool,
    },
}

impl Top {
    /// The command to run; None opens the dashboard.
    fn into_cmd(self) -> Option<Cmd> {
        match self {
            Top::Watch { cmd } => cmd,
            Top::Build(c) => Some(Cmd::Jazari(c)),
            Top::Install { no_statusline } => Some(Cmd::Install { no_statusline }),
            Top::Uninstall => Some(Cmd::Uninstall),
            Top::Update(a) => Some(Cmd::Update(a)),
            Top::Hook => Some(Cmd::Hook),
            Top::Statusline => Some(Cmd::Statusline),
            Top::Guard { hook } => Some(Cmd::Guard { hook }),
        }
    }
}

/// `jazari watch …`. The top-level commands run through the same dispatch, hidden here.
#[derive(Subcommand)]
enum Cmd {
    #[command(hide = true)]
    Hook,
    #[command(hide = true)]
    Statusline,
    #[command(hide = true)]
    Install {
        #[arg(long)]
        no_statusline: bool,
    },
    #[command(hide = true)]
    Uninstall,
    /// Index new transcript data into the usage history.
    Sync,
    /// Show or set how many days of usage history are kept (default 30).
    Retention(report::RetentionArgs),
    /// Total tokens used since jazari started counting, including days no longer kept.
    Total(report::TotalArgs),
    /// Show your rank and achievement levels, with progress to the next.
    Badges(badges::BadgesArgs),
    /// Print a LinkedIn post of today's work and your rank (--print), and write its PNG card.
    Share(share::ShareArgs),
    /// Summarize token usage (by day, session, skill, project, model, agent).
    Report(report::ReportArgs),
    /// Run a read-only SQL query against the usage history.
    Sql(report::SqlArgs),
    /// Print the usage history database schema.
    Schema,
    /// Show live sessions: status, current activity, context left, and usage limits.
    Status(live::StatusArgs),
    /// Write all jazari data (day counter, history, settings) to one file, to back up or move it.
    Export {
        /// Output file (default: jazari-export-YYYY-MM-DD.json).
        file: Option<PathBuf>,
    },
    /// Merge a `jazari watch export` file into this machine's data; importing twice never double counts.
    Import {
        file: PathBuf,
        /// The file is this machine's own earlier data (a reinstall with the old transcripts still
        /// on disk): days its pre-0.4 totals already count are dropped from the day counter.
        #[arg(long)]
        restore: bool,
    },
    /// How full this session's context is, and the automatic handoff warning (--warn).
    Context(context::ContextArgs),
    /// Free disk space and the levels at which sessions are warned and stopped (--guard on/off).
    Disk(disk::DiskArgs),
    /// Find caches, build output and Docker data that can be rebuilt, and remove them safely.
    Clean(clean::CleanArgs),
    /// Send a running session a message, or stop (--stop) or interrupt (--interrupt) it.
    Tell(channel::TellArgs),
    /// From inside a session: post what it is doing to jazari (--attention: it needs the user).
    Say(channel::SayArgs),
    /// Disk, memory, swap and CPU in one reading, with what to be careful about before heavy work.
    System(system::SystemArgs),
    /// Ask about or refuse risky tool calls: check a command (--check), turn it on/off, list findings.
    Risk(risk::RiskArgs),
    /// Background disk watcher (started by the SessionStart hook), or with --hook the synchronous
    /// hook that checks the disk and risky commands before each tool call and prompt.
    #[command(hide = true)]
    Guard {
        /// Hook entrypoint called by Claude Code on PreToolUse and UserPromptSubmit.
        #[arg(long)]
        hook: bool,
    },
    #[command(hide = true)]
    Update(update::UpdateArgs),
    /// Hold what jazari does on its own (interrupting or ending sessions, cleaning up, updating).
    Pause(act::PauseArgs),
    /// Turn jazari's own actions back on after `jazari watch pause`.
    Resume,
    /// What jazari did, refused or held: messages, stops, cleanups, setting changes and more.
    Actions(act::ActionsArgs),
    #[command(subcommand, hide = true)]
    Jazari(jazari::JazariCmd),
}

/// An environment setting, `JAZARI_<name>`. An empty value counts as unset.
pub fn env(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("JAZARI_{name}")).filter(|v| !v.is_empty())
}

/// The folder `JAZARI_HOME` names, used instead of `~/.jazari`.
pub fn home_override() -> Option<PathBuf> {
    env("HOME").map(PathBuf::from)
}

/// Where jazari keeps its data: `~/.jazari`, or `JAZARI_HOME`. What every coding tool shares (the
/// settings, usage history, prompts and launch files) is at the top; each tool's own files are in
/// a subfolder named after it, and the build part's are in `build/`.
pub fn data_dir() -> PathBuf {
    home_override().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".jazari"))
}

/// Claude Code's own files: the hook event log, status line reports and context-warning markers.
pub fn claude_data_dir() -> PathBuf {
    data_dir().join("claude")
}

pub fn claude_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude")
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn main() -> anyhow::Result<()> {
    let cmd = Cli::parse().top.and_then(Top::into_cmd);
    let quiet = matches!(cmd, Some(Cmd::Hook | Cmd::Statusline | Cmd::Context(_) | Cmd::Guard { .. }));
    act::set_origin(match &cmd {
        None => act::Origin::Dashboard,
        Some(Cmd::Guard { hook: false }) => act::Origin::Watcher,
        Some(Cmd::Hook | Cmd::Statusline | Cmd::Guard { hook: true }) => act::Origin::Hook,
        Some(Cmd::Context(_)) if quiet && std::env::args().any(|a| a == "--hook") => act::Origin::Hook,
        // Anything else is worked out on first use: the user's terminal or a Claude Code session.
        Some(_) => return dispatch(cmd),
    });
    dispatch(cmd)
}

fn dispatch(cmd: Option<Cmd>) -> anyhow::Result<()> {
    match cmd {
        Some(Cmd::Hook) => {
            // Never fail or print: a hook must not disturb the session.
            let _ = hook::run();
            Ok(())
        }
        Some(Cmd::Statusline) => {
            let _ = statusline::run();
            Ok(())
        }
        Some(Cmd::Install { no_statusline }) => {
            act::run(&act::Action::new(act::Kind::Install, "~/.claude/settings.json").detail("install"), || install::install(!no_statusline))
        }
        Some(Cmd::Uninstall) => act::run(&act::Action::new(act::Kind::Install, "~/.claude/settings.json").detail("uninstall"), install::uninstall),
        Some(Cmd::Sync) => report::sync_now(),
        Some(Cmd::Retention(args)) => report::retention(args),
        Some(Cmd::Total(args)) => report::total(args),
        Some(Cmd::Badges(args)) => badges::run(args),
        Some(Cmd::Share(args)) => share::run(args),
        Some(Cmd::Report(args)) => report::report(args),
        Some(Cmd::Sql(args)) => report::sql(args),
        Some(Cmd::Schema) => {
            report::schema();
            Ok(())
        }
        Some(Cmd::Status(args)) => live::status(args),
        Some(Cmd::Context(args)) => context::run(args),
        Some(Cmd::Export { file }) => portable::export(file),
        Some(Cmd::Import { file, restore }) => portable::import(&file, restore),
        Some(Cmd::Disk(args)) => disk::run(args),
        Some(Cmd::System(args)) => system::run(args),
        Some(Cmd::Tell(args)) => channel::run_tell(args),
        Some(Cmd::Say(args)) => channel::run_say(args),
        Some(Cmd::Clean(args)) => clean::run(args),
        Some(Cmd::Risk(args)) => risk::run(args),
        Some(Cmd::Guard { hook }) => {
            if !hook {
                return watch::run();
            }
            // Never fail or print anything but the verdict: a hook must not disturb the session, so
            // even a panic prints nothing and exits 0.
            std::panic::set_hook(Box::new(|_| {}));
            if let Ok(Some(out)) = std::panic::catch_unwind(disk::hook) {
                println!("{out}");
            }
            Ok(())
        }
        Some(Cmd::Update(args)) => update::run(args),
        Some(Cmd::Pause(args)) => act::run_pause(args),
        Some(Cmd::Resume) => act::run_resume(),
        Some(Cmd::Actions(args)) => act::run_actions(args),
        Some(Cmd::Jazari(cmd)) => jazari::run(cmd),
        None => app::run(),
    }
}

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
mod hook;
mod install;
mod jsonl;
mod live;
mod migrate;
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
mod share_history;
mod share_view;
mod shot;
mod statusline;
mod store;
mod system;
mod term;
mod transcript;
mod ui;
mod update;
mod usage;
mod watch;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// A live dashboard for every Claude Code session on this machine.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Hook entrypoint called by Claude Code (reads hook JSON on stdin).
    Hook,
    /// Status line entrypoint called by Claude Code (reads status JSON on stdin).
    Statusline,
    /// Register marsad hooks (and status line) in ~/.claude/settings.json.
    Install {
        /// Don't install the status line; context size is then estimated from transcripts.
        #[arg(long)]
        no_statusline: bool,
    },
    /// Remove marsad hooks from ~/.claude/settings.json.
    Uninstall,
    /// Index new transcript data into the usage history.
    Sync,
    /// Show or set how many days of usage history are kept (default 30).
    Retention(report::RetentionArgs),
    /// Total tokens used since marsad started counting, including days no longer kept.
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
    /// Write all marsad data (day counter, history, settings) to one file, to back up or move it.
    Export {
        /// Output file (default: marsad-export-YYYY-MM-DD.json).
        file: Option<PathBuf>,
    },
    /// Merge a `marsad export` file into this machine's data; importing twice never double counts.
    Import {
        file: PathBuf,
        /// The file is this machine's own earlier data (a reinstall with the old transcripts still
        /// on disk): days its pre-0.4 totals already count are dropped from the day counter.
        #[arg(long)]
        restore: bool,
    },
    /// How full this session's context is, and the automatic handoff warning (--warn).
    Context(context::ContextArgs),
    /// List the rules a Claude Code session can start with, or print the command that loads a chosen set.
    Rules(rules::RulesArgs),
    /// Write, edit, copy or delete your own prompts: `new` drafts one from a rough idea with Claude.
    #[command(subcommand)]
    Prompt(prompt_gen::PromptCmd),
    /// Print the `claude` command that starts a session with the coordination rules (`marsad rules coordination`).
    Coordination(rules::CoordinationArgs),
    /// Free disk space and the levels at which sessions are warned and stopped (--guard on/off).
    Disk(disk::DiskArgs),
    /// Find caches, build output and Docker data that can be rebuilt, and remove them safely.
    Clean(clean::CleanArgs),
    /// Send a running session a message, or stop (--stop) or interrupt (--interrupt) it.
    Tell(channel::TellArgs),
    /// From inside a session: post what it is doing to marsad (--attention: it needs the user).
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
    /// Update marsad to the latest release (or turn automatic updates on/off with --auto).
    Update(update::UpdateArgs),
}

/// The folder `MARSAD_HOME` names, which replaces both `data_dir` and `legacy_dir`.
pub fn home_override() -> Option<PathBuf> {
    std::env::var_os("MARSAD_HOME").filter(|d| !d.is_empty()).map(PathBuf::from)
}

/// Where marsad keeps its data: `~/.marsad`, or `MARSAD_HOME`. What every coding tool shares (the
/// settings, usage history, prompts and launch files) is at the top; each tool's own files are in
/// a subfolder named after it.
pub fn data_dir() -> PathBuf {
    home_override().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".marsad"))
}

/// Claude Code's own files: the hook event log, status line reports and context-warning markers.
pub fn claude_data_dir() -> PathBuf {
    data_dir().join("claude")
}

/// Where versions before 0.11 kept the data (`~/.claude/marsad`, or `MARSAD_HOME`). It still holds
/// the files at the paths they printed, which are kept current.
pub fn legacy_dir() -> PathBuf {
    home_override().unwrap_or_else(|| claude_dir().join("marsad"))
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
    let cmd = Cli::parse().cmd;
    // Before anything reads or writes data. The hooks and the status line must stay quiet.
    let quiet = matches!(cmd, Some(Cmd::Hook | Cmd::Statusline | Cmd::Context(_) | Cmd::Guard { .. }));
    // Claude Code waits on the guard hook before every tool call: it never copies data, which can
    // take longer than the hook may. Any other marsad run does that.
    let sync_hook = matches!(cmd, Some(Cmd::Guard { hook: true }));
    let outcome = if sync_hook { migrate::Outcome::Idle } else { migrate::from_legacy(if quiet { migrate::HOOK_WAIT } else { migrate::WAIT }, !quiet) };
    match outcome {
        migrate::Outcome::Busy if !quiet => {
            anyhow::bail!("marsad's data is being copied to {} by another marsad; try again in a moment", data_dir().display())
        }
        _ => {}
    }
    // The dashboard shows it in its footer.
    if !quiet && cmd.is_some()
        && let Some(note) = migrate::take_notice()
    {
        eprintln!("{note}");
    }
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
        Some(Cmd::Install { no_statusline }) => install::install(!no_statusline),
        Some(Cmd::Uninstall) => install::uninstall(),
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
        Some(Cmd::Rules(args)) => rules::run(args),
        Some(Cmd::Prompt(cmd)) => prompt_gen::run(cmd),
        Some(Cmd::Coordination(args)) => rules::run_coordination(args),
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
        None => app::run(),
    }
}

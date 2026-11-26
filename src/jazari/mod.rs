//! jazari: the part of marsad that runs an AI software company.
//! - Telegram: pairing, the bridge that takes commands and forwards sessions' `--attention`
//!   notes, and `notify` for scripts.
//! - The engine: products, a task queue, and a supervisor that runs each task as a headless
//!   Claude worker in the product's container. It sleeps whenever the subscription's usage windows
//!   call for it.

mod audit;
mod bridge;
mod limits;
mod sandbox;
pub mod secrets;
mod store;
mod supervisor;
pub mod telegram;
mod worker;

use clap::Subcommand;
use std::path::PathBuf;
use telegram::{Bot, Curl};

#[derive(Subcommand)]
pub enum JazariCmd {
    /// Connect your Telegram bot: pair a chat, send a test message, check or remove the pairing.
    #[command(subcommand)]
    Telegram(TelegramCmd),
    /// Take commands from the paired Telegram chat and forward sessions' --attention notes (runs until stopped).
    Bridge,
    /// Send a message to the paired Telegram chat (credentials in it are redacted).
    Notify {
        /// The message; read from stdin when left out.
        message: Vec<String>,
    },
    /// Add or list the products jazari works on (each gets its own container).
    #[command(subcommand)]
    Product(ProductCmd),
    /// Queue a task for a product's workers.
    Task {
        product: String,
        /// What to do, in one line.
        title: Vec<String>,
        /// The persona that does it.
        #[arg(long, default_value = "engineer")]
        role: String,
        /// Higher runs first.
        #[arg(long, default_value_t = 0)]
        priority: i64,
        /// More detail for the worker.
        #[arg(long, default_value = "")]
        brief: String,
    },
    /// List tasks: running and blocked first, then queued, then finished.
    Tasks {
        #[arg(long)]
        product: Option<String>,
        #[arg(short = 'n', long, default_value_t = 30)]
        limit: usize,
    },
    /// Show or set your Claude plan (pro, max5, max20); it sets how hard jazari works and what it keeps for you.
    Plan { plan: Option<String> },
    /// The last usage reading, and whether jazari would start a task now.
    Limits,
    /// Run the supervisor: take queued tasks one at a time until stopped (--once: at most one task).
    Run {
        #[arg(long)]
        once: bool,
    },
    /// Check that the audit record is whole.
    Audit,
}

#[derive(Subcommand)]
pub enum ProductCmd {
    /// Add a product: a name (lowercase, digits, dashes) and its git repo folder.
    Add { name: String, repo: std::path::PathBuf },
    /// List the products.
    List,
}

#[derive(Subcommand)]
pub enum TelegramCmd {
    /// Show a one-time code and wait for it in a private chat with the bot; that chat becomes the only one obeyed.
    Pair,
    /// Send a test message to the paired chat.
    Test,
    /// Whether the bot token and a paired chat are set.
    Status,
    /// Forget the paired chat.
    Unpair,
}

/// jazari's own files: `~/.marsad/jazari`.
pub fn dir() -> PathBuf {
    crate::data_dir().join("jazari")
}

fn bot() -> anyhow::Result<Bot<Curl>> {
    let token = secrets::get(secrets::TELEGRAM_TOKEN)
        .ok_or_else(|| anyhow::anyhow!("no Telegram bot token: {}", secrets::how_to_add(secrets::TELEGRAM_TOKEN)))?;
    Ok(Bot { transport: Curl::new(token) })
}

fn chat() -> anyhow::Result<i64> {
    secrets::get(secrets::TELEGRAM_CHAT)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no Telegram chat is paired; run `marsad jazari telegram pair`"))
}

pub fn run(cmd: JazariCmd) -> anyhow::Result<()> {
    match cmd {
        JazariCmd::Telegram(TelegramCmd::Pair) => bridge::pair(&bot()?, &dir()),
        JazariCmd::Telegram(TelegramCmd::Test) => {
            bot()?.send(chat()?, "Test message from jazari: the bridge can reach you.")?;
            println!("Sent.");
            Ok(())
        }
        JazariCmd::Telegram(TelegramCmd::Status) => {
            let token = secrets::get(secrets::TELEGRAM_TOKEN).is_some();
            println!("Bot token: {}", if token { "set" } else { "missing" });
            if token {
                println!("Bot: @{}", bot()?.username()?);
            }
            println!("Paired chat: {}", if chat().is_ok() { "yes" } else { "no (`marsad jazari telegram pair`)" });
            Ok(())
        }
        JazariCmd::Telegram(TelegramCmd::Unpair) => {
            secrets::delete(secrets::TELEGRAM_CHAT)?;
            println!("Unpaired: no chat can control jazari until you pair again.");
            Ok(())
        }
        JazariCmd::Bridge => bridge::run(&bot()?, chat()?, &dir()),
        JazariCmd::Notify { message } => {
            let mut text = message.join(" ");
            if text.trim().is_empty() {
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            }
            anyhow::ensure!(!text.trim().is_empty(), "nothing to send");
            bot()?.send(chat()?, text.trim())
        }
        JazariCmd::Product(ProductCmd::Add { name, repo }) => {
            let repo = std::fs::canonicalize(&repo).map_err(|e| anyhow::anyhow!("{}: {e}", repo.display()))?;
            anyhow::ensure!(repo.join(".git").exists(), "{} isn't a git repo", repo.display());
            open()?.add_product(&name, &repo.to_string_lossy(), crate::now())?;
            println!("Added {name} ({}). Its container is created at its first task.", repo.display());
            Ok(())
        }
        JazariCmd::Product(ProductCmd::List) => {
            for p in open()?.products()? {
                println!("{:<20} {}", p.name, p.repo);
            }
            Ok(())
        }
        JazariCmd::Task { product, title, role, priority, brief } => {
            let id = open()?.add_task(&product, &role, &title.join(" "), &brief, priority, None, crate::now())?;
            println!("Queued task #{id} for {product}.");
            Ok(())
        }
        JazariCmd::Tasks { product, limit } => {
            let store = open()?;
            for t in store.tasks(product.as_deref(), limit)? {
                let note = if t.summary.is_empty() { String::new() } else { format!(" — {}", crate::hook::truncate(&t.summary, 70)) };
                println!("#{:<4} {:<8} {:<10} {:<14} {}{note}", t.id, t.status, t.product, crate::hook::truncate(&t.role, 14), t.title);
            }
            let counts: Vec<String> = store.counts()?.iter().map(|(s, n)| format!("{n} {s}")).collect();
            println!("{}", if counts.is_empty() { "No tasks yet.".to_string() } else { counts.join(" · ") });
            Ok(())
        }
        JazariCmd::Plan { plan: None } => {
            let p = limits::plan();
            let prof = p.profile();
            println!("Plan: {} — keeps {:.0}% of the 5-hour and {:.0}% of the 7-day window for you, up to {} turns per task.", p.label(), prof.reserve_5h, prof.reserve_7d, prof.max_turns);
            Ok(())
        }
        JazariCmd::Plan { plan: Some(p) } => {
            let plan = limits::Plan::parse(&p).ok_or_else(|| anyhow::anyhow!("expected pro, max5 or max20"))?;
            limits::set_plan(plan)?;
            println!("Plan set to {}.", plan.label());
            Ok(())
        }
        JazariCmd::Limits => {
            let store = open()?;
            let l: limits::Limits = store.get("limits")?.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let show = |name: &str, w: Option<limits::Window>| match w {
                Some(w) => println!("{name}: {:.0}% used{}", w.used_pct, if w.allowed { "" } else { " (refused)" }),
                None => println!("{name}: no reading yet"),
            };
            show("5-hour", l.five_hour);
            show("7-day ", l.seven_day);
            match limits::decide(&l, &limits::plan().profile(), crate::now()) {
                limits::Decision::Go => println!("jazari would start a task now."),
                limits::Decision::Sleep { reason, .. } => println!("jazari would wait: {reason}."),
            }
            Ok(())
        }
        JazariCmd::Run { once } => run_supervisor(once),
        JazariCmd::Audit => match audit::verify(&dir().join("audit.jsonl")) {
            Ok(n) => {
                println!("The audit record is whole: {n} entries.");
                Ok(())
            }
            Err(e) => anyhow::bail!("the audit record was changed: {e}"),
        },
    }
}

fn open() -> anyhow::Result<store::Store> {
    store::Store::open(&dir().join("jazari.db"))
}

/// Tells the founder on Telegram when it is set up, and always prints it.
fn tell_founder(text: &str) {
    println!("{text}");
    if let (Ok(bot), Ok(chat)) = (bot(), chat())
        && let Err(e) = bot.send(chat, text)
    {
        eprintln!("[jazari] couldn't reach Telegram: {e}");
    }
}

fn run_supervisor(once: bool) -> anyhow::Result<()> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("supervisor.lock"))?;
    lock.try_lock().map_err(|_| anyhow::anyhow!("a jazari supervisor is already running"))?;
    let token = secrets::get(secrets::CLAUDE_TOKEN)
        .ok_or_else(|| anyhow::anyhow!("no Claude login token for the workers: run `claude setup-token`, then {}", secrets::how_to_add(secrets::CLAUDE_TOKEN)))?;
    let store = open()?;
    let requeued = store.requeue_running(crate::now())?;
    if requeued > 0 {
        println!("{requeued} task(s) left running by an earlier supervisor went back in the queue.");
    }
    let runner = worker::Docker { token };
    let audit_path = dir.join("audit.jsonl");
    let now = crate::now;
    let paused = crate::act::paused;
    let ensure = |p: &str, r: &str| sandbox::ensure(p, r);
    let audit = |kind: &str, data: serde_json::Value| {
        if let Err(e) = audit::append(&audit_path, crate::now(), kind, data) {
            eprintln!("[jazari] couldn't write the audit record: {e}");
        }
    };
    let profile = limits::plan().profile();
    let deps = supervisor::Deps { store: &store, runner: &runner, profile, now: &now, paused: &paused, ensure: &ensure, notify: &tell_founder, audit: &audit };
    println!("jazari supervisor on ({}). Ctrl-C stops it; a task in progress goes back in the queue next start.", profile.plan.label());
    let mut errors = 0u32;
    loop {
        let wait = match supervisor::tick(&deps) {
            Ok(supervisor::Tick::Ran { .. }) => {
                errors = 0;
                if once {
                    return Ok(());
                }
                0.0
            }
            Ok(supervisor::Tick::Idle) if once => {
                println!("No queued tasks.");
                return Ok(());
            }
            Ok(supervisor::Tick::Sleeping { until }) if once => {
                println!("Waiting for usage to free up until {until:.0}.");
                return Ok(());
            }
            Ok(supervisor::Tick::Idle | supervisor::Tick::Paused) => 60.0,
            Ok(supervisor::Tick::Sleeping { until }) => (until - crate::now()).clamp(5.0, 300.0),
            Err(e) => {
                errors += 1;
                if errors == 3 {
                    tell_founder(&format!("⚠️ jazari supervisor keeps failing: {e}"));
                }
                anyhow::ensure!(!once, "{e}");
                eprintln!("[jazari] {e}");
                (30.0 * f64::from(errors)).min(600.0)
            }
        };
        std::thread::sleep(std::time::Duration::from_secs_f64(wait));
    }
}

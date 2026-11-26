//! jazari: the part of marsad that runs an AI software company.
//! - Telegram: pairing, the bridge that takes commands and forwards sessions' `--attention`
//!   notes, and `notify` for scripts.
//! - The engine: products, a task queue, and a supervisor that runs each task as a headless
//!   Claude worker in the product's container. It sleeps whenever the subscription's usage windows
//!   call for it.

mod audit;
mod bridge;
mod evals;
mod git;
mod limits;
mod personas;
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
        /// Start even if the persona checks (`marsad jazari evals`) block.
        #[arg(long)]
        skip_evals: bool,
    },
    /// Check the team after a change to personas, rubrics or the evaluator: lint for free, then
    /// calibrate the judges with known-good and known-bad cases, with effort matched to the change.
    Evals {
        /// Check everything at full effort, as if nothing had been checked before.
        #[arg(long)]
        all: bool,
        /// Show what changed and what would run, without running it.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Check that the audit record is whole.
    Audit,
    /// Start a product from an idea: a new git repo, and a task for the Chief of Staff to plan it.
    New {
        name: String,
        /// The idea, in your own words.
        idea: Vec<String>,
    },
    /// Give an existing product a new idea for the Chief of Staff to plan.
    Idea { product: String, idea: Vec<String> },
    /// Answer a blocked task's question; it goes back in the queue with your answer.
    Answer { task: i64, answer: Vec<String> },
    /// Approve a task's deploy, spend or contact request.
    Approve { task: i64 },
    /// Deny a task's deploy, spend or contact request.
    Deny { task: i64 },
    /// List the roles workers can take, with their model tier and rubric.
    Personas {
        /// Copy the built-in personas.md and rubrics.md to ~/.marsad/jazari/ to edit; jazari uses those copies.
        #[arg(long)]
        export: bool,
    },
    /// How each role's work fares at each gate: checks, evaluator, reviews, the merge queue.
    Quality,
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
            let title = title.join(" ");
            let role = personas::resolve(&role);
            let task = store::NewTask { product: &product, role: &role, title: &title, brief: &brief, priority, ..Default::default() };
            let id = open()?.add_task(&task, crate::now())?;
            println!("Queued task #{id} for {product}.");
            Ok(())
        }
        JazariCmd::Tasks { product, limit } => {
            let store = open()?;
            for t in store.tasks(product.as_deref(), limit)? {
                let waiting = t.question.as_deref().or(t.approval.as_deref()).map(|q| format!(" — waits for you: {}", crate::hook::truncate(q, 70)));
                let note = waiting.unwrap_or_else(|| if t.summary.is_empty() { String::new() } else { format!(" — {}", crate::hook::truncate(&t.summary, 70)) });
                println!("#{:<4} {:<8} {:<10} {:<16} {}{note}", t.id, t.status, t.product, crate::hook::truncate(&t.role, 16), t.title);
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
        JazariCmd::Run { once, skip_evals } => run_supervisor(once, skip_evals),
        JazariCmd::Evals { all, dry_run, json } => {
            let store = open()?;
            let roster = personas::all();
            if dry_run {
                let plan = evals::plan(&store, &roster, all)?;
                for (item, level) in &plan.changed {
                    println!("changed: {item} ({level:?})");
                }
                for (judge, group, level) in &plan.calls {
                    let how = if *level == evals::Level::Major { "every case, production model, high effort" } else { "bad cases, small model, low effort" };
                    println!("would run: {judge} on the {group} cases ({how})");
                }
                if plan.calls.is_empty() {
                    println!("No calibration calls needed.");
                }
                return Ok(());
            }
            let report = evals::check(&store, &roster, all, limits::plan().profile().build_model, &evals::claude_judge)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&evals::report_json(&report))?);
            } else {
                println!("{}", report.text());
            }
            anyhow::ensure!(!report.blocking(), "the team's checks block the supervisor");
            Ok(())
        }
        JazariCmd::Audit => match audit::verify(&dir().join("audit.jsonl")) {
            Ok(n) => {
                println!("The audit record is whole: {n} entries.");
                Ok(())
            }
            Err(e) => anyhow::bail!("the audit record was changed: {e}"),
        },
        JazariCmd::New { name, idea } => {
            let (repo, id) = create_product(&open()?, &name, &idea.join(" "))?;
            println!("Created {name} in {}. Task #{id}: the Chief of Staff turns the idea into a plan.", repo.display());
            Ok(())
        }
        JazariCmd::Idea { product, idea } => {
            let id = queue_idea(&open()?, &product, &idea.join(" "))?;
            println!("Queued task #{id}: the Chief of Staff turns the idea into a plan and the first tasks.");
            Ok(())
        }
        JazariCmd::Answer { task, answer } => {
            open()?.reply(task, &answer.join(" "), crate::now())?;
            println!("Task #{task} is back in the queue with your answer.");
            Ok(())
        }
        JazariCmd::Approve { task } => {
            println!("{}", decide(&open()?, task, true)?);
            Ok(())
        }
        JazariCmd::Deny { task } => {
            println!("{}", decide(&open()?, task, false)?);
            Ok(())
        }
        JazariCmd::Quality => {
            let store = open()?;
            let rows = store.quality()?;
            if rows.is_empty() {
                println!("No judgments yet.");
            }
            println!("{:<26} {:<9} {:>6} {:>7} {:>6}", "ROLE", "GATE", "TIMES", "PASSED", "SCORE");
            for (role, stage, n, passed, score) in rows {
                let pct = if n > 0 { passed * 100 / n } else { 0 };
                let score = score.map(|s| format!("{s:.1}")).unwrap_or_else(|| "-".into());
                let up = if stage == "eval" && store.get(&format!("upgrade:{role}"))?.is_some() { "  ↑ moved to the lead model" } else { "" };
                println!("{role:<26} {stage:<9} {n:>6} {pct:>6}% {score:>6}{up}");
            }
            Ok(())
        }
        JazariCmd::Personas { export } => {
            if export {
                std::fs::create_dir_all(dir())?;
                for (name, text) in [("personas.md", personas::ROSTER), ("rubrics.md", personas::RUBRICS)] {
                    let path = dir().join(name);
                    anyhow::ensure!(!path.exists(), "{} already exists; edit it, or delete it to go back to the built-in one", path.display());
                    std::fs::write(&path, text)?;
                    println!("Wrote {}", path.display());
                }
                println!("jazari now uses these copies. `marsad jazari evals` checks your changes, with effort matched to their size.");
                return Ok(());
            }
            for p in personas::all() {
                println!("{:<26} {:<6} {:<9} {}", p.key, format!("{:?}", p.tier).to_lowercase(), p.rubric, p.title);
            }
            Ok(())
        }
    }
}

/// What the Chief of Staff is asked to do with a new idea.
const INTAKE: &str = "This is a new idea from the founder. Turn it into a plan the team can build:
1. Write or update company/VISION.md, company/ROADMAP.md (first milestone small, shippable and testable),
   company/DECISIONS/ (an ADR for the architecture), company/THREAT-MODEL.md and company/SECURITY.md.
   For an existing product, read them first and change only what the idea changes.
2. Commit them.
3. Return the first milestone's tasks as follow-ups, in build order. Each task is small, concrete and
   testable, has the right role, and has a brief with acceptance criteria.
If the idea is unclear, make the simplest useful reading, write the assumption in VISION.md, and go on.

The founder's idea:
";

pub(crate) fn queue_idea(store: &store::Store, product: &str, idea: &str) -> anyhow::Result<i64> {
    anyhow::ensure!(!idea.trim().is_empty(), "describe the idea");
    let brief = format!("{INTAKE}{}", idea.trim());
    let title = format!("Plan: {}", crate::hook::truncate(idea.lines().next().unwrap_or(idea), 80));
    store.add_task(&store::NewTask { product, role: "chief-of-staff", title: &title, brief: &brief, priority: 100, ..Default::default() }, crate::now())
}

/// Where new products' repos go: `jazari.products_dir`, or `~/jazari-products`.
fn products_dir() -> PathBuf {
    crate::settings::get("jazari")
        .and_then(|j| j["products_dir"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("jazari-products"))
}

/// Makes a git repo for a new product, registers it and queues the idea.
pub(crate) fn create_product(store: &store::Store, name: &str, idea: &str) -> anyhow::Result<(PathBuf, i64)> {
    anyhow::ensure!(store::valid_name(name), "a product name uses lowercase letters, digits and dashes: {name:?}");
    anyhow::ensure!(store.product(name)?.is_none(), "there is already a product called {name}");
    anyhow::ensure!(!idea.trim().is_empty(), "describe the idea");
    let repo = products_dir().join(name);
    anyhow::ensure!(!repo.exists(), "{} already exists", repo.display());
    std::fs::create_dir_all(&repo)?;
    std::fs::write(repo.join("README.md"), format!("# {name}\n\n{}\n", idea.trim()))?;
    std::fs::write(repo.join(".gitignore"), ".jazari/\n.env\n.env.*\n")?;
    let git = |args: &[&str]| -> anyhow::Result<()> {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=jazari", "-c", "user.email=jazari@localhost", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()?
            .success();
        anyhow::ensure!(ok, "git {} failed", args.join(" "));
        Ok(())
    };
    git(&["init", "-q", "-b", "main"])?;
    git(&["add", "README.md", ".gitignore"])?;
    git(&["commit", "-q", "-m", "chore: start the product"])?;
    let repo = std::fs::canonicalize(&repo)?;
    store.add_product(name, &repo.to_string_lossy(), crate::now())?;
    Ok((repo.clone(), queue_idea(store, name, idea)?))
}

/// Approves or denies a task's request; the task goes back in the queue with the decision.
pub(crate) fn decide(store: &store::Store, task: i64, approve: bool) -> anyhow::Result<String> {
    let t = store.task(task)?.ok_or_else(|| anyhow::anyhow!("no task #{task}"))?;
    let what = t.approval.clone().ok_or_else(|| anyhow::anyhow!("task #{task} isn't waiting for an approval"))?;
    let reply = if approve {
        "Approved. Production, payment and outreach credentials are not in your container: prepare everything, write the exact steps in company/RUNBOOKS/, and report what the founder must run."
    } else {
        "Denied. Don't do it. Finish the task without it, or explain the alternative."
    };
    store.reply(task, reply, crate::now())?;
    let verdict = if approve { "Approved" } else { "Denied" };
    Ok(format!("{verdict}: #{task} {what}"))
}

/// The tasks running, waiting for the founder, and queued next, for Telegram.
pub(crate) fn task_summary(store: &store::Store) -> anyhow::Result<String> {
    let tasks = store.tasks(None, 200)?;
    let mut out = Vec::new();
    let line = |t: &store::Task| format!("#{} {} · {} · {}", t.id, t.product, t.role, crate::hook::truncate(&t.title, 70));
    let running: Vec<_> = tasks.iter().filter(|t| t.status == "running").map(line).collect();
    let waiting: Vec<_> = tasks
        .iter()
        .filter(|t| t.status == "blocked")
        .map(|t| format!("{}\n   ↳ {}", line(t), crate::hook::truncate(t.question.as_deref().or(t.approval.as_deref()).unwrap_or(&t.summary), 120)))
        .collect();
    let queued: Vec<_> = tasks
        .iter()
        .filter(|t| t.status == "queued")
        .take(8)
        .map(|t| format!("{}{}", line(t), t.after_task.map(|a| format!(" (after #{a})")).unwrap_or_default()))
        .collect();
    for (head, list) in [("Running", running), ("Waiting for you", waiting), ("Next up", queued)] {
        if !list.is_empty() {
            out.push(format!("{head}:\n{}", list.join("\n")));
        }
    }
    let counts: Vec<String> = store.counts()?.iter().map(|(s, n)| format!("{n} {s}")).collect();
    out.push(if counts.is_empty() { "No tasks yet. Send /new <name> <idea> to start.".into() } else { counts.join(" · ") });
    Ok(out.join("\n\n"))
}

fn open() -> anyhow::Result<store::Store> {
    store::Store::open(&dir().join("jazari.db"))
}

/// Tells the founder on Telegram when it is set up, and always prints it.
fn tell_founder(text: &str, buttons: &[supervisor::Button]) {
    println!("{text}");
    if let (Ok(bot), Ok(chat)) = (bot(), chat())
        && let Err(e) = bot.send_with_buttons(chat, text, buttons)
    {
        eprintln!("[jazari] couldn't reach Telegram: {e}");
    }
}

fn run_supervisor(once: bool, skip_evals: bool) -> anyhow::Result<()> {
    let dir = dir();
    std::fs::create_dir_all(&dir)?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("supervisor.lock"))?;
    lock.try_lock().map_err(|_| anyhow::anyhow!("a jazari supervisor is already running"))?;
    let token = secrets::get(secrets::CLAUDE_TOKEN)
        .ok_or_else(|| anyhow::anyhow!("no Claude login token for the workers: run `claude setup-token`, then {}", secrets::how_to_add(secrets::CLAUDE_TOKEN)))?;
    let store = open()?;
    // The team's own checks: free when nothing changed, and matched to the change's size when
    // something did.
    let report = evals::check(&store, &personas::all(), false, limits::plan().profile().build_model, &evals::claude_judge)?;
    if !report.plan.changed.is_empty() || report.blocking() {
        println!("{}", report.text());
    }
    if report.blocking() && !skip_evals {
        tell_founder("⛔ jazari won't start: a persona or judge check failed. Run `marsad jazari evals` for details.", &[]);
        anyhow::bail!("the team's checks block the supervisor; fix them, or start with --skip-evals");
    }
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
    let deps = supervisor::Deps { store: &store, runner: &runner, repo: &supervisor::Git, profile, now: &now, paused: &paused, ensure: &ensure, notify: &tell_founder, audit: &audit };
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
                    tell_founder(&format!("⚠️ jazari supervisor keeps failing: {e}"), &[]);
                }
                anyhow::ensure!(!once, "{e}");
                eprintln!("[jazari] {e}");
                (30.0 * f64::from(errors)).min(600.0)
            }
        };
        std::thread::sleep(std::time::Duration::from_secs_f64(wait));
    }
}

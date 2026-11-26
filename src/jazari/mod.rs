//! jazari: the part of jazari that runs an AI software company.
//! - Telegram: pairing, the bridge that takes commands and forwards sessions' `--attention`
//!   notes, and `notify` for scripts.
//! - The engine: products, a task queue, and a supervisor that runs each task as a headless
//!   Claude worker in the product's container. It sleeps whenever the subscription's usage windows
//!   call for it.

mod audit;
mod bridge;
mod cadence;
mod card;
mod evals;
mod git;
mod limits;
mod llm;
mod personas;
mod profile;
mod sandbox;
pub mod secrets;
mod service;
mod store;
mod supervisor;
pub mod telegram;
mod worker;

use clap::Subcommand;
use std::path::PathBuf;
use telegram::{Bot, Curl};

#[derive(Subcommand)]
pub enum JazariCmd {
    /// Save a handoff prompt (stdin or --file) and open a new terminal where a fresh session starts from it.
    Handoff(crate::handoff::HandoffArgs),
    /// List the rules a Claude Code session can start with, or print the command that loads a chosen set.
    Rules(crate::rules::RulesArgs),
    /// Write, edit, copy or delete your own prompts: `new` drafts one from a rough idea with Claude.
    #[command(subcommand)]
    Prompt(crate::prompt_gen::PromptCmd),
    /// List, write, edit, copy, delete or install skills: `new` writes one from a rough idea with Claude.
    #[command(subcommand)]
    Skill(crate::prompt_gen::SkillCmd),
    /// Print the `claude` command that starts a session with the coordination rules.
    Coordination(crate::rules::CoordinationArgs),
    /// Connect your Telegram bot: pair a chat, send a test message, check or remove the pairing.
    #[command(subcommand)]
    Telegram(TelegramCmd),
    /// Take commands from the paired Telegram chat and forward sessions' --attention notes (runs until stopped).
    Bridge,
    /// Run the bridge or the supervisor in the background on macOS: it starts at login and again
    /// after a crash. Nothing runs this way until you install it.
    #[command(subcommand)]
    Service(ServiceCmd),
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
        /// Start even if the persona checks (`jazari build evals`) block.
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
        /// Keep improving it after your tasks are done (see `mode`).
        #[arg(long)]
        autonomous: bool,
    },
    /// Set how a product works: `normal` stops when your tasks are done; `autonomous` keeps
    /// improving it (bugs, security, UX, performance, docs, next improvements).
    Mode { product: String, mode: String },
    /// Tell the team something about how you like the work done; jazari learns it after a few times.
    Feedback { product: String, text: Vec<String> },
    /// Set a preference now: a product (or `all`), an area (design, copy, code, product, process,
    /// communication) and the rule.
    Prefer { product: String, area: String, rule: Vec<String> },
    /// Drop a preference (it stays in the history).
    Forget { id: i64 },
    /// List what jazari has learned about how you like the work done.
    Prefs {
        #[arg(long)]
        product: Option<String>,
    },
    /// Accept a proposal from jazari (an automatic check for a recurring problem).
    Accept { id: i64 },
    /// Ignore a proposal from jazari.
    Ignore { id: i64 },
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
        /// Copy the built-in personas.md and rubrics.md to ~/.jazari/build/ to edit; jazari uses those copies.
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

#[derive(Subcommand)]
pub enum ServiceCmd {
    /// Install and start services (default: the bridge only; the supervisor spends usage, so name it).
    Install {
        #[arg(value_enum)]
        parts: Vec<service::Part>,
    },
    /// Stop and remove services (default: both). The logs are kept.
    Uninstall {
        #[arg(value_enum)]
        parts: Vec<service::Part>,
    },
    /// Whether each service is installed and running, and where its log is.
    Status,
}

/// jazari's own files: `~/.jazari/build`.
pub fn dir() -> PathBuf {
    crate::data_dir().join("build")
}

fn bot() -> anyhow::Result<Bot<Curl>> {
    let token = secrets::get(secrets::TELEGRAM_TOKEN)
        .ok_or_else(|| anyhow::anyhow!("no Telegram bot token: {}", secrets::how_to_add(secrets::TELEGRAM_TOKEN)))?;
    Ok(Bot { transport: Curl::new(token) })
}

fn chat() -> anyhow::Result<i64> {
    secrets::get(secrets::TELEGRAM_CHAT)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no Telegram chat is paired; run `jazari build telegram pair`"))
}

pub fn run(cmd: JazariCmd) -> anyhow::Result<()> {
    match cmd {
        JazariCmd::Handoff(a) => crate::handoff::run(a),
        JazariCmd::Rules(a) => crate::rules::run(a),
        JazariCmd::Prompt(c) => crate::prompt_gen::run(c),
        JazariCmd::Skill(c) => crate::prompt_gen::run_skill(c),
        JazariCmd::Coordination(a) => crate::rules::run_coordination(a),
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
            println!("Paired chat: {}", if chat().is_ok() { "yes" } else { "no (`jazari build telegram pair`)" });
            Ok(())
        }
        JazariCmd::Telegram(TelegramCmd::Unpair) => {
            let removed = secrets::delete(secrets::TELEGRAM_CHAT)?;
            let env = secrets::env_name(secrets::TELEGRAM_CHAT);
            if std::env::var(&env).is_ok_and(|v| !v.trim().is_empty()) {
                println!("{env} is set, so that chat is still paired: unset it in the environment that runs jazari.");
            } else if removed {
                println!("Unpaired: no chat can control jazari until you pair again.");
            } else {
                println!("No paired chat was stored here, so nothing changed.");
            }
            Ok(())
        }
        JazariCmd::Bridge => bridge::run(&bot()?, chat()?, &dir()),
        JazariCmd::Service(cmd) => {
            let e = service::Env::real()?;
            let print = &mut |line: String| println!("{line}");
            match cmd {
                ServiceCmd::Install { parts } => service::install(&e, if parts.is_empty() { &[service::Part::Bridge] } else { &parts }, print),
                ServiceCmd::Uninstall { parts } => service::uninstall(&e, if parts.is_empty() { &service::Part::ALL } else { &parts }, print),
                ServiceCmd::Status => service::status(&e, print),
            }
        }
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
        JazariCmd::Audit => match audit::check(&dir().join("audit.jsonl")) {
            Ok(w) => {
                if let Some(warning) = &w.warning {
                    println!("⚠️ Check by hand: {warning}.");
                }
                println!("The audit record is whole: {} entries.", w.count);
                println!("The supervisor sends you its count and head on Telegram when it starts, when it stops and every {} entries.", audit::EVERY);
                println!("If the last such message there isn't this line, the record was changed:\n\n    {}", audit::head_text(w.count, &w.head));
                Ok(())
            }
            Err(e) => anyhow::bail!("the audit record was changed: {e}"),
        },
        JazariCmd::New { name, idea, autonomous } => {
            let store = open()?;
            let (repo, id) = create_product(&store, &name, &idea.join(" "))?;
            if autonomous {
                store.set_mode(&name, "autonomous")?;
            }
            println!("Created {name} in {}. Task #{id}: the Chief of Staff turns the idea into a plan.", repo.display());
            Ok(())
        }
        JazariCmd::Mode { product, mode } => {
            println!("{}", set_mode(&open()?, &product, &mode)?);
            Ok(())
        }
        JazariCmd::Feedback { product, text } => {
            println!("{}", feedback(&open()?, &product, &text.join(" "))?);
            Ok(())
        }
        JazariCmd::Prefer { product, area, rule } => {
            println!("{}", prefer(&open()?, &product, &area, &rule.join(" "))?);
            Ok(())
        }
        JazariCmd::Forget { id } => {
            println!("{}", forget(&open()?, id)?);
            Ok(())
        }
        JazariCmd::Prefs { product } => {
            println!("{}", profile::summary(&open()?, product.as_deref())?);
            Ok(())
        }
        JazariCmd::Accept { id } => {
            println!("{}", proposal(&open()?, id, true)?);
            Ok(())
        }
        JazariCmd::Ignore { id } => {
            println!("{}", proposal(&open()?, id, false)?);
            Ok(())
        }
        JazariCmd::Idea { product, idea } => {
            let id = queue_idea(&open()?, &product, &idea.join(" "))?;
            println!("Queued task #{id}: the Chief of Staff turns the idea into a plan and the first tasks.");
            Ok(())
        }
        JazariCmd::Answer { task, answer } => {
            answer_task(&open()?, task, &answer.join(" "), true)?;
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
                println!("jazari now uses these copies. `jazari build evals` checks your changes, with effort matched to their size.");
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
Add a short \"Risks and assumptions\" section to VISION.md: who it is really for, what could make it fail, and
the cheapest way to test it. If the idea has a serious problem (security, law, users' safety, or it can't work as
described), don't plan it as asked: return blocked with a needs_founder question that offers a better alternative,
with the evidence.

The founder's idea:
";

pub(crate) fn queue_idea(store: &store::Store, product: &str, idea: &str) -> anyhow::Result<i64> {
    anyhow::ensure!(!idea.trim().is_empty(), "describe the idea");
    anyhow::ensure!(store.product(product)?.is_some(), "no product called {product}");
    store.add_signal(product, "idea", idea, crate::now())?;
    let brief = format!("{INTAKE}{}", idea.trim());
    let title = format!("Plan: {}", crate::hook::truncate(idea.lines().next().unwrap_or(idea), 80));
    store.add_task(&store::NewTask { product, role: "chief-of-staff", title: &title, brief: &brief, priority: 100, ..Default::default() }, crate::now())
}

/// The founder's answer to a task's question. An answer the founder typed is also a signal for the
/// profile. A button tap (`typed` false) never is, and neither is a typed pick of an offered
/// option ("option 2", or its label): a worker wrote those words, and nothing a worker writes may
/// become a preference.
pub(crate) fn answer_task(store: &store::Store, task: i64, answer: &str, typed: bool) -> anyhow::Result<()> {
    let t = store.reply(task, answer, crate::now())?;
    if typed && !picks_an_offered_option(t.question.as_deref().unwrap_or(""), answer) {
        store.add_signal(&t.product, "answer", answer, crate::now())?;
    }
    Ok(())
}

/// Whether an answer only picks one of the options listed in a stored question.
fn picks_an_offered_option(question: &str, answer: &str) -> bool {
    let answer = answer.trim().to_lowercase();
    question.lines().any(|l| {
        let Some((n, rest)) = l.split_once(". ") else { return false };
        if !n.chars().all(|c| c.is_ascii_digit()) || n.is_empty() {
            return false;
        }
        let label = rest.split(": ").next().unwrap_or(rest).trim().to_lowercase();
        answer == label || answer == n || answer == format!("option {n}")
    })
}

pub(crate) fn feedback(store: &store::Store, product: &str, text: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!text.trim().is_empty(), "say what you'd like");
    anyhow::ensure!(product == "all" || store.product(product)?.is_some(), "no product called {product}");
    store.add_signal(if product == "all" { "*" } else { product }, "feedback", text, crate::now())?;
    store.put("last_product", product)?;
    Ok("Noted. jazari learns it as a preference once you have said it a few times. To make it a rule now, use /prefer.".into())
}

pub(crate) fn prefer(store: &store::Store, product: &str, area: &str, rule: &str) -> anyhow::Result<String> {
    let scope = if product == "all" { "*" } else { product };
    anyhow::ensure!(scope == "*" || store.product(scope)?.is_some(), "no product called {product}");
    let id = profile::prefer(store, scope, area, rule, crate::now())?;
    write_profile(store);
    Ok(format!("📌 Preference #{id} set: {}. The team follows it, and the evaluator checks it.", rule.trim()))
}

pub(crate) fn forget(store: &store::Store, id: i64) -> anyhow::Result<String> {
    let said = profile::forget(store, id, crate::now())?;
    write_profile(store);
    Ok(said)
}

pub(crate) fn set_mode(store: &store::Store, product: &str, mode: &str) -> anyhow::Result<String> {
    store.set_mode(product, mode)?;
    Ok(if mode == "autonomous" {
        format!("{product} is now autonomous: after your tasks, jazari keeps improving it (bugs, security, UX, performance, docs, next improvements).")
    } else {
        format!("{product} is now in normal mode: jazari stops when your tasks are done.")
    })
}

/// Accepts or ignores a proposal; an accepted one becomes a task.
pub(crate) fn proposal(store: &store::Store, id: i64, accept: bool) -> anyhow::Result<String> {
    let key = format!("proposal:{id}");
    let v: serde_json::Value = store.get(&key)?.and_then(|t| serde_json::from_str(&t).ok()).ok_or_else(|| anyhow::anyhow!("no open proposal #{id}"))?;
    store.put(&key, "{}")?;
    let (Some(product), Some(criterion)) = (v["product"].as_str(), v["criterion"].as_str()) else { anyhow::bail!("no open proposal #{id}") };
    if !accept {
        return Ok(format!("Ignored proposal #{id}."));
    }
    let brief = format!(
        "Reviews of this product keep failing the criterion \"{criterion}\". Add an automatic check to company/checks.sh that catches this problem before review (for example a linter rule, a test or a coverage limit). Make the checks pass on the main branch.\n\nAcceptance: the new check fails on an example of the problem and passes on correct code."
    );
    let task = store.add_task(&store::NewTask { product, role: "platform-engineer", title: &format!("Add an automatic check: {criterion}"), brief: &brief, priority: 5, ..Default::default() }, crate::now())?;
    Ok(format!("Queued task #{task}: add an automatic check for \"{criterion}\"."))
}

/// Writes the profile to `~/.jazari/build/FOUNDER.md`.
fn write_profile(store: &store::Store) {
    if let Ok(text) = profile::render(store) {
        let _ = std::fs::create_dir_all(dir());
        let _ = std::fs::write(dir().join("FOUNDER.md"), text);
    }
}

/// Where new products' repos go: `jazari.products_dir`, or `products` in jazari's data folder
/// (so `JAZARI_HOME` moves it too).
fn products_dir() -> PathBuf {
    crate::settings::get("jazari").and_then(|j| j["products_dir"].as_str().map(PathBuf::from)).unwrap_or_else(|| dir().join("products"))
}

/// The skills workers get: `jazari.skills` in settings.json (`["all"]` for the whole library),
/// or Easy English. They load a skill only when they need it, so each one costs a line of its
/// description per run.
fn worker_skills() -> Vec<String> {
    crate::settings::get("jazari")
        .and_then(|j| j["skills"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(String::from)).collect::<Vec<_>>()))
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| vec!["easy-english".into()])
}

/// Writes the chosen skills from jazari's library into `<work>/.skills`, replacing what was there,
/// so a worker that changed them doesn't pass the change on. The container mounts the work folder,
/// so `.skills` may be anything a worker left: a link out of the folder, a file. Nothing there is
/// followed. The skills are built in a fresh folder and moved into place.
fn write_skills(work: &std::path::Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(work)?;
    let work = std::fs::canonicalize(work)?;
    let fresh = work.join(format!(".skills-new-{}", std::process::id()));
    clear(&fresh)?;
    std::fs::create_dir(&fresh)?;
    let keys = worker_skills();
    let all = keys.iter().any(|k| k == "all");
    for skill in crate::skills::library().into_iter().filter(|s| all || keys.contains(&s.key)) {
        for f in &skill.files {
            anyhow::ensure!(!f.path.split('/').any(|part| part == ".." || part.is_empty()), "a skill file has an unsafe path: {}", f.path);
            let path = fresh.join(&skill.key).join(&f.path);
            let parent = path.parent().expect("a skill file has a folder");
            std::fs::create_dir_all(parent)?;
            anyhow::ensure!(std::fs::canonicalize(parent)?.starts_with(&fresh), "a skill folder leads outside the work folder: {}", parent.display());
            std::fs::write(path, &f.bytes)?;
        }
    }
    let dest = work.join(".skills");
    clear(&dest)?;
    std::fs::rename(&fresh, &dest)?;
    Ok(())
}

/// Removes whatever is at `path` without following it: a link or a file is unlinked, a real
/// folder is removed with its contents.
fn clear(path: &std::path::Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))?,
        Err(_) => {}
    }
    Ok(())
}

/// A product's work folder: the task clones its container sees, in jazari's data folder.
pub fn work_dir(product: &str) -> PathBuf {
    let dir = dir().join("work").join(product);
    std::path::absolute(&dir).unwrap_or(dir)
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
    // The free lint always gates the start; --skip-evals skips only the paid calibration.
    let roster = personas::all();
    let lint: Vec<_> = evals::lint(&roster, &personas::rubrics_text()).into_iter().filter(|i| i.error).collect();
    if !lint.is_empty() {
        for i in &lint {
            eprintln!("✗ {}", i.what);
        }
        tell_founder("⛔ jazari won't start: the personas or rubrics are broken. Run `jazari build evals` for details.", &[]);
        anyhow::bail!("fix personas.md or rubrics.md first");
    }
    if !skip_evals {
        // The judges' calibration: free when nothing changed, and matched to the change's size
        // when something did.
        let report = evals::check(&store, &roster, false, limits::plan().profile().build_model, &evals::claude_judge)?;
        if !report.plan.changed.is_empty() || report.blocking() {
            println!("{}", report.text());
        }
        if report.blocking() {
            tell_founder("⛔ jazari won't start: a judge check failed. Run `jazari build evals` for details.", &[]);
            anyhow::bail!("the judges' checks block the supervisor; fix them, or start with --skip-evals");
        }
    }
    let requeued = store.requeue_running(crate::now())?;
    if requeued > 0 {
        println!("{requeued} task(s) left running by an earlier supervisor went back in the queue.");
    }
    let runner = worker::Docker { token };
    let audit_path = dir.join("audit.jsonl");
    let now = crate::now;
    let paused = crate::act::paused;
    let ensure = |p: &str, work: &str| sandbox::ensure(p, work);
    let reset = |name: &str| sandbox::reset(name);
    let skills = |work: &std::path::Path| write_skills(work);
    let small = |system: &str, prompt: &str, schema: &str| {
        llm::claude(&llm::Ask { system, prompt, schema, model: llm::SMALL, effort: "low", timeout: std::time::Duration::from_secs(120) })
    };
    let founder_md = dir.join("FOUNDER.md");
    let profile_file = |text: &str| {
        let _ = std::fs::write(&founder_md, text);
    };
    let work = |p: &str| work_dir(p);
    // Read once: the supervisor is the only writer (its lock is held), so it carries the count and head.
    let audit_tail = std::cell::RefCell::new(audit::tail(&audit_path));
    let audit = |kind: &str, data: serde_json::Value| {
        let mut t = audit_tail.borrow_mut();
        match audit::append(&audit_path, &mut t, crate::now(), kind, data) {
            Ok(()) if t.count.is_multiple_of(audit::EVERY) => {
                // The head goes off this machine, so a record written again here can't match it.
                tell_founder(&audit::head_text(t.count, &t.head), &[]);
                if let Err(e) = audit::save_head(&audit_path, t.count, &t.head) {
                    eprintln!("[jazari] couldn't save the audit head: {e}");
                }
            }
            Ok(()) => {}
            Err(e) => eprintln!("[jazari] couldn't write the audit record: {e}"),
        }
    };
    let profile = limits::plan().profile();
    let deps = supervisor::Deps {
        store: &store,
        runner: &runner,
        repo: &supervisor::Git,
        profile,
        now: &now,
        paused: &paused,
        work: &work,
        ensure: &ensure,
        reset: &reset,
        skills: &skills,
        llm: &small,
        profile_file: &profile_file,
        notify: &tell_founder,
        audit: &audit,
    };
    println!("jazari supervisor on ({}). Ctrl-C stops it; a task in progress goes back in the queue next start.", profile.plan.label());
    send_audit_head(&audit_path);
    let result = supervise(&deps, once);
    send_audit_head(&audit_path);
    result
}

/// Sends the founder the audit record's count and head on Telegram and keeps a copy, so a record
/// cut or written again later doesn't match. When the record is already broken, says so instead.
fn send_audit_head(record: &std::path::Path) {
    match audit::check(record) {
        Ok(w) => {
            tell_founder(&audit::head_text(w.count, &w.head), &[]);
            if let Err(e) = audit::save_head(record, w.count, &w.head) {
                eprintln!("[jazari] couldn't save the audit head: {e}");
            }
        }
        Err(e) => tell_founder(&format!("⚠️ The audit record was changed: {e}. Run `jazari build audit`."), &[]),
    }
}

/// The supervisor's loop: one task at a time until stopped, or one round with `once`.
fn supervise(deps: &supervisor::Deps, once: bool) -> anyhow::Result<()> {
    let mut errors = 0u32;
    loop {
        let wait = match supervisor::tick(deps) {
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
            Ok(supervisor::Tick::Paused) if once => {
                println!("jazari is paused (`jazari watch resume` turns it back on).");
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

#[cfg(test)]
mod tests {
    #[test]
    fn picking_a_worker_written_option_never_becomes_a_preference_but_typed_answers_do() {
        let store = super::store::Store::memory();
        store.add_product("shop", "/r", 0.0).unwrap();
        let q = "Which review policy?\n1. Skip security review for small changes: faster\n2. Keep every review: slower\nRecommended: Keep every review";
        for answer in ["Skip security review for small changes", "option 1", "1"] {
            let t = store.add_task(&super::store::NewTask { product: "shop", role: "e", title: "x", ..Default::default() }, 0.0).unwrap();
            store.wait_for_founder(t, Some(q), None, 0.0).unwrap();
            super::answer_task(&store, t, answer, true).unwrap();
        }
        let t = store.add_task(&super::store::NewTask { product: "shop", role: "e", title: "x", ..Default::default() }, 0.0).unwrap();
        store.wait_for_founder(t, Some(q), None, 0.0).unwrap();
        super::answer_task(&store, t, "Policy: anything a button sends", false).unwrap();
        assert!(store.pending_signals(10).unwrap().is_empty(), "button picks carry a worker's words, not the founder's");
        let t = store.add_task(&super::store::NewTask { product: "shop", role: "e", title: "x", ..Default::default() }, 0.0).unwrap();
        store.wait_for_founder(t, Some(q), None, 0.0).unwrap();
        super::answer_task(&store, t, "Keep reviews, but make the error text shorter", true).unwrap();
        assert_eq!(store.pending_signals(10).unwrap().len(), 1, "what the founder typed is a signal");
    }

    #[test]
    fn an_accepted_proposal_becomes_a_task_and_each_proposal_is_used_once() {
        let store = super::store::Store::memory();
        store.add_product("shop", "/r", 0.0).unwrap();
        store.put("proposal:1", r#"{"product":"shop","criterion":"Tests prove the behavior"}"#).unwrap();
        let said = super::proposal(&store, 1, true).unwrap();
        let task = store.tasks(Some("shop"), 5).unwrap().remove(0);
        assert!(said.contains(&format!("#{}", task.id)) && task.role == "platform-engineer" && task.brief.contains("company/checks.sh"));
        assert!(super::proposal(&store, 1, true).is_err(), "a proposal is used once");
        assert!(super::set_mode(&store, "shop", "autonomous").unwrap().contains("keeps improving"));
        assert!(super::feedback(&store, "nope", "x").is_err());
        assert!(super::feedback(&store, "shop", "shorter sentences").is_ok());
        assert_eq!(store.pending_signals(5).unwrap()[0].text, "shorter sentences");
    }

    #[cfg(unix)]
    #[test]
    fn a_planted_link_in_the_work_folder_is_never_followed() {
        let base = std::env::temp_dir().join(format!("jazari-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (work, outside) = (base.join("work"), base.join("outside"));
        std::fs::create_dir_all(&work).unwrap();
        // A dangling link, where the host would otherwise create the skills outside the work folder.
        std::os::unix::fs::symlink(&outside, work.join(".skills")).unwrap();
        super::write_skills(&work).unwrap();
        assert!(!outside.exists(), "nothing was written through the link");
        let meta = std::fs::symlink_metadata(work.join(".skills")).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        // A link to an existing folder outside: still replaced, never followed.
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::remove_dir_all(work.join(".skills")).unwrap();
        std::os::unix::fs::symlink(&outside, work.join(".skills")).unwrap();
        super::write_skills(&work).unwrap();
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "the outside folder stays empty");
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn the_chosen_skills_are_written_fresh_into_the_work_folder() {
        let work = std::env::temp_dir().join(format!("jazari-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(work.join(".skills/old")).unwrap();
        std::fs::write(work.join(".skills/old/SKILL.md"), "changed by a worker").unwrap();
        super::write_skills(&work).unwrap();
        assert!(!work.join(".skills/old").exists(), "what a worker left there is replaced");
        if super::worker_skills().iter().any(|k| k == "easy-english" || k == "all") {
            assert!(work.join(".skills/easy-english/SKILL.md").is_file());
            assert!(work.join(".skills/easy-english/scripts/check.py").is_file());
        }
        std::fs::remove_dir_all(work).ok();
    }
}

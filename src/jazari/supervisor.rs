//! The supervisor: one task at a time, for as long as it runs.
//!
//! Each tick it checks the pause and the usage windows, then takes the next queued task. The
//! worker runs in the product's container, in a fresh clone the host made in jazari's work
//! folder. The container never sees the product's repo or its `.git`. The worker's commits come
//! back as a bundle, and nothing it claims is trusted until it is verified.
//!
//! A branch with new commits goes through these gates in order:
//! 1. **Harness checks.** Deterministic and free:
//!    - the base branch's `company/checks.sh`, run in the container on a fresh clone of the
//!      branch, so a worker can't weaken the gate it is judged by;
//!    - a secret scan of the diff.
//! 2. **The evaluator.** A fresh, read-only judge scores the work against the builder's role
//!    rubric, with the check output as evidence.
//! 3. **Code review**, then **security review.** Both are read-only. Documentation-only (`*.md`)
//!    changes skip them, and any change to `checks.sh` gets both.
//! 4. **The merge queue.** The approved commit (pinned when the checks passed, not the branch
//!    name) is merged aside. The checks run again on the merged result, and only then does the
//!    default branch move.
//!
//! A failed gate sends a fix task back to the builder; after a few rounds the founder decides.
//! Every judgment is recorded per role and prompt version. A role that keeps failing evaluation
//! moves up to the plan's lead model.

use super::card::Card;
use super::git::{Stage, Staged};
use super::limits::{self, Decision, Limits, Profile};
use super::personas::{self, Tier};
use super::store::{NewTask, Run, Store, Task};
use super::worker::{self, Kind, Outcome, Runner, Setup};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Failed attempts before a task is set aside for the founder.
pub const MAX_ATTEMPTS: i64 = 3;
/// Fix rounds a branch gets before the founder is asked.
pub const MAX_ROUNDS: i64 = 3;
/// Queued tasks a product may have before build roles' follow-ups are dropped; planning roles
/// may still add theirs.
pub const MAX_QUEUED: i64 = 25;
/// How long to wait after a refusal that gave no reset time.
const BACKOFF_SECS: f64 = 900.0;
/// How long a product whose container or clone can't be set up is passed over.
const HOLD_SECS: f64 = 600.0;
/// Evaluations looked at to decide whether a role needs a stronger model.
const UPGRADE_WINDOW: usize = 8;
/// The product's own checks: build, tests, lint, type checks.
pub const CHECKS: &str = "company/checks.sh";
/// The longest the checks may run, in seconds.
const CHECKS_TIMEOUT_SECS: u64 = 15 * 60;
/// Where the harness asks the container to write a worker's branch.
const BUNDLE: &str = ".jazari-out.bundle";

/// Git on a product's repo, so tests run without one. Every call runs on the host, never in a
/// folder a container has written.
pub trait Repo {
    fn default_branch(&self, repo: &Path) -> anyhow::Result<String>;
    fn head(&self, repo: &Path, rev: &str) -> Option<String>;
    fn clone_for_task(&self, repo: &Path, dest: &Path, branch: &str, base: &str) -> anyhow::Result<()>;
    fn clone_at(&self, repo: &Path, dest: &Path, commit: &str) -> anyhow::Result<()>;
    fn import_bundle(&self, repo: &Path, bundle: &Path, branch: &str) -> anyhow::Result<()>;
    fn commits_ahead(&self, repo: &Path, base: &str, rev: &str) -> usize;
    fn changed_files(&self, repo: &Path, base: &str, rev: &str) -> Vec<String>;
    fn secrets(&self, repo: &Path, base: &str, rev: &str) -> Vec<String>;
    fn show(&self, repo: &Path, rev: &str, path: &str) -> Option<String>;
    fn stage(&self, repo: &Path, base: &str, commit: &str, message: &str) -> anyhow::Result<Stage>;
    fn advance(&self, repo: &Path, base: &str, staged: &Staged) -> anyhow::Result<String>;
    fn discard(&self, repo: &Path, staged: &Staged);
}

pub struct Git;

impl Repo for Git {
    fn default_branch(&self, repo: &Path) -> anyhow::Result<String> {
        super::git::default_branch(repo)
    }
    fn head(&self, repo: &Path, rev: &str) -> Option<String> {
        super::git::head(repo, rev)
    }
    fn clone_for_task(&self, repo: &Path, dest: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
        super::git::clone_for_task(repo, dest, branch, base)
    }
    fn clone_at(&self, repo: &Path, dest: &Path, commit: &str) -> anyhow::Result<()> {
        super::git::clone_at(repo, dest, commit)
    }
    fn import_bundle(&self, repo: &Path, bundle: &Path, branch: &str) -> anyhow::Result<()> {
        super::git::import_bundle(repo, bundle, branch)
    }
    fn commits_ahead(&self, repo: &Path, base: &str, rev: &str) -> usize {
        super::git::commits_ahead(repo, base, rev)
    }
    fn changed_files(&self, repo: &Path, base: &str, rev: &str) -> Vec<String> {
        super::git::changed_files(repo, base, rev)
    }
    fn secrets(&self, repo: &Path, base: &str, rev: &str) -> Vec<String> {
        super::git::secrets(repo, base, rev)
    }
    fn show(&self, repo: &Path, rev: &str, path: &str) -> Option<String> {
        super::git::show(repo, rev, path)
    }
    fn stage(&self, repo: &Path, base: &str, commit: &str, message: &str) -> anyhow::Result<Stage> {
        super::git::stage(repo, base, commit, message)
    }
    fn advance(&self, repo: &Path, base: &str, staged: &Staged) -> anyhow::Result<String> {
        super::git::advance(repo, base, staged)
    }
    fn discard(&self, repo: &Path, staged: &Staged) {
        super::git::discard(repo, staged)
    }
}

/// A Telegram button: its label and the command it sends back.
pub type Button = (String, String);

/// The supervisor's surroundings, so tests run without Docker, git, Telegram or a clock.
pub struct Deps<'a> {
    pub store: &'a Store,
    pub runner: &'a dyn Runner,
    pub repo: &'a dyn Repo,
    pub profile: Profile,
    pub now: &'a dyn Fn() -> f64,
    pub paused: &'a dyn Fn() -> bool,
    /// The product's work folder: the only folder its container mounts.
    pub work: &'a dyn Fn(&str) -> PathBuf,
    /// Starts the product's container around its work folder and returns its name.
    pub ensure: &'a dyn Fn(&str, &str) -> anyhow::Result<String>,
    /// Restarts a container, ending anything a worker left running.
    pub reset: &'a dyn Fn(&str) -> anyhow::Result<()>,
    /// Writes the skills the workers get into the product's work folder (`.skills/`).
    pub skills: &'a dyn Fn(&Path) -> anyhow::Result<()>,
    pub notify: &'a dyn Fn(&str, &[Button]),
    pub audit: &'a dyn Fn(&str, serde_json::Value),
}

#[derive(Debug, PartialEq)]
pub enum Tick {
    Paused,
    Sleeping { until: f64 },
    Idle,
    Ran { task: i64, kind: Kind },
}

fn limits(store: &Store) -> Limits {
    store.get("limits").ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn clock(ts: f64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0).map(|t| t.with_timezone(&chrono::Local).format("%a %H:%M").to_string()).unwrap_or_default()
}

/// Sends the founder a card.
fn say(d: &Deps, card: &Card) {
    (d.notify)(&card.text(), &card.buttons());
}

/// Sends a card once per `key`, however often it is true again.
fn once(d: &Deps, key: &str, card: &Card) {
    let key = format!("told:{key}");
    if d.store.get(&key).ok().flatten().is_none() {
        let _ = d.store.put(&key, "1");
        say(d, card);
    }
}

fn announce_sleep(d: &Deps, until: f64, reason: &str, key: &str) {
    if d.store.get(&format!("told:sleep:{key}")).ok().flatten().is_none() {
        (d.audit)("sleep", json!({ "until": until, "reason": reason }));
    }
    once(d, &format!("sleep:{key}"), &Card::new("😴", "jazari is paused for usage", format!("It starts again at {}.", clock(until))).why(format!("{}.", capital(reason))));
}

// ---------- the harness's own checks ----------

/// What the harness found by running things itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checks {
    pub passed: bool,
    /// Neither the base nor the branch has a `company/checks.sh`.
    pub missing: bool,
    /// The end of the output, for the evaluator and for fixes.
    pub log: String,
}

/// The checks script to run for a change: the base branch's, so the change can't weaken the gate
/// it is judged by. While the base has none, the change's own copy is used once; such a change
/// always gets full review. Returns the script and whether it is that first copy.
fn checks_script(d: &Deps, repo: &Path, base: &str, rev: &str) -> (Option<String>, bool) {
    match d.repo.show(repo, base, CHECKS) {
        Some(s) => (Some(s), false),
        None => (d.repo.show(repo, rev, CHECKS), true),
    }
}

/// The shell a checks script asks for on its `#!` line: bash or zsh when named, else `sh`. The
/// script runs as `<shell> -c`, so the line itself would be ignored.
fn shell(script: &str) -> &'static str {
    let first = script.lines().next().unwrap_or("");
    let wants = |name: &str| first.starts_with("#!") && first.split(['/', ' ']).any(|w| w == name);
    if wants("bash") {
        "bash"
    } else if wants("zsh") {
        "zsh"
    } else {
        "sh"
    }
}

/// Runs a checks script inside the product's container, in `dir` (the container path of a fresh
/// clone the host made), after a restart. Never on the host, and never with the login token.
pub fn run_checks(d: &Deps, container: &str, dir: &str, script: Option<&str>) -> Checks {
    let Some(script) = script else { return Checks { passed: false, missing: true, log: String::new() } };
    if let Err(e) = (d.reset)(container) {
        return Checks { passed: false, missing: false, log: format!("the checks couldn't run: {e}") };
    }
    let command: Vec<String> = vec![
        "timeout".into(),
        "--kill-after=30".into(),
        CHECKS_TIMEOUT_SECS.to_string(),
        shell(script).into(),
        "-c".into(),
        format!("exec 2>&1\n{script}"),
    ];
    let mut lines: Vec<String> = Vec::new();
    let code = d.runner.run(container, dir, &command, false, &mut |l| lines.push(l.to_string()));
    let tail = lines[lines.len().saturating_sub(60)..].join("\n");
    let log = super::telegram::redact(&crate::hook::truncate(&tail, 4000));
    match code {
        Ok(0) => Checks { passed: true, missing: false, log },
        Ok(c) => Checks { passed: false, missing: false, log: format!("exit code {c}\n{log}") },
        Err(e) => Checks { passed: false, missing: false, log: format!("the checks couldn't run: {e}") },
    }
}

/// Clones `rev` into a scratch folder of the work area, runs the checks there, and removes it.
fn checks_on(d: &Deps, container: &str, work: &Path, name: &str, clone: impl FnOnce(&Path) -> anyhow::Result<()>, script: Option<&str>, docs: &[String]) -> (Checks, String) {
    let dir = work.join(name);
    let result = match clone(&dir) {
        Ok(()) => {
            let checks = run_checks(d, container, &super::sandbox::inside(name), script);
            (checks, easy_english(d, container, &super::sandbox::inside(name), docs))
        }
        Err(e) => (Checks { passed: false, missing: false, log: format!("the checks couldn't be set up: {e}") }, String::new()),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// The Easy English checker's report on the changed docs, run in the container. It is evidence
/// for the evaluator, not a gate: the checker can't see meaning.
fn easy_english(d: &Deps, container: &str, dir: &str, docs: &[String]) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let mut command: Vec<String> = ["timeout", "120", "sh", "-c", "python3 \"$HOME/.claude/skills/easy-english/scripts/check.py\" \"$@\" 2>&1", "sh"].map(String::from).to_vec();
    command.extend(docs.iter().take(20).cloned());
    let mut out = Vec::new();
    let _ = d.runner.run(container, dir, &command, false, &mut |l| out.push(l.to_string()));
    crate::hook::truncate(&out.join("\n"), 1500)
}

/// The gates a built branch must pass before an evaluator spends tokens on it.
struct Gates {
    /// What to fix, when a gate failed.
    failure: Option<String>,
    /// The evidence handed to the evaluator.
    report: String,
    docs: bool,
    /// The commit the gates passed: the one every later stage judges and the merge uses.
    head: Option<String>,
}

#[allow(clippy::too_many_arguments)]
fn gates(d: &Deps, task: &Task, container: &str, repo: &Path, work: &Path, base: &str, branch: &str) -> Gates {
    let head = d.repo.head(repo, &format!("refs/heads/{branch}"));
    let rev = head.clone().unwrap_or_else(|| branch.to_string());
    let files = d.repo.changed_files(repo, base, &rev);
    let docs = super::git::docs_only(&files);
    let secrets = d.repo.secrets(repo, base, &rev);
    let (script, first) = checks_script(d, repo, base, &rev);
    if files.iter().any(|f| f == CHECKS) {
        let what = if first { "adds" } else { "changes" };
        let card = Card::new("🛡", format!("Task #{} {what} the quality checks", task.id), format!("`{CHECKS}` decides what every branch must pass."))
            .why("A change to the checks changes the rules for all later work. So it gets a full code review and security review before it counts.");
        once(d, &format!("checks:{}:{rev}", task.id), &card);
    }
    let docs_changed: Vec<String> = files.iter().filter(|f| f.ends_with(".md")).cloned().collect();
    let (checks, plain) = checks_on(d, container, work, &format!("check-{}", task.id), |dir| d.repo.clone_for_task(repo, dir, branch, base), script.as_deref(), &docs_changed);
    let failure = if !secrets.is_empty() {
        Some(format!(
            "The secret scan found credentials in: {}. Remove them and read them from the environment instead. This branch isn't merged, so rewrite its own commits so the secret is gone from its history.",
            secrets.join(", ")
        ))
    } else if checks.missing && !docs {
        Some(format!("This repo has no `{CHECKS}`. Add it: one script that runs the build, every test suite, the linters and the type checks, and exits non-zero on any failure. Then make it pass."))
    } else if !checks.passed && !checks.missing {
        Some(format!("The harness ran `{CHECKS}` on your branch and it failed:\n```\n{}\n```\nMake it pass.", checks.log))
    } else {
        None
    };
    let stage = if secrets.is_empty() { "checks" } else { "secrets" };
    let why = failure.as_deref().map(|f| crate::hook::truncate(f, 200)).unwrap_or_default();
    let _ = judged(d, task, stage, failure.is_none(), None, &why);
    let status = if checks.missing {
        "not set up (documentation-only change)".to_string()
    } else if checks.passed {
        format!("passed{}", if first { " (the branch's own new script: the base has none yet)" } else { " (the base branch's script)" })
    } else {
        "FAILED".into()
    };
    let plain = if plain.is_empty() { String::new() } else { format!("\n\nThe Easy English checker on the changed docs (hints, not a gate; judge the meaning yourself):\n```\n{plain}\n```") };
    let report = format!(
        "Evidence the harness gathered itself (trust this over the worker's claims):\n- `{CHECKS}`: {status}\n- secret scan: {}\n- changed files: {}\n```\n{}\n```{plain}",
        if secrets.is_empty() { "clean" } else { "credentials found" },
        crate::hook::truncate(&files.join(", "), 600),
        checks.log
    );
    Gates { failure, report, docs, head }
}

// ---------- merges ----------

/// A merge waiting: for the founder's checkout to be clean, or after an error.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct Pending {
    task: i64,
    branch: String,
    title: String,
    head: String,
}

fn pending(store: &Store) -> Vec<Pending> {
    store.get("pending_merges").ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn set_pending(store: &Store, list: &[Pending]) -> anyhow::Result<()> {
    store.put("pending_merges", &serde_json::to_string(list)?)
}

/// What came of a merge attempt.
#[derive(Debug, PartialEq)]
enum Merged {
    Done,
    /// It can't happen yet (a dirty checkout); try again later.
    Waiting,
    /// It went back to the builder (a conflict, or checks failing on the merged result), or the
    /// branch moved after approval and its new commits have their own gates.
    Handled,
}

/// Keeps a merge for later and tells the founder once.
fn park(d: &Deps, p: Pending, why: &str) -> anyhow::Result<()> {
    let mut list = pending(d.store);
    if !list.iter().any(|x| x.task == p.task) {
        list.push(p.clone());
        set_pending(d.store, &list)?;
    }
    let card = Card::new("⏸", format!("Not merged yet: {}", p.title), "The work is approved, but jazari can't merge it now.").why(capital(why)).detail("jazari tries again by itself.");
    once(d, &format!("park:{}:{}", p.branch, p.head), &card);
    Ok(())
}

/// Tries the waiting merges; costs no tokens. An entry leaves the list only once it is settled.
fn retry_merges(d: &Deps) -> anyhow::Result<()> {
    let list = pending(d.store);
    if list.is_empty() {
        return Ok(());
    }
    let mut left = Vec::new();
    for p in list {
        let Some(task) = d.store.task(p.task)? else { continue };
        match merge(d, &task, &p.branch, &p.head, &p.title) {
            Ok(Merged::Done | Merged::Handled) => {}
            Ok(Merged::Waiting) => left.push(p),
            Err(e) => {
                once(d, &format!("merge-error:{}:{}", p.branch, p.head), &Card::new("⚠️", format!("Merge keeps failing: {}", p.title), capital(&e.to_string())).detail("jazari keeps trying."));
                left.push(p);
            }
        }
    }
    set_pending(d.store, &left)
}

/// Merges an approved commit and handles every outcome: a merge the caller must keep is parked.
fn merge_or_park(d: &Deps, task: &Task, branch: &str, head: &str, title: &str) -> anyhow::Result<()> {
    let p = || Pending { task: task.id, branch: branch.into(), title: title.into(), head: head.into() };
    match merge(d, task, branch, head, title) {
        Ok(Merged::Waiting) => park(d, p(), "your checkout of the base branch has uncommitted changes. Commit or stash them"),
        Ok(_) => Ok(()),
        Err(e) => park(d, p(), &format!("{e}")),
    }
}

/// The merge queue: merges exactly the approved commit, builds the merge aside, runs the checks on
/// the merged result in the container, and moves the default branch only when they pass.
fn merge(d: &Deps, task: &Task, branch: &str, head: &str, title: &str) -> anyhow::Result<Merged> {
    let product = d.store.product(&task.product)?.ok_or_else(|| anyhow::anyhow!("no product {}", task.product))?;
    let repo = PathBuf::from(&product.repo);
    if d.repo.head(&repo, &format!("refs/heads/{branch}")).as_deref() != Some(head) {
        (d.audit)("merge.skipped", json!({ "task": task.id, "branch": branch, "approved": head, "why": "the branch moved after approval" }));
        return Ok(Merged::Handled);
    }
    let base = d.repo.default_branch(&repo)?;
    match d.repo.stage(&repo, &base, head, &format!("Merge {branch}: {title}"))? {
        Stage::Ready(staged) => {
            let work = (d.work)(&product.name);
            let container = (d.ensure)(&product.name, &work.to_string_lossy())?;
            let script = d.repo.show(&repo, &base, CHECKS).or_else(|| d.repo.show(&repo, &staged.new, CHECKS));
            let (checks, _) = checks_on(d, &container, &work, "merge", |dir| d.repo.clone_at(&repo, dir, &staged.new), script.as_deref(), &[]);
            if !checks.passed && !checks.missing {
                d.repo.discard(&repo, &staged);
                judged(d, task, "merge", false, None, "checks fail on the merged result")?;
                let findings = format!(
                    "The branch passes on its own, but `{CHECKS}` fails once `{base}` is merged into it:\n```\n{}\n```\nMerge `{base}` into the branch and make the checks pass.",
                    checks.log
                );
                fix(d, task, branch, title, &findings)?;
                return Ok(Merged::Handled);
            }
            let sha = d.repo.advance(&repo, &base, &staged)?;
            d.store.land_branch(branch)?;
            (d.audit)("merge", json!({ "task": task.id, "branch": branch, "approved": head, "base": base, "commit": sha }));
            say(d, &Card::new("🚀", format!("Merged: {title}"), format!("It is now on {base}.")).detail(format!("{branch} · {}", &sha[..sha.len().min(10)])));
            Ok(Merged::Done)
        }
        Stage::Conflict => {
            let findings = format!("`{branch}` no longer merges cleanly into `{base}`. Merge `{base}` into the branch, resolve the conflicts keeping both sides' intent, and make the checks pass.");
            fix(d, task, branch, title, &findings)?;
            Ok(Merged::Handled)
        }
        Stage::Dirty(_) => Ok(Merged::Waiting),
    }
}

// ---------- the tick ----------

/// The text with its first letter in upper case, for a reason that starts a sentence.
fn capital(text: &str) -> String {
    let mut c = text.trim().chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The role that built the branch a task is about.
fn owner(task: &Task) -> String {
    task.owner_role.clone().unwrap_or_else(|| personas::resolve(&task.role))
}

/// The feature a task belongs to, without the pipeline's prefixes.
fn feature(title: &str) -> &str {
    let mut t = title;
    for prefix in ["Evaluate: ", "Review: ", "Security review: ", "Fix: ", "Resolve conflicts: "] {
        t = t.strip_prefix(prefix).unwrap_or(t);
    }
    t
}

/// The model for a role: the profile's, or the lead model once the role was moved up.
fn model_for(d: &Deps, persona: &personas::Persona) -> &'static str {
    let upgraded = persona.tier == Tier::Build && d.store.get(&format!("upgrade:{}", persona.key)).ok().flatten().is_some();
    if upgraded { d.profile.lead_model } else { persona.model(&d.profile) }
}

/// Products passed over for now, after their container or clone couldn't be set up.
fn held(d: &Deps, now: f64) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for p in d.store.products()? {
        if d.store.get(&format!("hold:{}", p.name))?.and_then(|t| t.parse::<f64>().ok()).is_some_and(|t| t > now) {
            out.push(p.name);
        }
    }
    Ok(out)
}

pub fn tick(d: &Deps) -> anyhow::Result<Tick> {
    if (d.paused)() {
        return Ok(Tick::Paused);
    }
    retry_merges(d)?;
    let now = (d.now)();
    if let Some(until) = d.store.get("backoff_until")?.and_then(|t| t.parse::<f64>().ok()).filter(|t| *t > now) {
        announce_sleep(d, until, "a usage limit refused the last run", &format!("backoff:{until:.0}"));
        return Ok(Tick::Sleeping { until });
    }
    if let Decision::Sleep { until, reason, key } = limits::decide(&limits(d.store), &d.profile, now) {
        announce_sleep(d, until, &reason, &key);
        return Ok(Tick::Sleeping { until });
    }
    // One product that can't be set up doesn't stop the others.
    let mut held = held(d, now)?;
    loop {
        let Some(task) = d.store.next_task_except(&held)? else { return Ok(Tick::Idle) };
        match run_task(d, &task, now) {
            Ok(t) => return Ok(t),
            Err(e) => {
                d.store.set_status(task.id, "queued", None, None, now)?;
                d.store.put(&format!("hold:{}", task.product), &(now + HOLD_SECS).to_string())?;
                (d.audit)("product.held", json!({ "product": task.product, "task": task.id, "why": e.to_string() }));
                let card = Card::new("⚠️", format!("{} is on hold for 10 minutes", task.product), "jazari couldn't prepare its container or its copy of the repo.")
                    .why(capital(&e.to_string()))
                    .detail("Other products keep working.");
                once(d, &format!("hold:{}:{}", task.product, (now / 3600.0) as i64), &card);
                held.push(task.product.clone());
            }
        }
    }
}

/// Runs one task. An error means its product couldn't be set up; nothing was spent.
fn run_task(d: &Deps, task: &Task, now: f64) -> anyhow::Result<Tick> {
    let product = d.store.product(&task.product)?.ok_or_else(|| anyhow::anyhow!("task #{} names a missing product", task.id))?;
    let repo = PathBuf::from(&product.repo);
    let branch = worker::branch(task);
    let base = d.repo.default_branch(&repo)?;
    let judge = matches!(task.kind.as_str(), "eval" | "review" | "security");
    if judge && let Some(approved) = &task.head
        && d.repo.head(&repo, &format!("refs/heads/{branch}")).as_ref() != Some(approved)
    {
        // A later fix moved the branch: its commits have their own gates.
        d.store.set_status(task.id, "done", Some("Skipped: the branch moved after this stage was queued."), None, now)?;
        (d.audit)("stage.skipped", json!({ "task": task.id, "branch": branch, "approved": approved }));
        return Ok(Tick::Ran { task: task.id, kind: Kind::Done });
    }
    let work = (d.work)(&product.name);
    let container = (d.ensure)(&product.name, &work.to_string_lossy())?;
    let item = format!("task-{}", task.id);
    let clone = work.join(&item);
    d.repo.clone_for_task(&repo, &clone, &branch, &base)?;
    (d.skills)(&work)?;
    (d.reset)(&container)?;
    // Claude Code in the container finds the skills in its own folder.
    let link: Vec<String> = ["sh", "-c", &format!("mkdir -p \"$HOME/.claude\" && ln -sfn {} \"$HOME/.claude/skills\"", super::sandbox::inside(".skills"))].map(String::from).to_vec();
    d.runner.run(&container, super::sandbox::WORK, &link, false, &mut |_| {})?;
    let since = d.repo.head(&repo, &format!("refs/heads/{branch}")).or_else(|| d.repo.head(&repo, &base)).unwrap_or_default();
    d.store.set_status(task.id, "running", None, Some(&branch), now)?;
    (d.audit)("task.start", json!({ "task": task.id, "product": task.product, "role": task.role, "kind": task.kind, "title": task.title, "branch": branch }));
    let persona = personas::find(&task.role);
    let system_prompt = if task.kind == "eval" {
        let builder = personas::find(&owner(task));
        format!("{}\n# How to score\n\n{}\n\n# The rubric for the {}\n\n{}\n", persona.system_prompt(), personas::scoring_rules(), builder.title, builder.rubric())
    } else {
        persona.system_prompt()
    };
    let workdir = super::sandbox::inside(&item);
    let setup = Setup {
        container: &container,
        workdir: &workdir,
        base: &base,
        model: model_for(d, &persona),
        max_turns: d.profile.max_turns,
        system_prompt: &system_prompt,
        max_follow_ups: persona.max_follow_ups(),
        read_only: judge,
    };
    let outcome = worker::run(d.runner, task, &setup);
    // The worker's commits come back as a bundle the container writes; judges change nothing.
    let mut built = false;
    if !judge && outcome.kind == Kind::Done && !since.is_empty() {
        let command: Vec<String> = ["git", "bundle", "create", &format!("{workdir}/{BUNDLE}"), &branch, &format!("^{since}")].map(String::from).to_vec();
        if d.runner.run(&container, &workdir, &command, false, &mut |_| {}).is_ok_and(|code| code == 0) {
            d.repo.import_bundle(&repo, &clone.join(BUNDLE), &branch)?;
            built = d.repo.commits_ahead(&repo, &base, &branch) > 0;
        }
    }
    let _ = std::fs::remove_dir_all(&clone);
    let gates = built.then(|| gates(d, task, &container, &repo, &work, &base, &branch));
    let rev = task.head.clone().unwrap_or_else(|| branch.clone());
    let docs = super::git::docs_only(&d.repo.changed_files(&repo, &base, &rev));
    record(d, task, &outcome, gates, docs, &branch, now)?;
    Ok(Tick::Ran { task: task.id, kind: outcome.kind })
}

/// Sends a branch back to the role that built it, with what to fix and the original spec. After
/// [`MAX_ROUNDS`] the fix waits for the founder.
fn fix(d: &Deps, task: &Task, branch: &str, title: &str, findings: &str) -> anyhow::Result<i64> {
    let owner = owner(task);
    let round = task.round + 1;
    let brief = format!("{findings}\n\nThe original spec:\n{}", task.spec);
    let id = d.store.add_task(
        &NewTask {
            product: &task.product,
            role: &owner,
            title: &format!("Fix: {}", feature(title)),
            brief: &brief,
            priority: task.priority + 1,
            parent: Some(task.id),
            kind: "work",
            branch: Some(branch),
            owner_role: Some(&owner),
            round,
            after: None,
            spec: &task.spec,
            head: None,
        },
        (d.now)(),
    )?;
    if round >= MAX_ROUNDS {
        let q = format!("`{branch}` failed its checks or reviews {round} times. Latest: {}", crate::hook::truncate(findings, 500));
        d.store.wait_for_founder(id, Some(&q), None, (d.now)())?;
        let card = Card::new("❓", format!("#{id} {}: still failing", feature(title)), format!("It failed its checks or reviews {round} times. The team needs your guidance."))
            .why(crate::hook::truncate(findings, 400))
            .reply(format!("/answer {id} <what to do>"));
        say(d, &card);
    }
    Ok(id)
}

/// Records a judgment of the builder's work, tagged with the builder's current prompt version.
fn judged(d: &Deps, task: &Task, stage: &str, passed: bool, score: Option<i64>, failed: &str) -> anyhow::Result<()> {
    let owner = owner(task);
    let version = super::evals::version(&personas::find(&owner));
    d.store.add_eval(task.id, &owner, stage, passed, score, failed, (d.now)(), &version)
}

/// Moves a role to the lead model when it fails most of its recent evaluations.
fn maybe_upgrade(d: &Deps, role: &str) -> anyhow::Result<()> {
    let persona = personas::find(role);
    let key = format!("upgrade:{}", persona.key);
    if persona.tier != Tier::Build || d.store.get(&key)?.is_some() {
        return Ok(());
    }
    let recent = d.store.recent_evals(role, UPGRADE_WINDOW)?;
    let passed = recent.iter().filter(|(p, _)| *p).count();
    if recent.len() >= 6 && passed * 2 < recent.len() {
        d.store.put(&key, d.profile.lead_model)?;
        (d.audit)("upgrade", json!({ "role": role, "passed": passed, "of": recent.len(), "model": d.profile.lead_model }));
        let card = Card::new("📈", format!("{} now uses a stronger model", persona.title), format!("It passed only {passed} of its last {} evaluations.", recent.len()))
            .detail(format!("New model: {}", d.profile.lead_model));
        say(d, &card);
    }
    Ok(())
}

fn record(d: &Deps, task: &Task, o: &Outcome, gates: Option<Gates>, docs: bool, branch: &str, started: f64) -> anyhow::Result<()> {
    let now = (d.now)();
    let s = d.store;
    s.add_run(&Run {
        task: task.id,
        started,
        ended: now,
        outcome: o.kind.key().into(),
        session: o.session.clone(),
        input: o.input,
        output: o.output,
        cache_read: o.cache_read,
        used_5h: o.limits.and_then(|l| l.five_hour).map(|w| w.used_pct),
        used_7d: o.limits.and_then(|l| l.seven_day).map(|w| w.used_pct),
    })?;
    if let Some(l) = o.limits {
        s.put("limits", &serde_json::to_string(&l)?)?;
    }
    let name = format!("#{} {}", task.id, task.title);
    let owner = owner(task);
    let next = |role: &str, title: String, brief: &str, kind: &str, head: Option<&str>| {
        s.add_task(
            &NewTask {
                product: &task.product,
                role,
                title: &title,
                brief,
                priority: task.priority + 1,
                parent: Some(task.id),
                kind,
                branch: Some(branch),
                owner_role: Some(&owner),
                round: task.round,
                after: None,
                spec: &task.spec,
                head,
            },
            now,
        )
    };
    match o.kind {
        Kind::Done => {
            s.set_status(task.id, "done", Some(&o.summary), None, now)?;
            let queued = follow_ups(d, task, o)?;
            let more = if queued == 0 { String::new() } else { format!("\n+{queued} follow-up task(s) queued.") };
            match task.kind.as_str() {
                "eval" | "review" | "security" => {
                    let approved = o.verdict.as_deref() == Some("approve");
                    let failed = o.failed.join("; ");
                    judged(d, task, &task.kind, approved, o.score, &failed)?;
                    if task.kind == "eval" {
                        maybe_upgrade(d, &owner)?;
                        let version = super::evals::version(&personas::find(&owner));
                        if let Some(warning) = super::evals::canary(s, &owner, &version)? {
                            say(d, &Card::new("⚠️", format!("{} got worse after a change", personas::find(&owner).title), warning).detail("Check the last change to personas.md, or undo it."));
                        }
                    }
                    let head = task.head.as_deref();
                    if !approved {
                        let who = match task.kind.as_str() {
                            "eval" => "evaluator",
                            "review" => "code review",
                            _ => "security review",
                        };
                        let criteria = if failed.is_empty() { String::new() } else { format!("\nFailed criteria: {failed}") };
                        fix(d, task, branch, &task.title, &format!("The {who} asked for changes:\n{}{criteria}", o.summary))?;
                    } else if task.kind == "eval" && !docs {
                        next("code-reviewer", format!("Review: {}", feature(&task.title)), "", "review", head)?;
                    } else if task.kind == "review" && !docs {
                        next("appsec-engineer", format!("Security review: {}", feature(&task.title)), "", "security", head)?;
                    } else if let Some(head) = head {
                        merge_or_park(d, task, branch, head, feature(&task.title))?;
                    }
                }
                _ => match gates {
                    None => {
                        s.land(task.id)?;
                        say(d, &Card::new("✅", format!("Done: {name}"), o.summary.clone()).detail(more.trim()));
                    }
                    Some(Gates { failure: Some(f), .. }) => {
                        (d.audit)("gate.failed", json!({ "task": task.id, "branch": branch, "why": crate::hook::truncate(&f, 300) }));
                        fix(d, task, branch, &task.title, &f)?;
                    }
                    Some(Gates { failure: None, report, docs, head }) => {
                        let brief = format!(
                            "Judge task #{} by the {} (`{owner}`).\n\nThe spec:\n{}\n\nWhat the worker claims:\n{}\n\n{report}{}",
                            task.id,
                            personas::find(&owner).title,
                            task.spec,
                            o.summary,
                            if docs { "\n\nThis change is documentation only." } else { "" }
                        );
                        next("evaluator", format!("Evaluate: {}", feature(&task.title)), &brief, "eval", head.as_deref())?;
                        say(d, &Card::new("✅", format!("Built: {name}"), o.summary.clone()).detail("The checks passed. Next: evaluation and reviews.").detail(more.trim()));
                    }
                },
            }
        }
        Kind::Blocked => {
            s.set_status(task.id, "blocked", Some(&o.summary), None, now)?;
            if let Some((kind, detail)) = &o.approval {
                s.wait_for_founder(task.id, None, Some(&format!("{kind}: {detail}")), now)?;
                let card = Card::new("🔐", format!("#{} needs your approval", task.id), format!("It wants to {kind}: {detail}"))
                    .why(o.summary.clone())
                    .detail(format!("Task: {}", task.title))
                    .actions(vec![("✅ Approve".into(), format!("/approve {}", task.id)), ("❌ Deny".into(), format!("/deny {}", task.id))]);
                say(d, &card);
            } else {
                let ask = o.needs_founder.clone().unwrap_or_else(|| worker::Ask { question: o.summary.clone(), ..Default::default() });
                s.wait_for_founder_on(task.id, Some(&ask.stored()), None, ask.hard_line, now)?;
                let mut card = Card::new("❓", format!("#{} {}", task.id, task.title), ask.question.clone())
                    .why(ask.why.clone())
                    .choices(ask.options.clone(), ask.recommend.clone())
                    .task(task.id)
                    .reply(if ask.options.is_empty() { format!("/answer {} <your answer>", task.id) } else { format!("tap a button, or /answer {} <your answer>", task.id) });
                if ask.hard_line {
                    card = card.detail(format!("⚠️ Choosing against the recommendation crosses a hard line. To do it anyway, reply: /answer {} override: <your reason>", task.id));
                }
                say(d, &card);
            }
        }
        Kind::Failed => {
            let attempts = s.bump_attempts(task.id)?;
            if attempts >= MAX_ATTEMPTS {
                let q = format!("It failed {attempts} times. Last error: {}", o.summary);
                s.wait_for_founder(task.id, Some(&q), None, now)?;
                let card = Card::new("⛔", format!("#{} failed {attempts} times", task.id), format!("{} is set aside until you say how to go on.", task.title))
                    .why(format!("Last error: {}", o.summary))
                    .reply(format!("/answer {} <what to do>", task.id));
                say(d, &card);
            } else {
                s.set_status(task.id, "queued", Some(&o.summary), None, now)?;
            }
        }
        Kind::Limited => {
            s.set_status(task.id, "queued", None, None, now)?;
            // With a refused window on record, the next tick sleeps until its reset; without one,
            // wait a while and try again.
            if !o.limits.is_some_and(|l| l.refused()) {
                s.put("backoff_until", &(now + BACKOFF_SECS).to_string())?;
            }
        }
    }
    (d.audit)(
        "task.end",
        json!({ "task": task.id, "kind": task.kind, "outcome": o.kind.key(), "verdict": o.verdict, "score": o.score, "summary": o.summary, "branch": branch, "head": task.head, "session": o.session }),
    );
    Ok(())
}

/// Queues a finished task's follow-ups. They run in order, each after the one before has landed,
/// the first after this task's own work. Judges add at most one; repeats of an existing task are
/// dropped, and a full queue takes only the planning roles' tasks.
fn follow_ups(d: &Deps, task: &Task, o: &Outcome) -> anyhow::Result<usize> {
    let s = d.store;
    let mut after = task.id;
    let planner = personas::find(&task.role).tier == Tier::Lead;
    let limit = if task.kind == "work" { o.follow_ups.len() } else { 1 };
    let mut queued = 0;
    for f in o.follow_ups.iter().take(limit) {
        if s.has_similar(&task.product, &f.title)? || (!planner && s.queued(&task.product)? >= MAX_QUEUED) {
            continue;
        }
        let role = personas::resolve(&f.role);
        let brief = format!("{}\n\nQueued by {} in task #{}: {}", f.brief, task.role, task.id, feature(&task.title));
        let new = NewTask { product: &task.product, role: &role, title: &f.title, brief: &brief, priority: task.priority - 1, parent: Some(task.id), after: Some(after), ..Default::default() };
        after = s.add_task(&new, (d.now)())?;
        queued += 1;
    }
    Ok(queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jazari::limits::Plan;
    use crate::jazari::worker::tests::{limit_line, result_line};
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    /// One command the container was asked to run.
    #[derive(Clone, Debug)]
    struct Exec {
        dir: String,
        command: Vec<String>,
        with_token: bool,
    }

    /// Plays back canned worker runs and answers check and bundle runs, keeping what it was asked.
    #[derive(Default)]
    struct Script {
        runs: RefCell<Vec<Vec<String>>>,
        execs: RefCell<Vec<Exec>>,
        /// Exit code and output per check run; passing with no output when empty.
        checks: RefCell<Vec<(i32, String)>>,
        /// The bundle step fails, as when a worker made no commits.
        no_bundle: Cell<bool>,
    }

    impl Script {
        fn of(&self, what: &str) -> Vec<Exec> {
            self.execs.borrow().iter().filter(|e| kind(&e.command) == what).cloned().collect()
        }
    }

    fn kind(command: &[String]) -> &'static str {
        let at = |i: usize| command.get(i).map(String::as_str).unwrap_or("");
        if at(3) == "claude" {
            "claude"
        } else if (at(0) == "sh" && at(2).contains("ln -sfn")) || at(4).contains("check.py") {
            "setup"
        } else if at(1) == "bundle" {
            "bundle"
        } else {
            "checks"
        }
    }

    impl Runner for Script {
        fn run(&self, _c: &str, dir: &str, command: &[String], with_token: bool, line: &mut dyn FnMut(&str)) -> anyhow::Result<i32> {
            self.execs.borrow_mut().push(Exec { dir: dir.into(), command: command.to_vec(), with_token });
            match kind(command) {
                "checks" => {
                    let (code, out) = {
                        let mut c = self.checks.borrow_mut();
                        if c.is_empty() { (0, "ok".to_string()) } else { c.remove(0) }
                    };
                    out.lines().for_each(&mut *line);
                    Ok(code)
                }
                "bundle" => Ok(if self.no_bundle.get() { 128 } else { 0 }),
                "setup" => {
                    if command.get(4).is_some_and(|c| c.contains("check.py")) {
                        line("company/VISION.md: 4 sentences, average 12.0 words, 0 findings");
                    }
                    Ok(0)
                }
                _ => {
                    let lines = {
                        let mut s = self.runs.borrow_mut();
                        if s.is_empty() { Vec::new() } else { s.remove(0) }
                    };
                    lines.iter().for_each(|l| line(l));
                    Ok(0)
                }
            }
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        heads: RefCell<HashMap<String, String>>,
        imports: Cell<u32>,
        ahead: Cell<usize>,
        files: RefCell<Vec<String>>,
        secrets: RefCell<Vec<String>>,
        /// `rev:path` → text; the base's checks script by default.
        shown: RefCell<HashMap<String, String>>,
        stages: RefCell<Vec<anyhow::Result<Stage>>>,
        staged_commits: RefCell<Vec<String>>,
        merged: RefCell<Vec<String>>,
        discarded: Cell<usize>,
        clones: RefCell<Vec<PathBuf>>,
        broken: RefCell<Vec<String>>,
    }

    impl Repo for FakeRepo {
        fn default_branch(&self, _r: &Path) -> anyhow::Result<String> {
            Ok("main".into())
        }
        fn head(&self, _r: &Path, rev: &str) -> Option<String> {
            if rev == "main" {
                return Some("base0".into());
            }
            self.heads.borrow().get(rev.trim_start_matches("refs/heads/")).cloned()
        }
        fn clone_for_task(&self, r: &Path, dest: &Path, _b: &str, _base: &str) -> anyhow::Result<()> {
            anyhow::ensure!(!self.broken.borrow().iter().any(|b| r.to_string_lossy().contains(b.as_str())), "clone failed");
            self.clones.borrow_mut().push(dest.to_path_buf());
            Ok(())
        }
        fn clone_at(&self, _r: &Path, dest: &Path, _c: &str) -> anyhow::Result<()> {
            self.clones.borrow_mut().push(dest.to_path_buf());
            Ok(())
        }
        fn import_bundle(&self, _r: &Path, _b: &Path, branch: &str) -> anyhow::Result<()> {
            self.imports.set(self.imports.get() + 1);
            self.heads.borrow_mut().insert(branch.into(), format!("sha{}", self.imports.get()));
            Ok(())
        }
        fn commits_ahead(&self, _r: &Path, _b: &str, _br: &str) -> usize {
            self.ahead.get()
        }
        fn changed_files(&self, _r: &Path, _b: &str, _br: &str) -> Vec<String> {
            self.files.borrow().clone()
        }
        fn secrets(&self, _r: &Path, _b: &str, _br: &str) -> Vec<String> {
            self.secrets.borrow().clone()
        }
        fn show(&self, _r: &Path, rev: &str, path: &str) -> Option<String> {
            let key = format!("{rev}:{path}");
            let shown = self.shown.borrow();
            shown.get(&key).cloned().or_else(|| (rev == "main" && path == CHECKS && !shown.contains_key("main:none")).then(|| "make test".to_string()))
        }
        fn stage(&self, _r: &Path, _b: &str, commit: &str, _m: &str) -> anyhow::Result<Stage> {
            self.staged_commits.borrow_mut().push(commit.into());
            let mut s = self.stages.borrow_mut();
            if s.is_empty() { Ok(Stage::Ready(Staged::for_tests("abc123def456"))) } else { s.remove(0) }
        }
        fn advance(&self, _r: &Path, _b: &str, s: &Staged) -> anyhow::Result<String> {
            self.merged.borrow_mut().push(s.new.clone());
            Ok(s.new.clone())
        }
        fn discard(&self, _r: &Path, _s: &Staged) {
            self.discarded.set(self.discarded.get() + 1);
        }
    }

    struct World {
        store: Store,
        runner: Script,
        repo: FakeRepo,
        now: Cell<f64>,
        paused: Cell<bool>,
        resets: Cell<u32>,
        sent: RefCell<Vec<(String, usize)>>,
        audit: RefCell<Vec<String>>,
    }

    impl World {
        fn new(runs: Vec<Vec<String>>) -> Self {
            let store = Store::memory();
            store.add_product("shop", "/repos/shop", 0.0).unwrap();
            let runner = Script::default();
            *runner.runs.borrow_mut() = runs;
            let repo = FakeRepo::default();
            repo.ahead.set(1);
            repo.files.borrow_mut().push("src/login.rs".into());
            World { store, runner, repo, now: Cell::new(1000.0), paused: Cell::new(false), resets: Cell::new(0), sent: RefCell::default(), audit: RefCell::default() }
        }

        fn tick(&self) -> Tick {
            let now = || self.now.get();
            let paused = || self.paused.get();
            let work = |p: &str| PathBuf::from(format!("/data/jazari/work/{p}"));
            let ensure = |p: &str, _w: &str| Ok(format!("jazari-{p}"));
            let reset = |_c: &str| {
                self.resets.set(self.resets.get() + 1);
                Ok(())
            };
            let skills = |_w: &Path| Ok(());
            let notify = |t: &str, b: &[Button]| self.sent.borrow_mut().push((t.to_string(), b.len()));
            let audit = |k: &str, _v: serde_json::Value| self.audit.borrow_mut().push(k.to_string());
            let d = Deps {
                store: &self.store,
                runner: &self.runner,
                repo: &self.repo,
                profile: Plan::Max5.profile(),
                now: &now,
                paused: &paused,
                work: &work,
                ensure: &ensure,
                reset: &reset,
                skills: &skills,
                notify: &notify,
                audit: &audit,
            };
            tick(&d).unwrap()
        }

        fn add(&self, title: &str) -> i64 {
            let t = NewTask { product: "shop", role: "backend-engineer", title, brief: "Accept: 5 tries a minute.", priority: 2, ..Default::default() };
            self.store.add_task(&t, 0.0).unwrap()
        }

        fn next(&self) -> Task {
            self.store.next_task().unwrap().expect("a queued task")
        }

        fn last(&self) -> String {
            self.sent.borrow().last().map(|s| s.0.clone()).unwrap_or_default()
        }

        fn said(&self, text: &str) -> bool {
            self.sent.borrow().iter().any(|(t, _)| t.contains(text))
        }
    }

    fn done(extra: serde_json::Value) -> Vec<String> {
        let mut v = json!({"status": "done", "summary": "Did it."});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        vec![limit_line(0.1, "allowed"), result_line(v)]
    }

    fn approve() -> Vec<String> {
        done(json!({"verdict": "approve", "score": 5}))
    }

    fn judge_task(kind: &'static str, title: &str, head: &str) -> NewTask<'static> {
        let (role, title) = match kind {
            "eval" => ("evaluator", title),
            "review" => ("code-reviewer", title),
            _ => ("appsec-engineer", title),
        };
        let title: &'static str = Box::leak(title.to_string().into_boxed_str());
        let head: &'static str = Box::leak(head.to_string().into_boxed_str());
        NewTask { product: "shop", role, title, kind, branch: Some("jazari/task-1"), owner_role: Some("frontend-engineer"), spec: "S", head: Some(head), priority: 50, ..Default::default() }
    }

    #[test]
    fn work_runs_in_a_clone_comes_back_as_a_bundle_and_passes_every_gate_before_its_approved_commit_merges() {
        let w = World::new(vec![done(json!({"follow_ups": [{"role": "qa-engineer", "title": "Test it"}]})), approve(), approve(), approve()]);
        assert_eq!(w.tick(), Tick::Idle);
        let id = w.add("Build login");
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Done });
        let claude = w.runner.of("claude");
        assert_eq!(claude[0].dir, "/work/task-1", "the worker runs in its own clone, by its container path");
        assert!(claude[0].with_token);
        let bundle = &w.runner.of("bundle")[0];
        assert!(!bundle.with_token && bundle.command.contains(&"jazari/task-1".to_string()) && bundle.command.contains(&"^base0".to_string()));
        assert!(bundle.command.contains(&"/work/task-1/.jazari-out.bundle".to_string()));
        assert_eq!(w.repo.clones.borrow()[0], PathBuf::from("/data/jazari/work/shop").join("task-1"), "the host makes the clone at its host path");
        let checks = w.runner.of("checks");
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].dir, "/work/check-1", "the checks run in the container, on a fresh clone");
        assert!(!checks[0].with_token, "the checks never see the login token");
        assert!(checks[0].command.last().unwrap().ends_with("make test"), "the base branch's checks script runs");
        assert!(w.resets.get() >= 2, "the container restarts before the worker and before the checks");
        let setup = w.runner.of("setup");
        assert!(setup[0].command[2].contains("ln -sfn /work/.skills"), "the skills are linked into the worker's Claude folder");
        assert!(!setup[0].with_token);
        let eval = w.next();
        assert_eq!((eval.kind.as_str(), eval.role.as_str(), eval.head.as_deref()), ("eval", "evaluator", Some("sha1")), "the checked commit is pinned");
        assert!(eval.brief.contains("Accept: 5 tries a minute.") && eval.brief.contains("`company/checks.sh`: passed (the base branch's script)"));
        w.tick();
        assert!(w.runner.of("claude").last().unwrap().command.contains(&"--allowedTools".to_string()), "the evaluator can't write");
        assert_eq!(w.runner.of("bundle").len(), 1, "a judge's clone is never read back");
        let review = w.next();
        assert_eq!((review.kind.as_str(), review.head.as_deref()), ("review", Some("sha1")));
        w.tick();
        assert!(w.runner.of("claude").last().unwrap().command.contains(&"--allowedTools".to_string()), "reviewers are read-only too");
        assert_eq!(w.next().kind, "security");
        w.tick();
        assert_eq!(*w.repo.staged_commits.borrow(), vec!["sha1"], "the approved commit is merged, not the branch name");
        assert_eq!(*w.repo.merged.borrow(), vec!["abc123def456"]);
        assert_eq!(w.runner.of("checks").last().unwrap().dir, "/work/merge", "the checks ran on the merged result");
        assert!(w.last().starts_with("🚀 Merged: Build login\nIt is now on main."), "{}", w.last());
        let next = w.next();
        assert_eq!((next.title.as_str(), next.after_task), ("Test it", Some(id)));
    }

    #[test]
    fn a_checks_script_runs_in_the_shell_its_first_line_names() {
        assert_eq!(shell("#!/usr/bin/env bash\nset -euo pipefail"), "bash");
        assert_eq!(shell("#!/bin/bash -e\n"), "bash");
        assert_eq!(shell("#!/bin/sh\n"), "sh");
        assert_eq!(shell("pytest -q"), "sh");
        assert_eq!(shell("# not bash\n"), "sh");
    }

    #[test]
    fn a_branch_that_moved_after_approval_is_not_merged() {
        let w = World::new(vec![approve()]);
        w.repo.heads.borrow_mut().insert("jazari/task-1".into(), "sha-new".into());
        w.store.add_task(&judge_task("security", "Security review: Login", "sha-approved"), 0.0).unwrap();
        w.tick();
        assert!(w.repo.staged_commits.borrow().is_empty() && w.runner.of("claude").is_empty(), "the stale stage didn't run or merge");
        assert!(w.audit.borrow().contains(&"stage.skipped".to_string()));
    }

    #[test]
    fn the_base_checks_script_judges_a_branch_and_a_new_script_is_announced_and_fully_reviewed() {
        let w = World::new(vec![done(json!({})), approve(), approve(), approve()]);
        w.repo.shown.borrow_mut().insert("main:none".into(), String::new());
        w.repo.shown.borrow_mut().insert("sha1:company/checks.sh".into(), "pytest".into());
        *w.repo.files.borrow_mut() = vec!["company/checks.sh".into(), "README.md".into()];
        w.add("Skeleton");
        w.tick();
        assert!(w.runner.of("checks")[0].command.last().unwrap().ends_with("pytest"), "with no base script, the branch's first one runs");
        assert!(w.said("🛡 Task #1 adds the quality checks"), "{:?}", w.sent.borrow());
        assert!(w.next().brief.contains("the branch's own new script"));
        w.tick();
        assert_eq!(w.next().kind, "review", "a checks.sh change is not documentation");
        w.tick();
        assert_eq!(w.next().kind, "security");
    }

    #[test]
    fn failing_checks_and_secrets_go_back_to_the_builder_without_spending_an_evaluation() {
        let w = World::new(vec![done(json!({})), done(json!({}))]);
        w.runner.checks.borrow_mut().push((1, "FAILED test_login - assert 401 == 200".into()));
        w.add("Build login");
        w.tick();
        let fix = w.next();
        assert_eq!((fix.title.as_str(), fix.round, fix.kind.as_str(), fix.branch.as_deref()), ("Fix: Build login", 1, "work", Some("jazari/task-1")));
        assert!(fix.brief.contains("assert 401 == 200") && fix.brief.contains("The original spec:\nAccept: 5 tries a minute."));
        w.repo.secrets.borrow_mut().push("config.py".into());
        w.tick();
        let fix2 = w.next();
        assert!(fix2.brief.contains("secret scan found credentials in: config.py") && fix2.round == 2);
        assert!(w.runner.of("claude").iter().all(|e| !e.command.contains(&"--allowedTools".to_string())), "no evaluator ran");
    }

    #[test]
    fn a_missing_checks_script_is_required_for_code_but_not_for_docs_and_no_commits_means_nothing_to_judge() {
        let w = World::new(vec![done(json!({}))]);
        w.repo.shown.borrow_mut().insert("main:none".into(), String::new());
        w.add("Build login");
        w.tick();
        assert!(w.next().brief.contains("This repo has no `company/checks.sh`"));
        let w = World::new(vec![done(json!({})), approve()]);
        w.repo.shown.borrow_mut().insert("main:none".into(), String::new());
        *w.repo.files.borrow_mut() = vec!["company/VISION.md".into()];
        w.add("Write the vision");
        w.tick();
        let eval = w.next();
        assert!(eval.brief.contains("not set up") && eval.brief.contains("documentation only"));
        let plain = w.runner.of("setup").into_iter().find(|e| e.command.get(4).is_some_and(|c| c.contains("check.py"))).expect("the Easy English checker ran");
        assert!(plain.command.contains(&"company/VISION.md".to_string()) && plain.dir == "/work/check-1" && !plain.with_token);
        assert!(eval.brief.contains("The Easy English checker on the changed docs"));
        w.tick();
        assert_eq!(w.repo.merged.borrow().len(), 1, "docs merge after the evaluator alone");
        let w = World::new(vec![done(json!({}))]);
        w.runner.no_bundle.set(true);
        let id = w.add("Look into it");
        w.tick();
        assert_eq!(w.repo.imports.get(), 0);
        assert_eq!(w.store.next_task().unwrap(), None);
        assert!(w.last().starts_with(&format!("✅ Done: #{id} Look into it")), "{}", w.last());
    }

    #[test]
    fn a_failed_evaluation_sends_the_criteria_back_and_repeated_failures_upgrade_the_role() {
        let fail = || done(json!({"verdict": "changes", "score": 2, "summary": "No test for lockout.",
            "criteria": [{"name": "Tests prove the behavior", "pass": false}, {"name": "Scope", "pass": true}]}));
        let w = World::new(vec![]);
        w.repo.heads.borrow_mut().insert("jazari/task-1".into(), "h".into());
        for i in 0..6 {
            *w.runner.runs.borrow_mut() = vec![fail()];
            let mut t = judge_task("eval", &format!("Evaluate: F{i}"), "h");
            t.owner_role = Some("backend-engineer");
            w.store.add_task(&t, 0.0).unwrap();
            w.tick();
            let fix = w.store.tasks(None, 100).unwrap().into_iter().find(|t| t.title == format!("Fix: F{i}")).unwrap();
            assert!(fix.brief.contains("The evaluator asked for changes:\nNo test for lockout.\nFailed criteria: Tests prove the behavior"));
            assert_eq!(fix.role, "backend-engineer");
            w.store.set_status(fix.id, "done", None, None, 0.0).unwrap();
        }
        assert!(w.said("📈 Backend Engineer now uses a stronger model\nIt passed only 0 of its last 6 evaluations."));
        assert_eq!(w.store.get("upgrade:backend-engineer").unwrap().as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn checks_failing_on_the_merged_result_hold_main_and_send_a_fix() {
        let w = World::new(vec![approve()]);
        w.repo.heads.borrow_mut().insert("jazari/task-1".into(), "h".into());
        w.runner.checks.borrow_mut().push((1, "conflicting migrations".into()));
        w.store.add_task(&judge_task("security", "Security review: Login", "h"), 0.0).unwrap();
        w.tick();
        assert!(w.repo.merged.borrow().is_empty() && w.repo.discarded.get() == 1);
        let fix = w.next();
        assert_eq!((fix.title.as_str(), fix.role.as_str()), ("Fix: Login", "frontend-engineer"));
        assert!(fix.brief.contains("fails once `main` is merged") && fix.brief.contains("conflicting migrations"));
    }

    #[test]
    fn waiting_and_failed_merges_are_kept_until_they_settle() {
        let w = World::new(vec![approve(), approve()]);
        w.repo.heads.borrow_mut().insert("jazari/task-1".into(), "h".into());
        w.repo.stages.borrow_mut().extend([Ok(Stage::Dirty("/repos/shop".into())), Err(anyhow::anyhow!("disk full")), Err(anyhow::anyhow!("disk full"))]);
        w.store.add_task(&judge_task("security", "Security review: Login", "h"), 0.0).unwrap();
        w.tick();
        assert!(w.last().contains("uncommitted changes") && pending(&w.store).len() == 1);
        w.store.add_task(&judge_task("security", "Security review: Signup", "h"), 0.0).unwrap();
        // The retry of Login fails; Signup's merge then fails too: both are kept, nothing is lost.
        w.tick();
        assert_eq!(pending(&w.store).len(), 2, "{:?}", pending(&w.store));
        assert!(w.said("⚠️ Merge keeps failing: Login\nDisk full"), "{:?}", w.sent.borrow());
        assert_eq!(w.tick(), Tick::Idle);
        assert!(pending(&w.store).is_empty(), "both merged once the error cleared");
        assert_eq!(w.repo.merged.borrow().len(), 2);
    }

    #[test]
    fn a_conflict_sends_the_branch_back_to_its_builder() {
        let w = World::new(vec![approve()]);
        w.repo.heads.borrow_mut().insert("jazari/task-1".into(), "h".into());
        w.repo.stages.borrow_mut().push(Ok(Stage::Conflict));
        w.store.add_task(&judge_task("security", "Security review: Login", "h"), 0.0).unwrap();
        w.tick();
        let fix = w.next();
        assert!(fix.title == "Fix: Login" && fix.brief.contains("no longer merges cleanly") && fix.role == "frontend-engineer");
    }

    #[test]
    fn a_product_that_cant_be_set_up_is_held_and_the_others_go_on() {
        let w = World::new(vec![done(json!({}))]);
        w.store.add_product("site", "/repos/site", 0.0).unwrap();
        w.repo.broken.borrow_mut().push("/repos/shop".into());
        w.runner.no_bundle.set(true);
        let broken = w.add("Build login");
        let fine = w.store.add_task(&NewTask { product: "site", role: "engineer", title: "Landing page", ..Default::default() }, 0.0).unwrap();
        assert_eq!(w.tick(), Tick::Ran { task: fine, kind: Kind::Done });
        assert_eq!(w.store.task(broken).unwrap().unwrap().status, "queued");
        assert!(w.said("⚠️ shop is on hold for 10 minutes") && w.said("Why: Clone failed") && w.said("Other products keep working."));
        assert_eq!(w.tick(), Tick::Idle, "still held");
        w.now.set(1000.0 + HOLD_SECS + 1.0);
        w.repo.broken.borrow_mut().clear();
        *w.runner.runs.borrow_mut() = vec![done(json!({}))];
        assert_eq!(w.tick(), Tick::Ran { task: broken, kind: Kind::Done });
    }

    #[test]
    fn a_plan_s_tasks_run_one_after_another_as_each_lands() {
        let plan = json!({"follow_ups": [{"role": "engineer", "title": "Skeleton"}, {"role": "engineer", "title": "Engine"}]});
        let w = World::new(vec![done(plan), approve(), done(json!({}))]);
        *w.repo.files.borrow_mut() = vec!["company/VISION.md".into()];
        w.store.add_task(&NewTask { product: "shop", role: "chief-of-staff", title: "Plan: notes", priority: 100, ..Default::default() }, 0.0).unwrap();
        w.tick();
        assert_eq!(w.next().title, "Evaluate: Plan: notes", "the plan's tasks wait for it to merge");
        w.tick();
        assert_eq!(w.next().title, "Skeleton");
        w.runner.no_bundle.set(true);
        w.tick();
        assert_eq!(w.next().title, "Engine", "nothing to merge counts as landed");
    }

    #[test]
    fn follow_ups_use_role_keys_skip_repeats_and_judges_add_one() {
        let w = World::new(vec![
            done(json!({"follow_ups": [{"role": "Backend Engineer", "title": "Add CI"}, {"role": "qa", "title": "add ci!"}, {"role": "devops", "title": "Pin deps"}]})),
            done(json!({"verdict": "changes", "follow_ups": [{"role": "engineer", "title": "One"}, {"role": "engineer", "title": "Two"}]})),
        ]);
        w.runner.no_bundle.set(true);
        w.add("Build login");
        w.tick();
        let titles: Vec<_> = w.store.tasks(None, 20).unwrap().into_iter().filter(|t| t.status == "queued").map(|t| (t.title, t.role)).collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&("Add CI".into(), "backend-engineer".into())) && titles.contains(&("Pin deps".into(), "platform-engineer".into())));
        w.repo.heads.borrow_mut().insert("jazari/task-1".into(), "h".into());
        let judge = w.store.add_task(&judge_task("eval", "Evaluate: X", "h"), 0.0).unwrap();
        assert_eq!(w.tick(), Tick::Ran { task: judge, kind: Kind::Done });
        assert!(w.store.has_similar("shop", "One").unwrap() && !w.store.has_similar("shop", "Two").unwrap());
    }

    #[test]
    fn questions_and_approvals_wait_for_the_founder() {
        let w = World::new(vec![
            vec![result_line(json!({"status": "blocked", "summary": "Need a provider.", "needs_founder": "Twilio or MessageBird?"}))],
            vec![result_line(json!({"status": "blocked", "summary": "Ready.", "approval_needed": {"kind": "deploy", "detail": "Deploy v1 to fly.io"}}))],
        ]);
        let q = w.add("SMS login");
        w.tick();
        let t = w.store.task(q).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.question.as_deref()), ("blocked", Some("Twilio or MessageBird?")));
        assert!(w.last().contains(&format!("/answer {q}")));
        let a = w.add("Ship v1");
        w.tick();
        assert_eq!(w.store.task(a).unwrap().unwrap().approval.as_deref(), Some("deploy: Deploy v1 to fly.io"));
        let (text, buttons) = w.sent.borrow().last().unwrap().clone();
        assert!(text.starts_with(&format!("🔐 #{a} needs your approval\nIt wants to deploy: Deploy v1 to fly.io")) && buttons == 2, "{text}");
        assert_eq!(w.tick(), Tick::Idle);
    }

    #[test]
    fn a_failing_task_is_retried_then_set_aside() {
        let w = World::new(vec![vec![], vec![], vec![]]);
        let id = w.add("Flaky");
        for _ in 0..2 {
            assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Failed });
            assert_eq!(w.store.task(id).unwrap().unwrap().status, "queued");
        }
        w.tick();
        assert_eq!(w.store.task(id).unwrap().unwrap().status, "blocked");
        assert_eq!(w.tick(), Tick::Idle);
        assert!(w.last().contains("failed 3 times"));
        w.paused.set(true);
        assert_eq!(w.tick(), Tick::Paused);
    }

    #[test]
    fn a_refused_run_is_requeued_and_jazari_sleeps_until_the_reset_saying_so_once() {
        let w = World::new(vec![vec![limit_line(1.0, "rejected")], done(json!({}))]);
        w.runner.no_bundle.set(true);
        let id = w.add("Build login");
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Limited });
        let t = w.store.task(id).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.attempts), ("queued", 0));
        let Tick::Sleeping { until } = w.tick() else { panic!("should sleep") };
        assert!(until > 9e9);
        assert!(matches!(w.tick(), Tick::Sleeping { .. }));
        assert_eq!(w.sent.borrow().len(), 1, "the sleep is announced once");
        assert!(w.last().contains("5-hour limit is used up"));
        w.now.set(until + 1.0);
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Done });
    }

    #[test]
    fn a_refusal_without_a_reset_time_backs_off() {
        let err = json!({"type": "result", "subtype": "success", "is_error": true, "result": "Claude usage limit reached"}).to_string();
        let w = World::new(vec![vec![err]]);
        w.add("Build login");
        assert!(matches!(w.tick(), Tick::Ran { kind: Kind::Limited, .. }));
        assert_eq!(w.tick(), Tick::Sleeping { until: 1000.0 + BACKOFF_SECS });
        w.now.set(1000.0 + BACKOFF_SECS + 1.0);
        assert!(matches!(w.tick(), Tick::Ran { .. }));
    }
}

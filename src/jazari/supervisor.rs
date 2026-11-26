//! The supervisor: one task at a time, for as long as it runs.
//!
//! Each tick it checks the pause and the usage windows, takes the next queued task, and runs a
//! worker on it in the product's container, in the task's own git worktree. Nothing a worker
//! claims is trusted until it is verified.
//!
//! A branch with new commits goes through these gates in order:
//! 1. **Harness checks.** Deterministic and free: the product's `company/checks.sh` and a secret
//!    scan of the diff.
//! 2. **The evaluator.** A fresh, read-only judge scores the work against the builder's role rubric,
//!    with the check output as evidence.
//! 3. **Code review**, then **security review.** Documentation-only changes skip both.
//! 4. **The merge queue.** The merge is built aside, the checks run on the merged result, and only
//!    then does the default branch move.
//!
//! A failed gate sends a fix task back to the builder; after a few rounds the founder decides.
//! Every judgment is recorded per role. A role that keeps failing evaluation moves up to the
//! plan's lead model.

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
/// Evaluations looked at to decide whether a role needs a stronger model.
const UPGRADE_WINDOW: usize = 8;
/// The product's own checks: build, tests, lint, type checks.
pub const CHECKS: &str = "company/checks.sh";
/// The longest the checks may run, in seconds.
const CHECKS_TIMEOUT_SECS: u64 = 15 * 60;
/// Printed when a repo has no checks script.
const NO_CHECKS: &str = "__jazari_no_checks__";

/// Git on a product's repo, so tests run without one.
pub trait Repo {
    fn default_branch(&self, repo: &Path) -> anyhow::Result<String>;
    fn prepare(&self, repo: &Path, task: i64, branch: &str, base: &str) -> anyhow::Result<PathBuf>;
    fn remove(&self, repo: &Path, worktree: &Path);
    fn commits_ahead(&self, repo: &Path, base: &str, branch: &str) -> usize;
    fn changed_files(&self, repo: &Path, base: &str, branch: &str) -> Vec<String>;
    fn secrets(&self, repo: &Path, base: &str, branch: &str) -> Vec<String>;
    fn stage(&self, repo: &Path, base: &str, branch: &str, message: &str) -> anyhow::Result<Stage>;
    fn advance(&self, repo: &Path, base: &str, staged: &Staged) -> anyhow::Result<String>;
    fn discard(&self, repo: &Path, staged: &Staged);
}

pub struct Git;

impl Repo for Git {
    fn default_branch(&self, repo: &Path) -> anyhow::Result<String> {
        super::git::default_branch(repo)
    }
    fn prepare(&self, repo: &Path, task: i64, branch: &str, base: &str) -> anyhow::Result<PathBuf> {
        super::git::prepare(repo, task, branch, base)
    }
    fn remove(&self, repo: &Path, worktree: &Path) {
        super::git::remove(repo, worktree)
    }
    fn commits_ahead(&self, repo: &Path, base: &str, branch: &str) -> usize {
        super::git::commits_ahead(repo, base, branch)
    }
    fn changed_files(&self, repo: &Path, base: &str, branch: &str) -> Vec<String> {
        super::git::changed_files(repo, base, branch)
    }
    fn secrets(&self, repo: &Path, base: &str, branch: &str) -> Vec<String> {
        super::git::secrets(repo, base, branch)
    }
    fn stage(&self, repo: &Path, base: &str, branch: &str, message: &str) -> anyhow::Result<Stage> {
        super::git::stage(repo, base, branch, message)
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
    /// Starts the product's container and returns its name.
    pub ensure: &'a dyn Fn(&str, &str) -> anyhow::Result<String>,
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

/// Sends the sleep notice once per sleep, not every tick.
fn announce_sleep(d: &Deps, until: f64, reason: &str) {
    let key = format!("{until:.0}");
    if d.store.get("sleep_announced").ok().flatten().as_deref() != Some(key.as_str()) {
        let _ = d.store.put("sleep_announced", &key);
        (d.notify)(&format!("😴 jazari sleeps until {}: {reason}.", clock(until)), &[]);
        (d.audit)("sleep", json!({ "until": until, "reason": reason }));
    }
}

// ---------- the harness's own checks ----------

/// What the harness found by running things itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Checks {
    pub passed: bool,
    /// The repo has no `company/checks.sh`.
    pub missing: bool,
    /// The end of the output, for the evaluator and for fixes.
    pub log: String,
}

/// Runs `company/checks.sh` in a folder of the container.
pub fn run_checks(runner: &dyn Runner, container: &str, dir: &str) -> Checks {
    let script = format!("if [ ! -f {CHECKS} ]; then echo {NO_CHECKS}; exit 3; fi; timeout --kill-after=30 {CHECKS_TIMEOUT_SECS} sh {CHECKS} 2>&1");
    let mut lines: Vec<String> = Vec::new();
    let code = runner.run(container, dir, &["sh".into(), "-c".into(), script], &mut |l| lines.push(l.to_string()));
    let missing = lines.iter().any(|l| l == NO_CHECKS);
    lines.retain(|l| l != NO_CHECKS);
    let tail = lines[lines.len().saturating_sub(60)..].join("\n");
    let log = super::telegram::redact(&crate::hook::truncate(&tail, 4000));
    match code {
        Ok(0) if !missing => Checks { passed: true, missing, log },
        Ok(c) => Checks { passed: false, missing, log: if missing { log } else { format!("exit code {c}\n{log}") } },
        Err(e) => Checks { passed: false, missing, log: format!("the checks couldn't run: {e}") },
    }
}

/// The gates a built branch must pass before an evaluator spends tokens on it.
struct Gates {
    /// What to fix, when a gate failed.
    failure: Option<String>,
    /// The evidence handed to the evaluator.
    report: String,
    docs: bool,
}

fn gates(d: &Deps, task: &Task, container: &str, worktree: &Path, repo: &Path, base: &str, branch: &str) -> Gates {
    let files = d.repo.changed_files(repo, base, branch);
    let docs = super::git::docs_only(&files);
    let secrets = d.repo.secrets(repo, base, branch);
    let checks = run_checks(d.runner, container, &worktree.to_string_lossy());
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
    let status = if checks.missing { "not set up (documentation-only change)".to_string() } else if checks.passed { "passed".into() } else { "FAILED".into() };
    let report = format!(
        "Evidence the harness gathered itself (trust this over the worker's claims):\n- `{CHECKS}`: {status}\n- secret scan: {}\n- changed files: {}\n```\n{}\n```",
        if secrets.is_empty() { "clean" } else { "credentials found" },
        crate::hook::truncate(&files.join(", "), 600),
        checks.log
    );
    Gates { failure, report, docs }
}

// ---------- merges ----------

/// A merge waiting for the founder's checkout to be clean.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct Pending {
    task: i64,
    branch: String,
    title: String,
}

fn pending(store: &Store) -> Vec<Pending> {
    store.get("pending_merges").ok().flatten().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn set_pending(store: &Store, list: &[Pending]) -> anyhow::Result<()> {
    store.put("pending_merges", &serde_json::to_string(list)?)
}

/// Tries the merges that waited for a clean checkout; costs no tokens.
fn retry_merges(d: &Deps) -> anyhow::Result<()> {
    let list = pending(d.store);
    if list.is_empty() {
        return Ok(());
    }
    set_pending(d.store, &[])?;
    for p in list {
        if let Some(task) = d.store.task(p.task)? {
            merge(d, &task, &p.branch, &p.title)?;
        }
    }
    Ok(())
}

/// The merge queue: builds the merge aside, runs the checks on the merged result, and moves the
/// default branch only when they pass.
fn merge(d: &Deps, task: &Task, branch: &str, title: &str) -> anyhow::Result<()> {
    let product = d.store.product(&task.product)?.ok_or_else(|| anyhow::anyhow!("no product {}", task.product))?;
    let repo = PathBuf::from(&product.repo);
    let base = d.repo.default_branch(&repo)?;
    match d.repo.stage(&repo, &base, branch, &format!("Merge {branch}: {title}"))? {
        Stage::Ready(staged) => {
            let container = (d.ensure)(&product.name, &product.repo)?;
            let checks = run_checks(d.runner, &container, &staged.dir.to_string_lossy());
            if !checks.passed && !checks.missing {
                d.repo.discard(&repo, &staged);
                judged(d, task, "merge", false, None, "checks fail on the merged result")?;
                let findings = format!(
                    "The branch passes on its own, but `{CHECKS}` fails once `{base}` is merged into it:\n```\n{}\n```\nMerge `{base}` into the branch and make the checks pass.",
                    checks.log
                );
                fix(d, task, branch, title, &findings)?;
                return Ok(());
            }
            let sha = d.repo.advance(&repo, &base, &staged)?;
            d.store.land_branch(branch)?;
            (d.audit)("merge", json!({ "task": task.id, "branch": branch, "base": base, "commit": sha }));
            (d.notify)(&format!("🚀 Merged into {base}: {title}\n{branch} · {}", &sha[..sha.len().min(10)]), &[]);
        }
        Stage::Conflict => {
            let findings = format!("`{branch}` no longer merges cleanly into `{base}`. Merge `{base}` into the branch, resolve the conflicts keeping both sides' intent, and make the checks pass.");
            fix(d, task, branch, title, &findings)?;
        }
        Stage::Dirty(dir) => {
            let mut list = pending(d.store);
            list.push(Pending { task: task.id, branch: branch.into(), title: title.into() });
            set_pending(d.store, &list)?;
            let told = format!("dirty_told:{branch}");
            if d.store.get(&told)?.is_none() {
                d.store.put(&told, "1")?;
                (d.notify)(&format!("⏸ {title} is approved, but {base} in {} has uncommitted changes. jazari merges it once they are committed or stashed.", dir.display()), &[]);
            }
        }
    }
    Ok(())
}

// ---------- the tick ----------

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

pub fn tick(d: &Deps) -> anyhow::Result<Tick> {
    if (d.paused)() {
        return Ok(Tick::Paused);
    }
    retry_merges(d)?;
    let now = (d.now)();
    if let Some(until) = d.store.get("backoff_until")?.and_then(|t| t.parse::<f64>().ok()).filter(|t| *t > now) {
        announce_sleep(d, until, "a usage limit refused the last run");
        return Ok(Tick::Sleeping { until });
    }
    if let Decision::Sleep { until, reason } = limits::decide(&limits(d.store), &d.profile, now) {
        announce_sleep(d, until, &reason);
        return Ok(Tick::Sleeping { until });
    }
    let Some(task) = d.store.next_task()? else { return Ok(Tick::Idle) };
    let product = d.store.product(&task.product)?.ok_or_else(|| anyhow::anyhow!("task #{} names a missing product", task.id))?;
    let repo = PathBuf::from(&product.repo);
    let branch = worker::branch(&task);
    let base = d.repo.default_branch(&repo)?;
    d.store.set_status(task.id, "running", None, Some(&branch), now)?;
    // Without a container or a worktree nothing can run: hold the queue rather than burn attempts.
    let ready = (d.ensure)(&product.name, &product.repo).and_then(|c| Ok((c, d.repo.prepare(&repo, task.id, &branch, &base)?)));
    let (container, worktree) = match ready {
        Ok(r) => r,
        Err(e) => {
            d.store.set_status(task.id, "queued", None, None, now)?;
            return Err(e);
        }
    };
    (d.audit)("task.start", json!({ "task": task.id, "product": task.product, "role": task.role, "kind": task.kind, "title": task.title, "branch": branch }));
    let persona = personas::find(&task.role);
    let judge = task.kind == "eval";
    let system_prompt = if judge {
        let builder = personas::find(&owner(&task));
        format!("{}\n# How to score\n\n{}\n\n# The rubric for the {}\n\n{}\n", persona.system_prompt(), personas::scoring_rules(), builder.title, builder.rubric())
    } else {
        persona.system_prompt()
    };
    let workdir = worktree.to_string_lossy().to_string();
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
    let outcome = worker::run(d.runner, &task, &setup);
    let built = outcome.kind == Kind::Done && task.kind == "work" && d.repo.commits_ahead(&repo, &base, &branch) > 0;
    let gates = built.then(|| gates(d, &task, &container, &worktree, &repo, &base, &branch));
    let docs = super::git::docs_only(&d.repo.changed_files(&repo, &base, &branch));
    d.repo.remove(&repo, &worktree);
    record(d, &task, &outcome, gates, docs, &branch, now)?;
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
        },
        (d.now)(),
    )?;
    if round >= MAX_ROUNDS {
        let q = format!("`{branch}` failed its gates {round} times. Latest: {}", crate::hook::truncate(findings, 500));
        d.store.wait_for_founder(id, Some(&q), None, (d.now)())?;
        (d.notify)(&format!("❓ #{id} {}\n{q}\nReply /answer {id} <guidance> to try again.", feature(title)), &[]);
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
        (d.notify)(&format!("📈 {} passed only {passed} of its last {} evaluations; it now runs on {}.", persona.title, recent.len(), d.profile.lead_model), &[]);
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
    let next = |role: &str, title: String, brief: &str, kind: &str| {
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
                            (d.notify)(&warning, &[]);
                        }
                    }
                    if !approved {
                        let who = match task.kind.as_str() {
                            "eval" => "evaluator",
                            "review" => "code review",
                            _ => "security review",
                        };
                        let criteria = if failed.is_empty() { String::new() } else { format!("\nFailed criteria: {failed}") };
                        fix(d, task, branch, &task.title, &format!("The {who} asked for changes:\n{}{criteria}", o.summary))?;
                    } else if task.kind == "eval" && !docs {
                        next("code-reviewer", format!("Review: {}", feature(&task.title)), "", "review")?;
                    } else if task.kind == "review" && !docs {
                        next("appsec-engineer", format!("Security review: {}", feature(&task.title)), "", "security")?;
                    } else {
                        merge(d, task, branch, feature(&task.title))?;
                    }
                }
                _ => match gates {
                    None => {
                        s.land(task.id)?;
                        (d.notify)(&format!("✅ {name}\n{}{more}", o.summary), &[]);
                    }
                    Some(Gates { failure: Some(f), .. }) => {
                        (d.audit)("gate.failed", json!({ "task": task.id, "branch": branch, "why": crate::hook::truncate(&f, 300) }));
                        fix(d, task, branch, &task.title, &f)?;
                    }
                    Some(Gates { failure: None, report, docs }) => {
                        let brief = format!(
                            "Judge task #{} by the {} (`{owner}`).\n\nThe spec:\n{}\n\nWhat the worker claims:\n{}\n\n{report}{}",
                            task.id,
                            personas::find(&owner).title,
                            task.spec,
                            o.summary,
                            if docs { "\n\nThis change is documentation only." } else { "" }
                        );
                        next("evaluator", format!("Evaluate: {}", feature(&task.title)), &brief, "eval")?;
                        (d.notify)(&format!("✅ {name}\n{}\nChecks passed; now evaluated and reviewed.{more}", o.summary), &[]);
                    }
                },
            }
        }
        Kind::Blocked => {
            s.set_status(task.id, "blocked", Some(&o.summary), None, now)?;
            if let Some((kind, detail)) = &o.approval {
                s.wait_for_founder(task.id, None, Some(&format!("{kind}: {detail}")), now)?;
                let buttons = [("✅ Approve".to_string(), format!("/approve {}", task.id)), ("❌ Deny".to_string(), format!("/deny {}", task.id))];
                (d.notify)(&format!("🔐 {name} needs your approval to {kind}:\n{detail}\n\n{}", o.summary), &buttons);
            } else {
                let q = o.needs_founder.clone().unwrap_or_else(|| o.summary.clone());
                s.wait_for_founder(task.id, Some(&q), None, now)?;
                (d.notify)(&format!("❓ {name}\n{q}\nReply /answer {} <your answer>", task.id), &[]);
            }
        }
        Kind::Failed => {
            let attempts = s.bump_attempts(task.id)?;
            if attempts >= MAX_ATTEMPTS {
                let q = format!("It failed {attempts} times. Last error: {}", o.summary);
                s.wait_for_founder(task.id, Some(&q), None, now)?;
                (d.notify)(&format!("⛔ {name}\n{q}\nReply /answer {} <guidance> to try again.", task.id), &[]);
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
        json!({ "task": task.id, "kind": task.kind, "outcome": o.kind.key(), "verdict": o.verdict, "score": o.score, "summary": o.summary, "branch": branch, "session": o.session }),
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

    /// Plays back canned worker runs and answers check runs, keeping what it was asked.
    #[derive(Default)]
    struct Script {
        runs: RefCell<Vec<Vec<String>>>,
        prompts: RefCell<Vec<String>>,
        systems: RefCell<Vec<String>>,
        read_only: RefCell<Vec<bool>>,
        /// Exit code and output per check run; passing with no output when empty.
        checks: RefCell<Vec<(i32, String)>>,
        check_dirs: RefCell<Vec<String>>,
    }

    impl Runner for Script {
        fn run(&self, _c: &str, dir: &str, cmd: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<i32> {
            if cmd[0] == "sh" {
                self.check_dirs.borrow_mut().push(dir.into());
                let (code, out) = { let mut c = self.checks.borrow_mut(); if c.is_empty() { (0, "ok".to_string()) } else { c.remove(0) } };
                out.lines().for_each(&mut *line);
                return Ok(code);
            }
            let arg = |k: &str| cmd.iter().position(|a| a == k).map(|i| cmd[i + 1].clone()).unwrap_or_default();
            self.prompts.borrow_mut().push(arg("-p"));
            self.systems.borrow_mut().push(arg("--append-system-prompt"));
            self.read_only.borrow_mut().push(cmd.iter().any(|a| a == "--allowedTools"));
            let lines = { let mut s = self.runs.borrow_mut(); if s.is_empty() { Vec::new() } else { s.remove(0) } };
            lines.iter().for_each(|l| line(l));
            Ok(0)
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        ahead: Cell<usize>,
        files: RefCell<Vec<String>>,
        secrets: RefCell<Vec<String>>,
        stages: RefCell<Vec<Stage>>,
        merged: RefCell<Vec<String>>,
        discarded: Cell<usize>,
    }

    fn staged() -> Staged {
        Staged::for_tests("/repos/shop/.jazari/merge", "abc123def456")
    }

    impl Repo for FakeRepo {
        fn default_branch(&self, _r: &Path) -> anyhow::Result<String> {
            Ok("main".into())
        }
        fn prepare(&self, r: &Path, task: i64, _b: &str, _base: &str) -> anyhow::Result<PathBuf> {
            Ok(PathBuf::from(format!("{}/wt-{task}", r.display())))
        }
        fn remove(&self, _r: &Path, _w: &Path) {}
        fn commits_ahead(&self, _r: &Path, _b: &str, _br: &str) -> usize {
            self.ahead.get()
        }
        fn changed_files(&self, _r: &Path, _b: &str, _br: &str) -> Vec<String> {
            self.files.borrow().clone()
        }
        fn secrets(&self, _r: &Path, _b: &str, _br: &str) -> Vec<String> {
            self.secrets.borrow().clone()
        }
        fn stage(&self, _r: &Path, _b: &str, _br: &str, _m: &str) -> anyhow::Result<Stage> {
            let mut s = self.stages.borrow_mut();
            Ok(if s.is_empty() { Stage::Ready(staged()) } else { s.remove(0) })
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
            World { store, runner, repo, now: Cell::new(1000.0), paused: Cell::new(false), sent: RefCell::default(), audit: RefCell::default() }
        }

        fn tick(&self) -> Tick {
            let now = || self.now.get();
            let paused = || self.paused.get();
            let ensure = |p: &str, _r: &str| Ok(format!("jazari-{p}"));
            let notify = |t: &str, b: &[Button]| self.sent.borrow_mut().push((t.to_string(), b.len()));
            let audit = |k: &str, _v: serde_json::Value| self.audit.borrow_mut().push(k.to_string());
            let d = Deps {
                store: &self.store,
                runner: &self.runner,
                repo: &self.repo,
                profile: Plan::Max5.profile(),
                now: &now,
                paused: &paused,
                ensure: &ensure,
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

    #[test]
    fn built_work_is_checked_evaluated_reviewed_secured_then_merged_after_checks_on_the_merge() {
        let w = World::new(vec![done(json!({"follow_ups": [{"role": "qa-engineer", "title": "Test it"}]})), approve(), approve(), approve()]);
        assert_eq!(w.tick(), Tick::Idle);
        let id = w.add("Build login");
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Done });
        assert_eq!(*w.runner.check_dirs.borrow(), vec!["/repos/shop/wt-1"], "the harness ran the checks on the branch");
        assert!(w.last().contains("Checks passed") && w.last().contains("+1 follow-up"));
        let eval = w.next();
        assert_eq!((eval.kind.as_str(), eval.role.as_str(), eval.title.as_str(), eval.owner_role.as_deref()), ("eval", "evaluator", "Evaluate: Build login", Some("backend-engineer")));
        assert!(eval.brief.contains("Accept: 5 tries a minute.") && eval.brief.contains("What the worker claims:\nDid it.") && eval.brief.contains("`company/checks.sh`: passed"));
        w.tick();
        assert!(*w.runner.read_only.borrow().last().unwrap(), "the evaluator can't write");
        let system = w.runner.systems.borrow().last().unwrap().clone();
        assert!(system.contains("# The rubric for the Backend Engineer") && system.contains("## code") && system.contains("5: ready to ship"));
        assert_eq!(w.next().kind, "review");
        w.tick();
        assert_eq!(w.next().kind, "security");
        w.tick();
        assert_eq!(*w.repo.merged.borrow(), vec!["abc123def456"]);
        assert_eq!(w.runner.check_dirs.borrow().last().unwrap(), "/repos/shop/.jazari/merge", "the checks ran on the merged result");
        assert!(w.last().starts_with("🚀 Merged into main: Build login"), "{}", w.last());
        let next = w.next();
        assert_eq!((next.title.as_str(), next.after_task), ("Test it", Some(id)));
        let q = w.store.quality().unwrap();
        assert!(q.iter().any(|(r, s, n, p, _)| r == "backend-engineer" && s == "eval" && *n == 1 && *p == 1));
        assert!(q.iter().any(|(r, s, ..)| r == "backend-engineer" && s == "checks"));
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
        assert_eq!(fix.spec, "Accept: 5 tries a minute.");
        w.repo.secrets.borrow_mut().push("config.py".into());
        w.tick();
        let fix2 = w.next();
        assert!(fix2.brief.contains("secret scan found credentials in: config.py") && fix2.round == 2);
        assert!(w.runner.read_only.borrow().iter().all(|r| !r), "no evaluator ran");
    }

    #[test]
    fn a_missing_checks_script_is_required_for_code_but_not_for_docs() {
        let w = World::new(vec![done(json!({})), done(json!({}))]);
        w.runner.checks.borrow_mut().extend([(3, NO_CHECKS.to_string()), (3, NO_CHECKS.to_string())]);
        w.add("Build login");
        w.tick();
        assert!(w.next().brief.contains("This repo has no `company/checks.sh`"));
        let w = World::new(vec![done(json!({})), approve()]);
        w.runner.checks.borrow_mut().push((3, NO_CHECKS.to_string()));
        *w.repo.files.borrow_mut() = vec!["company/VISION.md".into()];
        w.add("Write the vision");
        w.tick();
        let eval = w.next();
        assert!(eval.brief.contains("not set up") && eval.brief.contains("documentation only"));
        w.tick();
        assert_eq!(w.repo.merged.borrow().len(), 1, "docs merge after the evaluator alone");
    }

    #[test]
    fn a_failed_evaluation_sends_the_criteria_back_and_repeated_failures_upgrade_the_role() {
        let fail = || done(json!({"verdict": "changes", "score": 2, "summary": "No test for lockout.",
            "criteria": [{"name": "Tests prove the behavior", "pass": false}, {"name": "Scope", "pass": true}]}));
        let w = World::new(vec![]);
        for i in 0..6 {
            *w.runner.runs.borrow_mut() = vec![fail()];
            let t = NewTask { product: "shop", role: "evaluator", title: &format!("Evaluate: F{i}"), priority: 50, kind: "eval", branch: Some("b"), owner_role: Some("backend-engineer"), spec: "S", ..Default::default() };
            w.store.add_task(&t, 0.0).unwrap();
            w.tick();
            let fix = w.store.tasks(None, 100).unwrap().into_iter().find(|t| t.title == format!("Fix: F{i}")).unwrap();
            assert!(fix.brief.contains("The evaluator asked for changes:\nNo test for lockout.\nFailed criteria: Tests prove the behavior"));
            assert_eq!(fix.role, "backend-engineer");
            w.store.set_status(fix.id, "done", None, None, 0.0).unwrap();
        }
        assert!(w.last().contains("📈 Backend Engineer passed only 0 of its last 6 evaluations"), "{}", w.last());
        assert_eq!(w.store.get("upgrade:backend-engineer").unwrap().as_deref(), Some("claude-opus-5-5"));
        let d_model = {
            let p = personas::find("backend-engineer");
            let now = || 0.0;
            let paused = || false;
            let ensure = |_: &str, _: &str| Ok(String::new());
            let notify = |_: &str, _: &[Button]| {};
            let audit = |_: &str, _: serde_json::Value| {};
            let d = Deps { store: &w.store, runner: &w.runner, repo: &w.repo, profile: Plan::Max5.profile(), now: &now, paused: &paused, ensure: &ensure, notify: &notify, audit: &audit };
            model_for(&d, &p)
        };
        assert_eq!(d_model, "claude-opus-5-5");
    }

    #[test]
    fn checks_failing_on_the_merged_result_hold_main_and_send_a_fix() {
        let w = World::new(vec![approve()]);
        w.runner.checks.borrow_mut().push((1, "conflicting migrations".into()));
        let t = NewTask { product: "shop", role: "appsec-engineer", title: "Security review: Login", kind: "security", branch: Some("jazari/task-1"), owner_role: Some("frontend-engineer"), spec: "S", ..Default::default() };
        w.store.add_task(&t, 0.0).unwrap();
        w.tick();
        assert!(w.repo.merged.borrow().is_empty() && w.repo.discarded.get() == 1);
        let fix = w.next();
        assert_eq!((fix.title.as_str(), fix.role.as_str()), ("Fix: Login", "frontend-engineer"));
        assert!(fix.brief.contains("fails once `main` is merged") && fix.brief.contains("conflicting migrations"));
    }

    #[test]
    fn a_conflict_or_a_dirty_checkout_is_handled_without_losing_the_merge() {
        let w = World::new(vec![approve(), approve()]);
        w.repo.stages.borrow_mut().extend([Stage::Conflict, Stage::Dirty("/repos/shop".into())]);
        fn sec(title: &str) -> NewTask<'_> {
            NewTask { product: "shop", role: "appsec-engineer", title, kind: "security", branch: Some("jazari/task-1"), owner_role: Some("frontend-engineer"), spec: "S", ..Default::default() }
        }
        w.store.add_task(&sec("Security review: Login"), 0.0).unwrap();
        w.tick();
        let fix = w.next();
        assert!(fix.title == "Fix: Login" && fix.brief.contains("no longer merges cleanly"));
        w.store.set_status(fix.id, "done", None, None, 0.0).unwrap();
        w.store.add_task(&sec("Security review: Signup"), 0.0).unwrap();
        w.tick();
        assert!(w.last().contains("uncommitted changes"));
        assert_eq!(w.tick(), Tick::Idle);
        assert!(w.last().starts_with("🚀 Merged into main: Signup"), "{}", w.last());
        assert!(pending(&w.store).is_empty());
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
        w.repo.ahead.set(0);
        w.tick();
        assert_eq!(w.next().title, "Engine", "nothing to merge counts as landed");
    }

    #[test]
    fn follow_ups_use_role_keys_skip_repeats_and_judges_add_one() {
        let w = World::new(vec![
            done(json!({"follow_ups": [{"role": "Backend Engineer", "title": "Add CI"}, {"role": "qa", "title": "add ci!"}, {"role": "devops", "title": "Pin deps"}]})),
            done(json!({"verdict": "changes", "follow_ups": [{"role": "engineer", "title": "One"}, {"role": "engineer", "title": "Two"}]})),
        ]);
        w.repo.ahead.set(0);
        w.add("Build login");
        w.tick();
        let titles: Vec<_> = w.store.tasks(None, 20).unwrap().into_iter().filter(|t| t.status == "queued").map(|t| (t.title, t.role)).collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&("Add CI".into(), "backend-engineer".into())) && titles.contains(&("Pin deps".into(), "platform-engineer".into())));
        let t = NewTask { product: "shop", role: "evaluator", title: "Evaluate: X", kind: "eval", branch: Some("b"), owner_role: Some("engineer"), ..Default::default() };
        let judge = w.store.add_task(&NewTask { priority: 50, ..t }, 0.0).unwrap();
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
        assert!(text.starts_with(&format!("🔐 #{a} Ship v1 needs your approval to deploy")) && buttons == 2);
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
    fn a_refused_run_is_requeued_and_jazari_sleeps_until_the_reset() {
        let w = World::new(vec![vec![limit_line(1.0, "rejected")], done(json!({}))]);
        w.repo.ahead.set(0);
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

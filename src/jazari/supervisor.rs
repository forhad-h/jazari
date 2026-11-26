//! The supervisor: one task at a time, for as long as it runs.
//!
//! Each tick it checks the pause and the usage windows, takes the next queued task, and runs a
//! worker on it in the product's container, in the task's own git worktree. It then moves the work
//! along the pipeline:
//! - a branch with new commits goes to a code review;
//! - unless the change is documentation only, it then goes to a security review;
//! - after that it is merged into the default branch;
//! - a review that asks for changes sends a fix task back to the role that built the branch.
//!
//! Questions and approvals wait for the founder. When a usage window is used up, it sleeps until
//! the reset.

use super::git::Merge;
use super::limits::{self, Decision, Limits, Profile};
use super::store::{NewTask, Run, Store, Task};
use super::worker::{self, Kind, Outcome, Runner, Setup};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Failed attempts before a task is set aside for the founder.
pub const MAX_ATTEMPTS: i64 = 3;
/// Review rounds a branch gets before the founder is asked.
pub const MAX_ROUNDS: i64 = 3;
/// Queued tasks a product may have before build roles' follow-ups are dropped; planning roles
/// may still add theirs.
pub const MAX_QUEUED: i64 = 25;
/// How long to wait after a refusal that gave no reset time.
const BACKOFF_SECS: f64 = 900.0;

/// Git on a product's repo, so tests run without one.
pub trait Repo {
    fn default_branch(&self, repo: &Path) -> anyhow::Result<String>;
    fn prepare(&self, repo: &Path, task: i64, branch: &str, base: &str) -> anyhow::Result<PathBuf>;
    fn remove(&self, repo: &Path, worktree: &Path);
    fn commits_ahead(&self, repo: &Path, base: &str, branch: &str) -> usize;
    fn changed_files(&self, repo: &Path, base: &str, branch: &str) -> Vec<String>;
    fn merge(&self, repo: &Path, base: &str, branch: &str, message: &str) -> anyhow::Result<Merge>;
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
    fn merge(&self, repo: &Path, base: &str, branch: &str, message: &str) -> anyhow::Result<Merge> {
        super::git::merge(repo, base, branch, message)
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

/// A merge waiting for the founder's checkout to be clean.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct Pending {
    task: i64,
    product: String,
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
    let mut left = Vec::new();
    for p in list {
        let Some(product) = d.store.product(&p.product)? else { continue };
        let Some(task) = d.store.task(p.task)? else { continue };
        if !merge(d, &task, &product.repo, &p.branch, &p.title)? {
            left.push(p);
        }
    }
    set_pending(d.store, &left)
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
    let persona = super::personas::find(&task.role);
    let system_prompt = persona.system_prompt();
    let setup = Setup {
        container: &container,
        workdir: &worktree.to_string_lossy(),
        base: &base,
        model: persona.model(&d.profile),
        max_turns: d.profile.max_turns,
        system_prompt: &system_prompt,
        max_follow_ups: persona.max_follow_ups(),
    };
    let outcome = worker::run(d.runner, &task, &setup);
    d.repo.remove(&repo, &worktree);
    record(d, &task, &outcome, &product.repo, &base, &branch, now)?;
    Ok(Tick::Ran { task: task.id, kind: outcome.kind })
}

/// The feature a task belongs to, without the pipeline's prefixes.
fn feature(title: &str) -> &str {
    let mut t = title;
    for prefix in ["Review: ", "Security review: ", "Fix: ", "Resolve conflicts: "] {
        t = t.strip_prefix(prefix).unwrap_or(t);
    }
    t
}

#[allow(clippy::too_many_arguments)]
fn record(d: &Deps, task: &Task, o: &Outcome, repo: &str, base: &str, branch: &str, started: f64) -> anyhow::Result<()> {
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
    let owner = task.owner_role.clone().unwrap_or_else(|| task.role.clone());
    let next = |role: &str, title: String, brief: &str, kind: &str, round: i64, priority: i64| {
        s.add_task(
            &NewTask {
                product: &task.product,
                role,
                title: &title,
                brief,
                priority,
                parent: Some(task.id),
                kind,
                branch: Some(branch),
                owner_role: Some(&owner),
                round,
                after: None,
            },
            now,
        )
    };
    match o.kind {
        Kind::Done => {
            s.set_status(task.id, "done", Some(&o.summary), None, now)?;
            // Follow-ups run in order, each after the one before has landed, the first after this
            // task's own work. Reviews add at most one; repeats of an existing task are dropped, and
            // a full queue takes only the planning roles' tasks.
            let mut after = task.id;
            let planner = super::personas::find(&task.role).tier == super::personas::Tier::Lead;
            let limit = if task.kind == "work" { o.follow_ups.len() } else { 1 };
            let mut queued = 0;
            for f in o.follow_ups.iter().take(limit) {
                if s.has_similar(&task.product, &f.title)? || (!planner && s.queued(&task.product)? >= MAX_QUEUED) {
                    continue;
                }
                let role = super::personas::find(&f.role);
                let role = if role.key == "engineer" && super::personas::key(&f.role) != "engineer" { f.role.trim().to_string() } else { role.key };
                let brief = format!("{}\n\nQueued by {} in task #{}: {}", f.brief, task.role, task.id, feature(&task.title));
                let new = NewTask { product: &task.product, role: &role, title: &f.title, brief: &brief, priority: task.priority - 1, parent: Some(task.id), after: Some(after), ..Default::default() };
                after = s.add_task(&new, now)?;
                queued += 1;
            }
            let more = if queued == 0 { String::new() } else { format!("\n+{queued} follow-up task(s) queued.") };
            match task.kind.as_str() {
                "review" | "security" => {
                    if o.verdict.as_deref() == Some("approve") {
                        let docs = super::git::docs_only(&d.repo.changed_files(Path::new(repo), base, branch));
                        if task.kind == "review" && !docs {
                            next("appsec-engineer", format!("Security review: {}", feature(&task.title)), "", "security", task.round, task.priority)?;
                        } else {
                            merge(d, task, repo, branch, feature(&task.title))?;
                        }
                    } else {
                        let findings = format!("The {} asked for changes on this branch:\n{}", if task.kind == "review" { "code review" } else { "security review" }, o.summary);
                        let fix = next(&owner, format!("Fix: {}", feature(&task.title)), &findings, "work", task.round + 1, task.priority + 1)?;
                        if task.round + 1 >= MAX_ROUNDS {
                            let q = format!("`{branch}` failed review {MAX_ROUNDS} times. Latest findings: {}", o.summary);
                            s.wait_for_founder(fix, Some(&q), None, now)?;
                            (d.notify)(&format!("❓ #{fix} {}\n{q}\nReply /answer {fix} <guidance> to try again.", feature(&task.title)), &[]);
                        }
                    }
                }
                _ if d.repo.commits_ahead(Path::new(repo), base, branch) > 0 => {
                    next("code-reviewer", format!("Review: {}", feature(&task.title)), "", "review", task.round, task.priority + 1)?;
                    (d.notify)(&format!("✅ {name}\n{}\nIn review.{more}", o.summary), &[]);
                }
                _ => {
                    s.land(task.id)?;
                    (d.notify)(&format!("✅ {name}\n{}{more}", o.summary), &[]);
                }
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
    (d.audit)("task.end", json!({ "task": task.id, "outcome": o.kind.key(), "verdict": o.verdict, "summary": o.summary, "branch": branch, "session": o.session }));
    Ok(())
}

/// Merges an approved branch. Returns false when it must wait for a clean checkout.
fn merge(d: &Deps, task: &Task, repo: &str, branch: &str, title: &str) -> anyhow::Result<bool> {
    let base = d.repo.default_branch(Path::new(repo))?;
    match d.repo.merge(Path::new(repo), &base, branch, &format!("Merge {branch}: {title}"))? {
        Merge::Merged(sha) => {
            d.store.land_branch(branch)?;
            (d.audit)("merge", json!({ "task": task.id, "branch": branch, "base": base, "commit": sha }));
            (d.notify)(&format!("🚀 Merged into {base}: {title}\n{branch} · {}", &sha[..sha.len().min(10)]), &[]);
            Ok(true)
        }
        Merge::Conflict => {
            let owner = task.owner_role.clone().unwrap_or_else(|| task.role.clone());
            let brief = format!("`{branch}` no longer merges cleanly into `{base}`. Merge `{base}` into the branch, resolve the conflicts keeping both sides' intent, and make the tests pass.");
            d.store.add_task(
                &NewTask {
                    product: &task.product,
                    role: &owner,
                    title: &format!("Resolve conflicts: {title}"),
                    brief: &brief,
                    priority: task.priority + 1,
                    parent: Some(task.id),
                    kind: "work",
                    branch: Some(branch),
                    owner_role: Some(&owner),
                    round: task.round,
                    after: None,
                },
                (d.now)(),
            )?;
            Ok(true)
        }
        Merge::Dirty(dir) => {
            let mut list = pending(d.store);
            if !list.iter().any(|p| p.branch == branch) {
                list.push(Pending { task: task.id, product: task.product.clone(), branch: branch.into(), title: title.into() });
                set_pending(d.store, &list)?;
                (d.notify)(
                    &format!("⏸ {title} is approved, but {base} in {} has uncommitted changes. jazari merges it once they are committed or stashed.", dir.display()),
                    &[],
                );
            }
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jazari::limits::Plan;
    use crate::jazari::worker::tests::{limit_line, result_line};
    use std::cell::{Cell, RefCell};

    /// Plays back canned lines, one list per run, and keeps the prompts it was given.
    struct Script(RefCell<Vec<Vec<String>>>, RefCell<Vec<String>>);

    impl Runner for Script {
        fn run(&self, _c: &str, _w: &str, cmd: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<()> {
            self.1.borrow_mut().push(cmd.iter().position(|a| a == "-p").map(|i| cmd[i + 1].clone()).unwrap_or_default());
            let lines = {
                let mut s = self.0.borrow_mut();
                if s.is_empty() { Vec::new() } else { s.remove(0) }
            };
            lines.iter().for_each(|l| line(l));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRepo {
        ahead: Cell<usize>,
        files: RefCell<Vec<String>>,
        merges: RefCell<Vec<Merge>>,
        merged: RefCell<Vec<String>>,
    }

    impl Repo for FakeRepo {
        fn default_branch(&self, _r: &Path) -> anyhow::Result<String> {
            Ok("main".into())
        }
        fn prepare(&self, r: &Path, task: i64, _b: &str, _base: &str) -> anyhow::Result<PathBuf> {
            Ok(r.join(format!("wt-{task}")))
        }
        fn remove(&self, _r: &Path, _w: &Path) {}
        fn commits_ahead(&self, _r: &Path, _b: &str, _br: &str) -> usize {
            self.ahead.get()
        }
        fn changed_files(&self, _r: &Path, _b: &str, _br: &str) -> Vec<String> {
            self.files.borrow().clone()
        }
        fn merge(&self, _r: &Path, _b: &str, branch: &str, _m: &str) -> anyhow::Result<Merge> {
            self.merged.borrow_mut().push(branch.into());
            let mut m = self.merges.borrow_mut();
            Ok(if m.is_empty() { Merge::Merged("abc123def456".into()) } else { m.remove(0) })
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
            World {
                store,
                runner: Script(RefCell::new(runs), RefCell::default()),
                repo: FakeRepo::default(),
                now: Cell::new(1000.0),
                paused: Cell::new(false),
                sent: RefCell::default(),
                audit: RefCell::default(),
            }
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
            self.store.add_task(&NewTask { product: "shop", role: "backend-engineer", title, priority: 2, ..Default::default() }, 0.0).unwrap()
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

    #[test]
    fn a_built_branch_goes_through_review_and_security_to_a_merge() {
        let w = World::new(vec![
            done(json!({"follow_ups": [{"role": "qa-engineer", "title": "Test it"}]})),
            done(json!({"verdict": "approve"})),
            done(json!({"verdict": "approve"})),
        ]);
        w.repo.ahead.set(2);
        w.repo.files.borrow_mut().push("src/login.rs".into());
        assert_eq!(w.tick(), Tick::Idle);
        let id = w.add("Build login");
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Done });
        assert!(w.last().contains("In review") && w.last().contains("+1 follow-up"));
        let review = w.store.next_task().unwrap().unwrap();
        assert_eq!((review.kind.as_str(), review.role.as_str(), review.title.as_str()), ("review", "code-reviewer", "Review: Build login"));
        assert_eq!((review.branch.as_deref(), review.owner_role.as_deref(), review.priority), (Some("jazari/task-1"), Some("backend-engineer"), 3));
        w.tick();
        assert!(w.runner.1.borrow()[1].contains("reviewing the branch `jazari/task-1` against `main`"));
        let sec = w.store.next_task().unwrap().unwrap();
        assert_eq!((sec.kind.as_str(), sec.role.as_str()), ("security", "appsec-engineer"));
        w.tick();
        assert_eq!(*w.repo.merged.borrow(), vec!["jazari/task-1"]);
        assert!(w.last().starts_with("🚀 Merged into main: Build login"), "{}", w.last());
        assert!(w.audit.borrow().contains(&"merge".to_string()));
        // The follow-up runs once the feature has landed.
        let next = w.store.next_task().unwrap().unwrap();
        assert_eq!((next.title.as_str(), next.after_task), ("Test it", Some(id)));
    }

    #[test]
    fn a_plan_s_tasks_run_one_after_another_as_each_lands() {
        let plan = json!({"follow_ups": [{"role": "engineer", "title": "Skeleton"}, {"role": "engineer", "title": "Engine"}]});
        let w = World::new(vec![done(plan), done(json!({"verdict": "approve"})), done(json!({}))]);
        w.repo.ahead.set(1);
        w.repo.files.borrow_mut().push("company/VISION.md".into());
        w.store.add_task(&NewTask { product: "shop", role: "chief-of-staff", title: "Plan: notes", priority: 100, ..Default::default() }, 0.0).unwrap();
        w.tick();
        assert_eq!(w.store.next_task().unwrap().unwrap().title, "Review: Plan: notes", "the plan's tasks wait for it to merge");
        w.tick();
        assert_eq!(w.store.next_task().unwrap().unwrap().title, "Skeleton");
        w.repo.ahead.set(0);
        w.tick();
        assert_eq!(w.store.next_task().unwrap().unwrap().title, "Engine", "nothing to merge counts as landed");
    }

    #[test]
    fn follow_ups_use_role_keys_skip_repeats_and_reviews_add_one() {
        let w = World::new(vec![
            done(json!({"follow_ups": [{"role": "Backend Engineer", "title": "Add CI"}, {"role": "qa", "title": "add ci!"}, {"role": "devops", "title": "Pin deps"}]})),
            done(json!({"verdict": "approve", "follow_ups": [{"role": "engineer", "title": "One"}, {"role": "engineer", "title": "Two"}]})),
        ]);
        w.repo.ahead.set(0);
        w.add("Build login");
        w.tick();
        let titles: Vec<_> = w.store.tasks(None, 20).unwrap().into_iter().filter(|t| t.status == "queued").map(|t| (t.title, t.role)).collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&("Add CI".into(), "backend-engineer".into())) && titles.contains(&("Pin deps".into(), "devops".into())));
        w.store.add_task(&NewTask { product: "shop", role: "code-reviewer", title: "Review: X", kind: "review", branch: Some("b"), ..Default::default() }, 0.0).unwrap();
        w.store.set_status(2, "done", None, None, 0.0).unwrap();
        w.store.set_status(3, "done", None, None, 0.0).unwrap();
        w.tick();
        assert!(w.store.has_similar("shop", "One").unwrap() && !w.store.has_similar("shop", "Two").unwrap());
    }

    #[test]
    fn docs_skip_the_security_review_and_changes_go_back_to_the_builder() {
        let w = World::new(vec![
            done(json!({})),
            done(json!({"verdict": "changes", "summary": "Missing tests."})),
            done(json!({})),
            done(json!({"verdict": "approve"})),
        ]);
        w.repo.ahead.set(1);
        w.repo.files.borrow_mut().push("company/VISION.md".into());
        w.add("Write the vision");
        w.tick();
        w.tick();
        let fix = w.store.next_task().unwrap().unwrap();
        assert_eq!((fix.title.as_str(), fix.role.as_str(), fix.round, fix.kind.as_str()), ("Fix: Write the vision", "backend-engineer", 1, "work"));
        assert!(fix.brief.contains("code review asked for changes") && fix.brief.contains("Missing tests."));
        w.tick();
        assert_eq!(w.store.next_task().unwrap().unwrap().title, "Review: Write the vision");
        w.tick();
        assert_eq!(w.repo.merged.borrow().len(), 1, "docs merge after the code review alone");
    }

    #[test]
    fn three_failed_reviews_ask_the_founder() {
        let w = World::new(vec![done(json!({"verdict": "changes", "summary": "Still broken."}))]);
        let id = w.store.add_task(&NewTask { product: "shop", role: "code-reviewer", title: "Review: Login", kind: "review", branch: Some("jazari/task-1"), owner_role: Some("backend-engineer"), round: 2, ..Default::default() }, 0.0).unwrap();
        w.tick();
        let fix = w.store.tasks(None, 10).unwrap().into_iter().find(|t| t.parent == Some(id)).unwrap();
        assert_eq!((fix.status.as_str(), fix.round), ("blocked", 3));
        assert!(fix.question.unwrap().contains("failed review 3 times"));
        assert!(w.last().contains(&format!("/answer {}", fix.id)));
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
    fn a_dirty_checkout_holds_the_merge_until_it_is_clean() {
        let w = World::new(vec![done(json!({"verdict": "approve"}))]);
        w.repo.merges.borrow_mut().push(Merge::Dirty("/repos/shop".into()));
        w.store.add_task(&NewTask { product: "shop", role: "appsec-engineer", title: "Security review: Login", kind: "security", branch: Some("jazari/task-1"), ..Default::default() }, 0.0).unwrap();
        w.tick();
        assert!(w.last().contains("uncommitted changes"));
        assert_eq!(pending(&w.store).len(), 1);
        assert_eq!(w.tick(), Tick::Idle);
        assert_eq!(pending(&w.store).len(), 0);
        assert!(w.last().starts_with("🚀 Merged into main: Login"), "{}", w.last());
    }

    #[test]
    fn a_conflict_sends_the_branch_back_to_its_builder() {
        let w = World::new(vec![done(json!({"verdict": "approve"}))]);
        w.repo.merges.borrow_mut().push(Merge::Conflict);
        w.store.add_task(&NewTask { product: "shop", role: "appsec-engineer", title: "Security review: Login", kind: "security", branch: Some("jazari/task-1"), owner_role: Some("frontend-engineer"), ..Default::default() }, 0.0).unwrap();
        w.tick();
        let t = w.store.next_task().unwrap().unwrap();
        assert_eq!((t.title.as_str(), t.role.as_str(), t.branch.as_deref()), ("Resolve conflicts: Login", "frontend-engineer", Some("jazari/task-1")));
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

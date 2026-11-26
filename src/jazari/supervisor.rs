//! The supervisor: one task at a time, for as long as it runs. Each tick it checks the pause and
//! the usage windows, takes the next queued task, runs a worker on it in the product's container,
//! and records and reports what came of it. When a window is used up it sleeps until the reset.

use super::limits::{self, Decision, Limits, Profile};
use super::store::{Run, Store, Task};
use super::worker::{self, Kind, Outcome, Runner};
use serde_json::json;

/// Failed attempts before a task is set aside for the founder.
pub const MAX_ATTEMPTS: i64 = 3;
/// How long to wait after a refusal that gave no reset time.
const BACKOFF_SECS: f64 = 900.0;

/// The supervisor's surroundings, so tests run without Docker, Telegram or a clock.
pub struct Deps<'a> {
    pub store: &'a Store,
    pub runner: &'a dyn Runner,
    pub profile: Profile,
    pub now: &'a dyn Fn() -> f64,
    pub paused: &'a dyn Fn() -> bool,
    /// Starts the product's container and returns its name.
    pub ensure: &'a dyn Fn(&str, &str) -> anyhow::Result<String>,
    pub notify: &'a dyn Fn(&str),
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
        (d.notify)(&format!("😴 jazari sleeps until {}: {reason}.", clock(until)));
        (d.audit)("sleep", json!({ "until": until, "reason": reason }));
    }
}

pub fn tick(d: &Deps) -> anyhow::Result<Tick> {
    if (d.paused)() {
        return Ok(Tick::Paused);
    }
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
    d.store.set_status(task.id, "running", None, None, now)?;
    (d.audit)("task.start", json!({ "task": task.id, "product": task.product, "role": task.role, "title": task.title }));
    let outcome = match (d.ensure)(&product.name, &product.repo) {
        Ok(container) => worker::run(d.runner, &container, &task, d.profile.build_model, d.profile.max_turns),
        Err(e) => {
            // No container means nothing can run: hold the queue rather than burn attempts.
            d.store.set_status(task.id, "queued", None, None, now)?;
            return Err(e);
        }
    };
    record(d, &task, &outcome, now)?;
    Ok(Tick::Ran { task: task.id, kind: outcome.kind })
}

fn record(d: &Deps, task: &Task, o: &Outcome, started: f64) -> anyhow::Result<()> {
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
    match o.kind {
        Kind::Done => {
            s.set_status(task.id, "done", Some(&o.summary), o.branch.as_deref(), now)?;
            for (role, title) in &o.follow_ups {
                s.add_task(&task.product, role, title, &format!("Follow-up from task #{}: {}", task.id, task.title), task.priority - 1, Some(task.id), now)?;
            }
            let more = if o.follow_ups.is_empty() { String::new() } else { format!("\n+{} follow-up task(s) queued.", o.follow_ups.len()) };
            (d.notify)(&format!("✅ {name}\n{}{more}", o.summary));
        }
        Kind::Blocked => {
            s.set_status(task.id, "blocked", Some(&o.summary), o.branch.as_deref(), now)?;
            let ask = o.needs_founder.as_deref().map(|q| format!("\nNeeds you: {q}")).unwrap_or_default();
            (d.notify)(&format!("⛔ {name} is blocked.\n{}{ask}", o.summary));
        }
        Kind::Failed => {
            let attempts = s.bump_attempts(task.id)?;
            if attempts >= MAX_ATTEMPTS {
                s.set_status(task.id, "blocked", Some(&o.summary), o.branch.as_deref(), now)?;
                (d.notify)(&format!("⛔ {name} failed {attempts} times and is set aside.\nLast error: {}", o.summary));
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
    (d.audit)("task.end", json!({ "task": task.id, "outcome": o.kind.key(), "summary": o.summary, "branch": o.branch, "session": o.session }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jazari::limits::Plan;
    use crate::jazari::worker::tests::{limit_line, result_line};
    use std::cell::{Cell, RefCell};

    /// Plays back canned lines, one list per run.
    struct Script(RefCell<Vec<Vec<String>>>);

    impl Runner for Script {
        fn run(&self, _c: &str, _a: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<()> {
            let lines = { let mut s = self.0.borrow_mut(); if s.is_empty() { Vec::new() } else { s.remove(0) } };
            lines.iter().for_each(|l| line(l));
            Ok(())
        }
    }

    struct World {
        store: Store,
        runner: Script,
        now: Cell<f64>,
        paused: Cell<bool>,
        sent: RefCell<Vec<String>>,
        audit: RefCell<Vec<String>>,
    }

    impl World {
        fn new(runs: Vec<Vec<String>>) -> Self {
            let store = Store::memory();
            store.add_product("shop", "/repos/shop", 0.0).unwrap();
            World { store, runner: Script(RefCell::new(runs)), now: Cell::new(1000.0), paused: Cell::new(false), sent: RefCell::default(), audit: RefCell::default() }
        }

        fn tick(&self) -> Tick {
            let now = || self.now.get();
            let paused = || self.paused.get();
            let ensure = |p: &str, _r: &str| Ok(format!("jazari-{p}"));
            let notify = |t: &str| self.sent.borrow_mut().push(t.to_string());
            let audit = |k: &str, _v: serde_json::Value| self.audit.borrow_mut().push(k.to_string());
            let d = Deps { store: &self.store, runner: &self.runner, profile: Plan::Max5.profile(), now: &now, paused: &paused, ensure: &ensure, notify: &notify, audit: &audit };
            tick(&d).unwrap()
        }
    }

    fn done(summary: &str) -> Vec<String> {
        vec![limit_line(0.1, "allowed"), result_line(serde_json::json!({"status": "done", "summary": summary, "branch": "jazari/task-1", "follow_ups": [{"role": "qa", "title": "Test it"}]}))]
    }

    #[test]
    fn a_task_runs_to_done_and_queues_its_follow_ups() {
        let w = World::new(vec![done("Built it.")]);
        assert_eq!(w.tick(), Tick::Idle);
        let id = w.store.add_task("shop", "engineer", "Build login", "", 2, None, 0.0).unwrap();
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Done });
        let t = w.store.task(id).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.summary.as_str(), t.branch.as_deref()), ("done", "Built it.", Some("jazari/task-1")));
        let next = w.store.next_task().unwrap().unwrap();
        assert_eq!((next.title.as_str(), next.role.as_str(), next.priority), ("Test it", "qa", 1));
        assert!(w.sent.borrow()[0].starts_with("✅ #1 Build login") && w.sent.borrow()[0].contains("+1 follow-up"));
        assert_eq!(*w.audit.borrow(), vec!["task.start", "task.end"]);
        w.paused.set(true);
        assert_eq!(w.tick(), Tick::Paused);
    }

    #[test]
    fn a_failing_task_is_retried_then_set_aside() {
        let w = World::new(vec![vec![], vec![], vec![]]);
        let id = w.store.add_task("shop", "engineer", "Flaky", "", 0, None, 0.0).unwrap();
        for _ in 0..2 {
            assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Failed });
            assert_eq!(w.store.task(id).unwrap().unwrap().status, "queued");
        }
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Failed });
        assert_eq!(w.store.task(id).unwrap().unwrap().status, "blocked");
        assert_eq!(w.tick(), Tick::Idle);
        assert_eq!(w.sent.borrow().len(), 1);
        assert!(w.sent.borrow()[0].contains("failed 3 times"));
    }

    #[test]
    fn a_refused_run_is_requeued_and_jazari_sleeps_until_the_reset() {
        let w = World::new(vec![vec![limit_line(1.0, "rejected")], done("Built it.")]);
        let id = w.store.add_task("shop", "engineer", "Build login", "", 0, None, 0.0).unwrap();
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Limited });
        let t = w.store.task(id).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.attempts), ("queued", 0));
        let Tick::Sleeping { until } = w.tick() else { panic!("should sleep") };
        assert!(until > 9e9);
        assert!(matches!(w.tick(), Tick::Sleeping { .. }));
        assert_eq!(w.sent.borrow().len(), 1, "the sleep is announced once");
        assert!(w.sent.borrow()[0].contains("5-hour limit is used up"));
        w.now.set(until + 1.0);
        assert_eq!(w.tick(), Tick::Ran { task: id, kind: Kind::Done });
    }

    #[test]
    fn a_refusal_without_a_reset_time_backs_off() {
        let err = serde_json::json!({"type": "result", "subtype": "success", "is_error": true, "result": "Claude usage limit reached"}).to_string();
        let w = World::new(vec![vec![err]]);
        w.store.add_task("shop", "engineer", "Build login", "", 0, None, 0.0).unwrap();
        assert!(matches!(w.tick(), Tick::Ran { kind: Kind::Limited, .. }));
        assert_eq!(w.tick(), Tick::Sleeping { until: 1000.0 + BACKOFF_SECS });
        w.now.set(1000.0 + BACKOFF_SECS + 1.0);
        assert!(matches!(w.tick(), Tick::Ran { .. }));
    }
}

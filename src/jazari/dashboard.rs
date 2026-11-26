//! What the dashboard's Build view reads from jazari's store and writes to it. Every write goes
//! through the same functions as the CLI and the Telegram bridge, so their rules (the hard-line
//! override, which answers become founder signals) stay in one place.

use super::{limits, personas, store};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The key that holds the time of the supervisor's last heartbeat.
const ALIVE: &str = "supervisor:alive";
/// A heartbeat older than this means that the supervisor is not running.
const ALIVE_SECS: f64 = 120.0;
/// How often the supervisor writes its heartbeat.
const BEAT_SECS: u64 = 30;

/// What the footer says when a write was not sent.
pub const NOT_CONFIRMED: &str = "Not sent: the presence check failed or was cancelled.";
pub const NO_CHECK: &str = "Not sent: this needs Touch ID or the Mac password. Use `jazari build` in a terminal.";

#[derive(Clone, Debug, PartialEq)]
pub struct ProductRow {
    pub name: String,
    pub mode: String,
    pub running: usize,
    pub blocked: usize,
    pub queued: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskRow {
    pub id: i64,
    pub product: String,
    pub role: String,
    pub title: String,
    pub status: String,
    pub summary: String,
    pub brief: String,
    pub branch: Option<String>,
    pub question: Option<String>,
    pub approval: Option<String>,
}

impl TaskRow {
    fn of(t: store::Task) -> Self {
        TaskRow { id: t.id, product: t.product, role: t.role, title: t.title, status: t.status, summary: t.summary, brief: t.brief, branch: t.branch, question: t.question, approval: t.approval }
    }

    /// Whether the task waits for the founder: a question or an approval.
    pub fn waits(&self) -> bool {
        self.status == "blocked" && (self.question.is_some() || self.approval.is_some())
    }

    /// The labels of the options in the stored question (lines like `1. label: effect`), in order.
    pub fn options(&self) -> Vec<String> {
        let Some(q) = &self.question else { return Vec::new() };
        q.lines()
            .filter_map(|l| {
                let (n, rest) = l.split_once(". ")?;
                (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| rest.split(": ").next().unwrap_or(rest).trim().to_string())
            })
            .collect()
    }

    /// The recommended option, as the question's `Recommended:` line gives it.
    pub fn recommended(&self) -> Option<String> {
        self.question.as_deref()?.lines().find_map(|l| l.strip_prefix("Recommended: ")).map(str::to_string)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub products: Vec<ProductRow>,
    /// Running and blocked first, then queued, then finished, like `jazari build tasks`.
    pub tasks: Vec<TaskRow>,
    /// The last usage reading and whether jazari would start a task now.
    pub limits: String,
    pub supervisor: bool,
    /// False when jazari has no store yet (no product was ever made).
    pub exists: bool,
}

impl Snapshot {
    /// The tasks that wait for the founder, by task number (oldest first).
    pub fn needs_you(&self) -> Vec<&TaskRow> {
        let mut v: Vec<&TaskRow> = self.tasks.iter().filter(|t| t.waits()).collect();
        v.sort_by_key(|t| t.id);
        v
    }

    pub fn tasks_of(&self, product: &str) -> Vec<&TaskRow> {
        self.tasks.iter().filter(|t| t.product == product).collect()
    }
}

/// A write from the Build view.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A typed answer: a founder signal, unless it only picks an offered option.
    Answer { task: i64, text: String },
    /// An offered option picked with a digit key: never a signal, like a Telegram button tap.
    Pick { task: i64, label: String },
    Approve { task: i64 },
    Deny { task: i64 },
    Idea { product: String, text: String },
    Task { product: String, title: String },
    NewProduct { name: String, idea: String },
    Mode { product: String, mode: String },
}

impl Action {
    /// Every write that approves or speaks for the founder needs the presence check. Deny is safe
    /// without it: it only stops something.
    pub fn needs_presence(&self) -> bool {
        !matches!(self, Action::Deny { .. })
    }

    /// The reason in the system's Touch ID or password dialog, which reads "jazari is trying to
    /// <reason>".
    pub fn reason(&self) -> String {
        match self {
            Action::Answer { task, .. } | Action::Pick { task, .. } => format!("answer build task #{task}"),
            Action::Approve { task } => format!("approve the request of build task #{task}"),
            Action::Deny { task } => format!("deny the request of build task #{task}"),
            Action::Idea { product, .. } => format!("give {product} a new idea"),
            Action::Task { product, .. } => format!("queue a task for {product}"),
            Action::NewProduct { name, .. } => format!("start the product {name}"),
            Action::Mode { product, mode } => format!("set {product} to {mode} mode"),
        }
    }
}

/// The outcome of the presence check.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Presence {
    Confirmed,
    /// Failed or cancelled.
    Failed,
    /// This computer can't ask for it.
    Unavailable,
}

/// Whether this computer can run the presence check. Without it the Build view is read-only plus
/// deny.
pub fn presence_available() -> bool {
    crate::presence::available()
}

/// Asks for Touch ID or the Mac password. It blocks for up to 2 minutes, so only the worker thread
/// calls it.
pub fn presence(reason: &str) -> Presence {
    if !crate::presence::available() {
        Presence::Unavailable
    } else if crate::presence::confirm(reason) {
        Presence::Confirmed
    } else {
        Presence::Failed
    }
}

/// jazari's store, as the Build view uses it.
pub struct Db {
    store: store::Store,
    create: fn(&store::Store, &str, &str) -> anyhow::Result<(PathBuf, i64)>,
}

impl Db {
    fn path() -> PathBuf {
        super::dir().join("jazari.db")
    }

    /// Whether jazari has a store yet. The view only reads until then, so opening it creates
    /// nothing.
    pub fn exists() -> bool {
        Self::path().exists()
    }

    pub fn open() -> anyhow::Result<Self> {
        Ok(Db { store: super::open()?, create: super::create_product })
    }

    /// How many tasks wait for the founder, for the count in the List view.
    pub fn waiting(&self) -> anyhow::Result<usize> {
        Ok(self.store.tasks(None, 1000)?.into_iter().map(TaskRow::of).filter(TaskRow::waits).count())
    }

    pub fn snapshot(&self, now: f64) -> anyhow::Result<Snapshot> {
        snapshot_with(&self.store, &limits::plan().profile(), now)
    }

    /// Runs a write after the presence check it needs. The error text is shown word for word.
    pub fn run(&self, a: &Action, presence: &dyn Fn(&str) -> Presence) -> Result<String, String> {
        if a.needs_presence() {
            match presence(&a.reason()) {
                Presence::Confirmed => {}
                Presence::Failed => return Err(NOT_CONFIRMED.into()),
                Presence::Unavailable => return Err(NO_CHECK.into()),
            }
        }
        self.apply(a).map_err(|e| e.to_string())
    }

    fn apply(&self, a: &Action) -> anyhow::Result<String> {
        let s = &self.store;
        match a {
            Action::Answer { task, text } => {
                anyhow::ensure!(!text.trim().is_empty(), "type an answer");
                super::answer_task(s, *task, text.trim(), true)?;
                Ok(format!("Task #{task} is back in the queue with your answer."))
            }
            Action::Pick { task, label } => {
                super::answer_task(s, *task, label, false)?;
                Ok(format!("Task #{task} is back in the queue with \"{label}\"."))
            }
            Action::Approve { task } => super::decide(s, *task, true),
            Action::Deny { task } => super::decide(s, *task, false),
            Action::Idea { product, text } => {
                let id = super::queue_idea(s, product, text)?;
                Ok(format!("Queued task #{id}: the Chief of Staff turns the idea into a plan."))
            }
            Action::Task { product, title } => {
                anyhow::ensure!(!title.trim().is_empty(), "say what to do");
                anyhow::ensure!(s.product(product)?.is_some(), "no product called {product}");
                let role = personas::resolve("engineer");
                let id = s.add_task(&store::NewTask { product, role: &role, title: title.trim(), brief: "", ..Default::default() }, crate::now())?;
                Ok(format!("Queued task #{id} for {product}."))
            }
            Action::NewProduct { name, idea } => {
                let (repo, id) = (self.create)(s, name.trim(), idea)?;
                Ok(format!("Created {} in {}. Task #{id}: the Chief of Staff turns the idea into a plan.", name.trim(), repo.display()))
            }
            Action::Mode { product, mode } => super::set_mode(s, product, mode),
        }
    }
}

fn snapshot_with(s: &store::Store, profile: &limits::Profile, now: f64) -> anyhow::Result<Snapshot> {
    let tasks: Vec<TaskRow> = s.tasks(None, 1000)?.into_iter().map(TaskRow::of).collect();
    let count = |p: &str, status: &str| tasks.iter().filter(|t| t.product == p && t.status == status).count();
    let products = s
        .products()?
        .into_iter()
        .map(|p| ProductRow { running: count(&p.name, "running"), blocked: count(&p.name, "blocked"), queued: count(&p.name, "queued"), name: p.name, mode: p.mode })
        .collect();
    Ok(Snapshot { products, limits: limits_line(s, profile, now)?, supervisor: alive(s, now), tasks, exists: true })
}

/// The same reading as `jazari build limits`, on one line.
fn limits_line(s: &store::Store, profile: &limits::Profile, now: f64) -> anyhow::Result<String> {
    let l: limits::Limits = s.get("limits")?.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let show = |name: &str, w: Option<limits::Window>| match w {
        Some(w) => format!("{name} {:.0}% used{}", w.used_pct, if w.allowed { "" } else { " (refused)" }),
        None => format!("{name}: no reading yet"),
    };
    let go = match limits::decide(&l, profile, now) {
        limits::Decision::Go => "jazari would start a task now".to_string(),
        limits::Decision::Sleep { reason, .. } => format!("jazari would wait: {reason}"),
    };
    Ok(format!("{} · {} · {go}", show("5-hour", l.five_hour), show("7-day", l.seven_day)))
}

/// Whether the supervisor's heartbeat is recent.
fn alive(s: &store::Store, now: f64) -> bool {
    s.get(ALIVE).ok().flatten().and_then(|t| t.parse::<f64>().ok()).is_some_and(|t| t > 0.0 && now - t < ALIVE_SECS)
}

/// The supervisor's heartbeat: a thread that writes the time to the store every 30 seconds. One
/// supervisor tick can run a whole task, which takes much longer than [`ALIVE_SECS`], so a beat
/// once per tick would show "stopped" during a long task.
pub(super) struct Heartbeat {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    pub(super) fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            // rusqlite's Connection isn't Sync, so the thread has its own store.
            let Ok(store) = super::open() else { return };
            loop {
                let _ = store.put(ALIVE, &crate::now().to_string());
                for _ in 0..BEAT_SECS {
                    if flag.load(Ordering::Relaxed) {
                        let _ = store.put(ALIVE, "0");
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        });
        Heartbeat { stop, thread: Some(thread) }
    }

    /// Ends the heartbeat and marks the supervisor as stopped. A supervisor that ends without this
    /// (a crash, `process::exit` or `_exit` from the signal handler) leaves an old beat, and the
    /// view shows "stopped" after [`ALIVE_SECS`].
    pub(super) fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        fn fake_create(s: &store::Store, name: &str, idea: &str) -> anyhow::Result<(PathBuf, i64)> {
            s.add_product(name, &format!("/repos/{name}"), 0.0)?;
            let id = s.add_task(&store::NewTask { product: name, role: "chief-of-staff", title: idea, brief: idea, ..Default::default() }, 0.0)?;
            Ok((PathBuf::from(format!("/repos/{name}")), id))
        }
        let store = store::Store::memory();
        store.add_product("shop", "/repos/shop", 0.0).unwrap();
        Db { store, create: fake_create }
    }

    fn task(d: &Db, title: &str) -> i64 {
        d.store.add_task(&store::NewTask { product: "shop", role: "engineer", title, brief: "b", ..Default::default() }, 0.0).unwrap()
    }

    const QUESTION: &str = "Remove the login rate limit?\n1. Raise the limit to 10: keeps the protection\n2. Remove it: allows guessing\nRecommended: Raise the limit to 10";

    fn yes(_: &str) -> Presence {
        Presence::Confirmed
    }

    fn signals(d: &Db) -> usize {
        d.store.pending_signals(100).unwrap().len()
    }

    fn status(d: &Db, id: i64) -> String {
        d.store.task(id).unwrap().unwrap().status
    }

    #[test]
    fn needs_you_holds_only_waiting_tasks_oldest_first() {
        let d = db();
        let a = task(&d, "a");
        let b = task(&d, "b");
        let _c = task(&d, "c");
        d.store.wait_for_founder(b, Some("Which?"), None, 0.0).unwrap();
        d.store.wait_for_founder(a, None, Some("deploy: to production"), 0.0).unwrap();
        let s = snapshot_with(&d.store, &limits::Plan::Max5.profile(), 1000.0).unwrap();
        assert_eq!(s.needs_you().iter().map(|t| t.id).collect::<Vec<_>>(), vec![a, b]);
        assert_eq!(s.products, vec![ProductRow { name: "shop".into(), mode: "normal".into(), running: 0, blocked: 2, queued: 1 }]);
    }

    #[test]
    fn options_and_recommendation_are_read_from_the_question() {
        let d = db();
        let id = task(&d, "t");
        d.store.wait_for_founder(id, Some(QUESTION), None, 0.0).unwrap();
        let t = TaskRow::of(d.store.task(id).unwrap().unwrap());
        assert_eq!(t.options(), vec!["Raise the limit to 10", "Remove it"]);
        assert_eq!(t.recommended().as_deref(), Some("Raise the limit to 10"));
    }

    #[test]
    fn a_typed_answer_is_a_signal_and_a_picked_option_is_not() {
        let d = db();
        let a = task(&d, "a");
        let b = task(&d, "b");
        d.store.wait_for_founder(a, Some(QUESTION), None, 0.0).unwrap();
        d.store.wait_for_founder(b, Some(QUESTION), None, 0.0).unwrap();
        d.run(&Action::Pick { task: a, label: "Remove it".into() }, &yes).unwrap();
        assert_eq!((status(&d, a), signals(&d)), ("queued".into(), 0));
        d.run(&Action::Answer { task: b, text: "Use a captcha after 5 tries".into() }, &yes).unwrap();
        assert_eq!((status(&d, b), signals(&d)), ("queued".into(), 1));
    }

    #[test]
    fn a_hard_line_answer_without_an_override_is_refused_word_for_word() {
        let d = db();
        let id = task(&d, "t");
        d.store.wait_for_founder_on(id, Some(QUESTION), None, true, 0.0).unwrap();
        let e = d.run(&Action::Pick { task: id, label: "Remove it".into() }, &yes).unwrap_err();
        assert!(e.contains("crosses a hard line"), "{e}");
        assert_eq!(status(&d, id), "blocked");
    }

    #[test]
    fn a_failed_or_missing_presence_check_writes_nothing() {
        let d = db();
        let id = task(&d, "t");
        d.store.wait_for_founder(id, None, Some("deploy: to production"), 0.0).unwrap();
        let writes = [
            Action::Approve { task: id },
            Action::Answer { task: id, text: "go".into() },
            Action::Pick { task: id, label: "x".into() },
            Action::Idea { product: "shop".into(), text: "an idea".into() },
            Action::Task { product: "shop".into(), title: "a task".into() },
            Action::NewProduct { name: "cafe".into(), idea: "an idea".into() },
            Action::Mode { product: "shop".into(), mode: "autonomous".into() },
        ];
        for (check, said) in [(Presence::Failed, NOT_CONFIRMED), (Presence::Unavailable, NO_CHECK)] {
            for a in &writes {
                assert_eq!(d.run(a, &|_| check), Err(said.to_string()), "{a:?}");
            }
        }
        assert_eq!(status(&d, id), "blocked");
        assert_eq!(d.store.tasks(None, 100).unwrap().len(), 1);
        assert_eq!(d.store.products().unwrap().len(), 1);
        assert_eq!(d.store.product("shop").unwrap().unwrap().mode, "normal");
        assert_eq!(signals(&d), 0);
    }

    #[test]
    fn deny_needs_no_presence_check() {
        let d = db();
        let id = task(&d, "t");
        d.store.wait_for_founder(id, None, Some("spend: $20 a month"), 0.0).unwrap();
        let said = d.run(&Action::Deny { task: id }, &|_| Presence::Unavailable).unwrap();
        assert!(said.starts_with("Denied"), "{said}");
        assert_eq!(status(&d, id), "queued");
    }

    #[test]
    fn ideas_tasks_products_and_modes_use_the_cli_paths() {
        let d = db();
        d.run(&Action::Idea { product: "shop".into(), text: "gift cards".into() }, &yes).unwrap();
        d.run(&Action::Task { product: "shop".into(), title: "fix the cart".into() }, &yes).unwrap();
        d.run(&Action::NewProduct { name: "cafe".into(), idea: "a menu app".into() }, &yes).unwrap();
        d.run(&Action::Mode { product: "shop".into(), mode: "autonomous".into() }, &yes).unwrap();
        let roles: Vec<String> = d.store.tasks(Some("shop"), 10).unwrap().into_iter().map(|t| t.role).collect();
        assert!(roles.contains(&"chief-of-staff".to_string()) && roles.contains(&personas::resolve("engineer")), "{roles:?}");
        assert!(d.store.product("cafe").unwrap().is_some());
        assert_eq!(d.store.product("shop").unwrap().unwrap().mode, "autonomous");
        assert!(d.run(&Action::Task { product: "nope".into(), title: "x".into() }, &yes).is_err());
    }

    #[test]
    fn the_heartbeat_counts_for_two_minutes() {
        let d = db();
        assert!(!alive(&d.store, 1000.0));
        d.store.put(ALIVE, "1000").unwrap();
        assert!(alive(&d.store, 1000.0 + ALIVE_SECS - 1.0));
        assert!(!alive(&d.store, 1000.0 + ALIVE_SECS));
        d.store.put(ALIVE, "0").unwrap();
        assert!(!alive(&d.store, 1.0));
    }
}

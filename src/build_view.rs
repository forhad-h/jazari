//! The dashboard's Build view (`B`): jazari's products and tasks, the questions and approvals that
//! wait for the founder, and an input box for answers, ideas, tasks and new products. Reads and
//! writes run on a worker thread that reports through a channel read in `tick()`.

use crate::jazari::dashboard::{Action, Db, NO_CHECK, Presence, Snapshot, TaskRow};
use crate::prompt_view::TextBox;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

/// How often the worker reads the store when nothing happens.
const REFRESH: Duration = Duration::from_secs(2);

pub enum Msg {
    Snapshot(Result<Snapshot, String>),
    Done(Result<String, String>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pane {
    Needs,
    Products,
    Tasks,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    Answer { task: i64 },
    Idea { product: String },
    Task { product: String },
    ProductName,
    ProductIdea { name: String },
}

impl InputKind {
    /// What Enter does, shown as the box's title.
    pub fn label(&self) -> String {
        match self {
            InputKind::Answer { task } => format!("Your answer to #{task} (start with \"override:\" to pick against a hard line)"),
            InputKind::Idea { product } => format!("A new idea for {product}: the Chief of Staff plans it"),
            InputKind::Task { product } => format!("A task for {product}'s engineer, in one line"),
            InputKind::ProductName => "The new product's name: lowercase letters, digits and dashes".into(),
            InputKind::ProductIdea { name } => format!("The idea for {name}, in your own words"),
        }
    }
}

pub struct Input {
    pub kind: InputKind,
    pub text: TextBox,
}

/// A write that waits for a second press of the same key.
pub struct Pending {
    pub key: char,
    pub action: Action,
    pub prompt: String,
}

pub struct BuildView {
    pub snap: Snapshot,
    /// Why the store couldn't be read, if it couldn't.
    pub error: Option<String>,
    pub loaded: bool,
    pub focus: Pane,
    pub sel_needs: usize,
    pub sel_product: usize,
    pub sel_task: usize,
    pub details: bool,
    pub input: Option<Input>,
    pub pending: Option<Pending>,
    /// A write was sent and its result hasn't come back yet.
    pub busy: Option<String>,
    /// The last write's result, for the status line.
    pub note: Option<Result<String, String>>,
    /// Whether this computer can ask for Touch ID or the Mac password. Without it the view only
    /// reads and denies.
    pub checks: bool,
    tx: Sender<Action>,
    rx: Receiver<Msg>,
}

impl BuildView {
    pub fn open() -> Self {
        let (atx, arx) = channel();
        let (mtx, mrx) = channel();
        std::thread::spawn(move || worker(arx, mtx));
        Self::with(atx, mrx, crate::jazari::dashboard::presence_available())
    }

    fn with(tx: Sender<Action>, rx: Receiver<Msg>, checks: bool) -> Self {
        BuildView {
            snap: Snapshot::default(),
            error: None,
            loaded: false,
            focus: Pane::Needs,
            sel_needs: 0,
            sel_product: 0,
            sel_task: 0,
            details: false,
            input: None,
            pending: None,
            busy: None,
            note: None,
            checks,
            tx,
            rx,
        }
    }

    pub fn tick(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Snapshot(Ok(s)) => {
                    if !self.loaded && s.needs_you().is_empty() {
                        self.focus = Pane::Products;
                    }
                    self.snap = s;
                    self.error = None;
                    self.loaded = true;
                    self.clamp();
                }
                Msg::Snapshot(Err(e)) => {
                    self.error = Some(e);
                    self.loaded = true;
                }
                Msg::Done(result) => {
                    self.busy = None;
                    if result.is_ok() {
                        self.input = None;
                    }
                    self.note = Some(result);
                }
            }
        }
    }

    fn clamp(&mut self) {
        let last = |n: usize| n.saturating_sub(1);
        self.sel_needs = self.sel_needs.min(last(self.snap.needs_you().len()));
        self.sel_product = self.sel_product.min(last(self.snap.products.len()));
        self.sel_task = self.sel_task.min(last(self.product_tasks().len()));
    }

    pub fn product(&self) -> Option<&crate::jazari::dashboard::ProductRow> {
        self.snap.products.get(self.sel_product)
    }

    pub fn product_tasks(&self) -> Vec<&TaskRow> {
        self.product().map(|p| self.snap.tasks_of(&p.name)).unwrap_or_default()
    }

    /// The task the keys act on: the selected one in Needs you or in Tasks.
    pub fn task(&self) -> Option<&TaskRow> {
        match self.focus {
            Pane::Needs => self.snap.needs_you().get(self.sel_needs).copied(),
            Pane::Tasks => self.product_tasks().get(self.sel_task).copied(),
            Pane::Products => None,
        }
    }

    /// The product the keys act on: the selected task's in Needs you, else the selected product.
    fn product_name(&self) -> Option<String> {
        match self.focus {
            Pane::Needs => self.task().map(|t| t.product.clone()),
            _ => self.product().map(|p| p.name.clone()),
        }
    }

    fn send(&mut self, a: Action) {
        if a.needs_presence() && !self.checks {
            self.note = Some(Err(NO_CHECK.into()));
            return;
        }
        self.busy = Some(match &a {
            Action::NewProduct { name, .. } => format!("Creating {}…", name.trim()),
            _ if a.needs_presence() => "Waiting for Touch ID or the Mac password…".into(),
            _ => "Sending…".into(),
        });
        self.note = None;
        let _ = self.tx.send(a);
    }

    /// Opens an input box, unless this computer can't run the presence check that sending needs.
    fn ask(&mut self, kind: InputKind) {
        if !self.checks {
            self.note = Some(Err(NO_CHECK.into()));
            return;
        }
        self.input = Some(Input { kind, text: TextBox::default() });
        self.note = None;
    }

    fn confirm(&mut self, key: char, action: Action, prompt: String) {
        if action.needs_presence() && !self.checks {
            self.note = Some(Err(NO_CHECK.into()));
            return;
        }
        self.pending = Some(Pending { key, action, prompt });
    }

    fn move_by(&mut self, d: isize) {
        let len = match self.focus {
            Pane::Needs => self.snap.needs_you().len(),
            Pane::Products => self.snap.products.len(),
            Pane::Tasks => self.product_tasks().len(),
        };
        let sel = match self.focus {
            Pane::Needs => &mut self.sel_needs,
            Pane::Products => &mut self.sel_product,
            Pane::Tasks => &mut self.sel_task,
        };
        *sel = sel.saturating_add_signed(d).min(len.saturating_sub(1));
        if self.focus == Pane::Products {
            self.sel_task = 0;
        }
    }

    /// Handles a key; false means the view should close.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if self.input.is_some() {
            self.on_input_key(key);
            return true;
        }
        let KeyEvent { code, .. } = key;
        if let Some(p) = self.pending.take() {
            if code == KeyCode::Char(p.key) && self.busy.is_none() {
                self.send(p.action);
            }
            return true;
        }
        match code {
            KeyCode::Esc if self.details => self.details = false,
            KeyCode::Esc | KeyCode::Char('B') => return false,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Pane::Needs => Pane::Products,
                    Pane::Products => Pane::Tasks,
                    Pane::Tasks => Pane::Needs,
                };
                self.details = false;
            }
            KeyCode::Enter => self.details = self.task().is_some() && !self.details,
            _ if self.busy.is_some() => {}
            KeyCode::Char('a') => {
                if let Some(t) = self.task().filter(|t| t.waits() && t.question.is_some()) {
                    let task = t.id;
                    self.ask(InputKind::Answer { task });
                }
            }
            KeyCode::Char(c @ '1'..='4') => {
                if let Some(t) = self.task().filter(|t| t.waits()) {
                    let n = c as usize - '1' as usize;
                    if let Some(label) = t.options().get(n) {
                        let prompt = format!("Press {c} again to answer #{} with \"{label}\"", t.id);
                        let action = Action::Pick { task: t.id, label: label.clone() };
                        self.confirm(c, action, prompt);
                    }
                }
            }
            KeyCode::Char(c @ ('y' | 'd')) => {
                if let Some(t) = self.task().filter(|t| t.waits())
                    && let Some(what) = t.approval.clone()
                {
                    let (verb, action) = if c == 'y' { ("approve", Action::Approve { task: t.id }) } else { ("deny", Action::Deny { task: t.id }) };
                    let prompt = format!("Press {c} again to {verb} #{}: {what}", t.id);
                    self.confirm(c, action, prompt);
                }
            }
            KeyCode::Char('i') => {
                if let Some(product) = self.product_name() {
                    self.ask(InputKind::Idea { product });
                }
            }
            KeyCode::Char('t') => {
                if let Some(product) = self.product_name() {
                    self.ask(InputKind::Task { product });
                }
            }
            KeyCode::Char('N') => self.ask(InputKind::ProductName),
            KeyCode::Char('m') => {
                if let Some(p) = (self.focus != Pane::Needs).then(|| self.product()).flatten() {
                    let mode = if p.mode == "autonomous" { "normal" } else { "autonomous" };
                    let prompt = format!("Press m again to set {} to {mode} mode", p.name);
                    let action = Action::Mode { product: p.name.clone(), mode: mode.into() };
                    self.confirm('m', action, prompt);
                }
            }
            _ => {}
        }
        true
    }

    fn on_input_key(&mut self, key: KeyEvent) {
        let Some(input) = self.input.as_mut() else { return };
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter if self.busy.is_none() => {
                let text = input.text.text();
                if text.trim().is_empty() {
                    return;
                }
                let action = match input.kind.clone() {
                    InputKind::Answer { task } => Action::Answer { task, text },
                    InputKind::Idea { product } => Action::Idea { product, text },
                    InputKind::Task { product } => Action::Task { product, title: text },
                    InputKind::ProductName => {
                        input.kind = InputKind::ProductIdea { name: text.trim().to_string() };
                        input.text = TextBox::default();
                        return;
                    }
                    InputKind::ProductIdea { name } => Action::NewProduct { name, idea: text },
                };
                self.send(action);
            }
            _ if self.busy.is_none() => {
                input.text.on_key(key);
            }
            _ => {}
        }
    }

    /// The line over the footer: a second press to confirm, a write in progress, or the last
    /// result. The bool is true for an error.
    pub fn status(&self) -> Option<(String, bool)> {
        if let Some(p) = &self.pending {
            return Some((format!("{} · any other key cancels", p.prompt), false));
        }
        if let Some(b) = &self.busy {
            return Some((b.clone(), false));
        }
        match &self.note {
            Some(Ok(t)) => Some((t.clone(), false)),
            Some(Err(e)) => Some((e.clone(), true)),
            None => None,
        }
    }

    pub fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.input.is_some() {
            return vec![("enter", "send"), ("esc", "cancel")];
        }
        if self.pending.is_some() {
            return vec![("same key", "confirm"), ("any key", "cancel")];
        }
        let mut keys = vec![("↑↓", "select"), ("tab", "pane")];
        if let Some(t) = self.task() {
            keys.push(("enter", "details"));
            if self.checks && t.waits() && t.question.is_some() {
                keys.extend([("a", "answer"), ("1-4", "pick")]);
            }
            if t.waits() && t.approval.is_some() {
                if self.checks {
                    keys.push(("y", "approve"));
                }
                keys.push(("d", "deny"));
            }
        }
        if self.checks && self.product_name().is_some() {
            keys.extend([("i", "idea"), ("t", "task")]);
        }
        if self.checks && self.focus != Pane::Needs && self.product().is_some() {
            keys.push(("m", "mode"));
        }
        if self.checks {
            keys.push(("N", "new product"));
        }
        keys.push(("esc", "back"));
        keys
    }

    #[cfg(test)]
    fn for_tests(snap: Snapshot, checks: bool) -> (Self, Receiver<Action>, Sender<Msg>) {
        let (atx, arx) = channel();
        let (mtx, mrx) = channel();
        let mut v = Self::with(atx, mrx, checks);
        mtx.send(Msg::Snapshot(Ok(snap))).unwrap();
        v.tick();
        (v, arx, mtx)
    }
}

/// Counts the build questions and approvals that wait for the founder, for the List view. It
/// reads the store every [`COUNT_EVERY`] on its own thread, and only when the store exists.
pub struct Watch {
    rx: Receiver<usize>,
    pub waiting: usize,
}

const COUNT_EVERY: Duration = Duration::from_secs(10);

impl Watch {
    pub fn start() -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut db: Option<Db> = None;
            loop {
                if db.is_none() && Db::exists() {
                    db = Db::open().ok();
                }
                let n = db.as_ref().and_then(|d| d.waiting().ok()).unwrap_or(0);
                if tx.send(n).is_err() {
                    return;
                }
                std::thread::sleep(COUNT_EVERY);
            }
        });
        Watch { rx, waiting: 0 }
    }

    pub fn poll(&mut self) {
        if let Some(n) = self.rx.try_iter().last() {
            self.waiting = n;
        }
    }
}

impl BuildView {
    /// Whether the view holds a box or a second press that a stray key or wheel scroll shouldn't
    /// type into or confirm.
    pub fn asking(&self) -> bool {
        self.input.is_some() || self.pending.is_some()
    }
}

/// Reads the store every [`REFRESH`] and after each write, and runs the writes. The store is
/// opened only when it exists or a write needs it, so looking at the view creates nothing.
fn worker(actions: Receiver<Action>, out: Sender<Msg>) {
    run_worker(actions, out, &Db::exists, &Db::open, &crate::jazari::dashboard::presence)
}

fn run_worker(actions: Receiver<Action>, out: Sender<Msg>, exists: &dyn Fn() -> bool, open: &dyn Fn() -> anyhow::Result<Db>, presence: &dyn Fn(&str) -> Presence) {
    let mut db: Option<Db> = None;
    loop {
        if db.is_none() && exists() {
            db = open().ok();
        }
        let snap = match &db {
            Some(d) => d.snapshot(crate::now()).map_err(|e| e.to_string()),
            None => Ok(Snapshot::default()),
        };
        if out.send(Msg::Snapshot(snap)).is_err() {
            return;
        }
        match actions.recv_timeout(REFRESH) {
            Ok(a) => {
                if db.is_none() {
                    match open() {
                        Ok(d) => db = Some(d),
                        Err(e) => {
                            let _ = out.send(Msg::Done(Err(e.to_string())));
                            continue;
                        }
                    }
                }
                let result = db.as_ref().map(|d| d.run(&a, presence)).unwrap_or_else(|| Err("jazari's store isn't open".into()));
                if out.send(Msg::Done(result)).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jazari::dashboard::ProductRow;
    use ratatui::crossterm::event::KeyModifiers;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn row(id: i64, status: &str, question: Option<&str>, approval: Option<&str>) -> TaskRow {
        TaskRow {
            id,
            product: "shop".into(),
            role: "engineer".into(),
            title: format!("task {id}"),
            status: status.into(),
            summary: String::new(),
            brief: String::new(),
            branch: None,
            question: question.map(str::to_string),
            approval: approval.map(str::to_string),
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            products: vec![ProductRow { name: "shop".into(), mode: "normal".into(), running: 0, blocked: 2, queued: 1 }],
            tasks: vec![
                row(7, "blocked", Some("Which provider?\n1. Twilio: reliable\n2. MessageBird: cheap\nRecommended: Twilio"), None),
                row(3, "blocked", None, Some("deploy: to production")),
                row(9, "queued", None, None),
            ],
            limits: "5-hour 10% used".into(),
            supervisor: true,
            exists: true,
        }
    }

    fn sent(rx: &Receiver<Action>) -> Vec<Action> {
        rx.try_iter().collect()
    }

    #[test]
    fn needs_you_comes_first_and_lists_the_oldest_task_first() {
        let (v, _, _) = BuildView::for_tests(snap(), true);
        assert_eq!(v.focus, Pane::Needs);
        assert_eq!(v.task().map(|t| t.id), Some(3));
    }

    #[test]
    fn an_approval_needs_a_second_press_of_the_same_key() {
        let (mut v, rx, _) = BuildView::for_tests(snap(), true);
        v.on_key(key('y'));
        assert!(sent(&rx).is_empty());
        v.on_key(key('d'));
        assert!(sent(&rx).is_empty(), "y then d does neither");
        v.on_key(key('y'));
        v.on_key(key('y'));
        assert_eq!(sent(&rx), vec![Action::Approve { task: 3 }]);
    }

    #[test]
    fn a_digit_picks_an_offered_option_after_a_second_press() {
        let (mut v, rx, _) = BuildView::for_tests(snap(), true);
        v.on_key(key('j'));
        v.on_key(key('2'));
        v.on_key(key('2'));
        assert_eq!(sent(&rx), vec![Action::Pick { task: 7, label: "MessageBird".into() }]);
        v.on_key(key('4'));
        assert!(v.pending.is_none(), "no fourth option");
    }

    #[test]
    fn a_typed_answer_stays_in_the_box_until_it_is_accepted() {
        let (mut v, rx, tx) = BuildView::for_tests(snap(), true);
        v.on_key(key('j'));
        v.on_key(key('a'));
        for c in "override: we have a captcha".chars() {
            v.on_key(key(c));
        }
        v.on_key(code(KeyCode::Enter));
        assert_eq!(sent(&rx), vec![Action::Answer { task: 7, text: "override: we have a captcha".into() }]);
        tx.send(Msg::Done(Err("this choice crosses a hard line".into()))).unwrap();
        v.tick();
        assert_eq!(v.input.as_ref().map(|i| i.text.text()).as_deref(), Some("override: we have a captcha"));
        assert_eq!(v.status(), Some(("this choice crosses a hard line".into(), true)));
        v.on_key(code(KeyCode::Enter));
        tx.send(Msg::Done(Ok("Task #7 is back in the queue with your answer.".into()))).unwrap();
        v.tick();
        assert!(v.input.is_none());
    }

    #[test]
    fn a_new_product_asks_for_the_name_then_the_idea() {
        let (mut v, rx, _) = BuildView::for_tests(snap(), true);
        v.on_key(key('N'));
        for c in "cafe".chars() {
            v.on_key(key(c));
        }
        v.on_key(code(KeyCode::Enter));
        for c in "a menu app".chars() {
            v.on_key(key(c));
        }
        v.on_key(code(KeyCode::Enter));
        assert_eq!(sent(&rx), vec![Action::NewProduct { name: "cafe".into(), idea: "a menu app".into() }]);
        assert_eq!(v.busy.as_deref(), Some("Creating cafe…"));
    }

    #[test]
    fn without_a_presence_check_only_deny_is_sent() {
        let (mut v, rx, _) = BuildView::for_tests(snap(), false);
        for k in ['y', 'y', 'i', 't', 'N'] {
            v.on_key(key(k));
            assert!(v.input.is_none(), "{k}");
        }
        assert!(sent(&rx).is_empty());
        assert_eq!(v.status(), Some((NO_CHECK.into(), true)));
        v.on_key(key('d'));
        v.on_key(key('d'));
        assert_eq!(sent(&rx), vec![Action::Deny { task: 3 }]);
    }

    #[test]
    fn ideas_and_tasks_go_to_the_selected_product_and_mode_switches() {
        let (mut v, rx, tx) = BuildView::for_tests(snap(), true);
        v.on_key(code(KeyCode::Tab));
        assert_eq!(v.focus, Pane::Products);
        v.on_key(key('i'));
        for c in "gift cards".chars() {
            v.on_key(key(c));
        }
        v.on_key(code(KeyCode::Enter));
        tx.send(Msg::Done(Ok("ok".into()))).unwrap();
        v.tick();
        v.on_key(key('m'));
        v.on_key(key('m'));
        assert_eq!(
            sent(&rx),
            vec![Action::Idea { product: "shop".into(), text: "gift cards".into() }, Action::Mode { product: "shop".into(), mode: "autonomous".into() }]
        );
    }

    #[test]
    fn the_worker_reports_back_without_blocking_and_creates_nothing_until_a_write() {
        let (atx, arx) = channel();
        let (mtx, mrx) = channel();
        let opened = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = opened.clone();
        let t = std::thread::spawn(move || {
            run_worker(arx, mtx, &|| false, &|| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("no store in tests")
            }, &|_| Presence::Confirmed)
        });
        let mut v = BuildView::with(atx, mrx, true);
        while !v.loaded {
            v.tick();
            std::thread::yield_now();
        }
        assert_eq!(v.snap, Snapshot::default());
        assert!(!opened.load(std::sync::atomic::Ordering::SeqCst));
        v.send(Action::Deny { task: 1 });
        while v.busy.is_some() {
            v.tick();
            std::thread::yield_now();
        }
        assert_eq!(v.status(), Some(("no store in tests".into(), true)));
        assert!(opened.load(std::sync::atomic::Ordering::SeqCst));
        drop(v);
        t.join().unwrap();
    }

    #[test]
    fn b_opens_the_view_from_the_list_and_has_no_other_use() {
        let app = include_str!("app.rs");
        assert_eq!(app.matches("Char('B')").count(), 1);
        assert!(app.contains("(View::List, KeyCode::Char('B'))"));
        for other in [include_str!("clean_view.rs"), include_str!("usage.rs"), include_str!("answer_view.rs"), include_str!("prompt_view.rs"), include_str!("share_view.rs")] {
            assert!(!other.contains("Char('B')"));
        }
    }

    #[test]
    fn the_view_never_pushes_to_telegram() {
        let src = include_str!("build_view.rs");
        let code = src.split("#[cfg(test)]\nmod tests").next().unwrap();
        for push in ["tell_founder", "say(", "bot.send", "send_with_buttons", "notify"] {
            assert!(!code.contains(push), "{push}");
        }
    }
}


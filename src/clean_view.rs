//! The dashboard's cleanup view (`D`): the plan by tier with sizes, and removing items one at a time
//! after a confirmation. Scanning and removing run on a worker thread that reports through a
//! channel read in `tick()`.

use crate::clean::{Item, Outcome, Tier};
use ratatui::crossterm::event::KeyCode;
use std::sync::mpsc::{Receiver, channel};

enum Msg {
    Plan(Vec<Item>),
    Judged(Result<usize, String>),
    Ran { what: String, outcome: Outcome },
}

/// What waits for `y`.
#[derive(Clone, Debug, PartialEq)]
pub enum Confirm {
    One(usize),
    TierA,
}

pub struct CleanView {
    pub items: Vec<Item>,
    pub selected: usize,
    pub scanning: bool,
    /// Items being removed right now.
    pub running: bool,
    pub confirm: Option<Confirm>,
    /// What the last run did, newest last.
    pub log: Vec<String>,
    pub auto: bool,
    pub freed_30d: Option<u64>,
    /// Claude's saved verdicts on data-holding items, by id.
    pub verdicts: serde_json::Map<String, serde_json::Value>,
    /// Claude is being asked right now.
    pub judging: bool,
    rx: Option<Receiver<Msg>>,
    judge_rx: Option<Receiver<Msg>>,
}

impl CleanView {
    pub fn open() -> Self {
        let mut v = CleanView {
            items: Vec::new(),
            selected: 0,
            scanning: false,
            running: false,
            confirm: None,
            log: Vec::new(),
            auto: crate::clean::auto_enabled(),
            freed_30d: crate::clean::freed_last_30_days(crate::now()),
            verdicts: crate::clean_judge::load(),
            judging: false,
            rx: None,
            judge_rx: None,
        };
        v.scan();
        v
    }

    fn scan(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Plan(crate::clean::plan(&crate::clean::Env::real())));
        });
        self.rx = Some(rx);
        self.scanning = true;
    }

    /// Removes `items` one at a time, then scans again.
    fn start(&mut self, items: Vec<Item>) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let env = crate::clean::Env::real();
            let lock = crate::clean::CleanLock::take();
            for item in &items {
                let outcome = match lock {
                    Some(_) => crate::clean::run_and_record(item, &env, false),
                    None => crate::clean::Outcome::Kept("another cleanup is running".into()),
                };
                let _ = tx.send(Msg::Ran { what: format!("{} ({})", item.what, crate::clean::fmt_bytes(item.bytes)), outcome });
            }
            let _ = tx.send(Msg::Plan(crate::clean::plan(&env)));
        });
        self.rx = Some(rx);
        self.running = true;
        self.log.clear();
    }

    /// Asks Claude about the data-holding items on a worker thread.
    fn judge(&mut self) {
        let ask: Vec<Item> = crate::clean_judge::candidates(&self.items).into_iter().cloned().collect();
        if ask.is_empty() {
            self.log = vec!["no data-holding items to ask Claude about".into()];
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let refs: Vec<&Item> = ask.iter().collect();
            let _ = tx.send(Msg::Judged(crate::clean_judge::judge(&refs).map(|v| v.len())));
        });
        self.judge_rx = Some(rx);
        self.judging = true;
    }

    /// Claude's verdict on an item, if it gave one for the item's current size.
    pub fn verdict(&self, item: &Item) -> Option<crate::clean_judge::Verdict> {
        crate::clean_judge::lookup(&self.verdicts, item)
    }

    pub fn tick(&mut self) {
        if let Some(Ok(Msg::Judged(result))) = self.judge_rx.as_ref().map(|rx| rx.try_recv()) {
            self.judging = false;
            self.judge_rx = None;
            match result {
                Ok(n) => {
                    self.verdicts = crate::clean_judge::load();
                    self.log = vec![format!("Claude looked at {n} item(s); its advice is shown on each")];
                }
                Err(e) => self.log = vec![format!("couldn't ask Claude: {e}")],
            }
        }
        let Some(rx) = &self.rx else { return };
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Plan(items) => {
                    self.items = items;
                    self.selected = self.selected.min(self.items.len().saturating_sub(1));
                    self.scanning = false;
                    if self.running {
                        self.running = false;
                        self.freed_30d = crate::clean::freed_last_30_days(crate::now());
                    }
                }
                Msg::Judged(_) => {}
                Msg::Ran { what, outcome } => self.log.push(match outcome {
                    Outcome::Done { .. } => format!("removed {what}"),
                    Outcome::Kept(why) => format!("kept {what}: {why}"),
                    Outcome::Failed(e) => format!("failed {what}: {e}"),
                }),
            }
        }
    }

    #[cfg(test)]
    pub fn for_tests(items: Vec<Item>) -> Self {
        CleanView {
            items,
            selected: 0,
            scanning: false,
            running: false,
            confirm: None,
            log: Vec::new(),
            auto: true,
            freed_30d: None,
            verdicts: Default::default(),
            judging: false,
            rx: None,
            judge_rx: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.scanning || self.running
    }

    /// The question waiting for `y`, if any.
    pub fn question(&self) -> Option<String> {
        match self.confirm.as_ref()? {
            Confirm::One(i) => self.items.get(*i).map(|it| {
                let warn = if it.tier == Tier::C { " It holds data and can't be brought back." } else { "" };
                let claude = self.verdict(it).map(|v| format!(" Claude says {}: {}.", v.call, v.reason)).unwrap_or_default();
                format!("Remove {} ({})?{warn}{claude}", it.what, crate::clean::fmt_bytes(it.bytes))
            }),
            Confirm::TierA => {
                let total: u64 = self.items.iter().filter(|i| i.tier == Tier::A).map(|i| i.bytes).sum();
                Some(format!("Remove everything in tier A ({})?", crate::clean::fmt_bytes(total)))
            }
        }
    }

    /// Handles a key; false means the view should close.
    pub fn on_key(&mut self, code: KeyCode) -> bool {
        if let Some(confirm) = self.confirm.take() {
            if matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) && !self.busy() {
                let chosen: Vec<Item> = match confirm {
                    Confirm::One(i) => self.items.get(i).cloned().into_iter().collect(),
                    Confirm::TierA => self.items.iter().filter(|i| i.tier == Tier::A).cloned().collect(),
                };
                self.start(chosen);
            }
            return true;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('D') | KeyCode::Left | KeyCode::Char('h') => return false,
            KeyCode::Down | KeyCode::Char('j') => self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('r') if !self.busy() => self.scan(),
            KeyCode::Char('J') if !self.busy() && !self.judging => self.judge(),
            KeyCode::Char('o') => {
                self.auto = !self.auto;
                if crate::clean::set_auto(self.auto).is_err() {
                    self.auto = !self.auto;
                    self.log = vec!["couldn't save the auto-clean setting".into()];
                }
            }
            KeyCode::Enter | KeyCode::Char('x') if !self.busy() => match self.items.get(self.selected) {
                Some(item) if item.action == crate::clean::Action::None => self.log = vec![format!("{}: marsad only reports this ({})", item.what, item.note)],
                Some(_) => self.confirm = Some(Confirm::One(self.selected)),
                None => {}
            },
            KeyCode::Char('A') if !self.busy() && self.items.iter().any(|i| i.tier == Tier::A) => self.confirm = Some(Confirm::TierA),
            _ => {}
        }
        true
    }

    pub fn keys(&self) -> Vec<(&'static str, &'static str)> {
        if self.confirm.is_some() {
            return vec![("y", "remove"), ("any key", "keep")];
        }
        vec![
            ("↑↓", "select"),
            ("enter", "remove"),
            ("A", "all tier A"),
            ("J", "ask Claude"),
            ("o", if self.auto { "auto-clean: on" } else { "auto-clean: off" }),
            ("r", "rescan"),
            ("esc", "back"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::{Action, DirKind};

    fn item(id: &str, tier: Tier) -> Item {
        Item {
            id: id.into(),
            tier,
            what: id.into(),
            bytes: 1_000_000_000,
            action: Action::RemoveDir { path: "/nowhere/_cacache".into(), kind: DirKind::Cache { tools: &[] } },
            note: "n".into(),
        }
    }

    fn view(items: Vec<Item>) -> CleanView {
        CleanView::for_tests(items)
    }

    #[test]
    fn removing_needs_a_confirmation_and_report_only_items_never_ask() {
        let mut v = view(vec![item("a", Tier::A), item("b", Tier::B), item("c", Tier::C)]);
        v.on_key(KeyCode::Enter);
        assert_eq!(v.confirm, Some(Confirm::One(0)));
        assert_eq!(v.question().unwrap(), "Remove a (1.0 GB)?");
        v.on_key(KeyCode::Char('n'));
        assert_eq!((v.confirm.clone(), v.running), (None, false), "any other key keeps it");
        v.on_key(KeyCode::Down);
        v.on_key(KeyCode::Down);
        v.on_key(KeyCode::Enter);
        assert_eq!(v.confirm, Some(Confirm::One(2)), "a backup is removed one at a time, after a confirmation");
        assert!(v.question().unwrap().contains("can't be brought back"));
        v.on_key(KeyCode::Char('n'));
        v.items[2].action = Action::None;
        v.on_key(KeyCode::Enter);
        assert_eq!(v.confirm, None);
        assert!(v.log[0].contains("only reports"), "{:?}", v.log);
        v.on_key(KeyCode::Char('A'));
        assert_eq!(v.confirm, Some(Confirm::TierA));
        assert!(v.on_key(KeyCode::Esc), "esc answers the question first");
        assert!(!v.on_key(KeyCode::Esc));
    }

    #[test]
    fn keys_wait_while_busy() {
        let mut v = view(vec![item("a", Tier::A)]);
        v.scanning = true;
        v.on_key(KeyCode::Enter);
        v.on_key(KeyCode::Char('A'));
        assert_eq!(v.confirm, None);
    }
}

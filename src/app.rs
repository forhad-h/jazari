use crate::events;
use crate::jsonl::JsonlTail;
use crate::model::{EventKind, RateWindow, Session, SortKey, Status, Subagent, TimelineEntry};
use crate::transcript::{self, TranscriptReader};
use crate::badges::Badges;
use crate::share_view::{Outcome, ShareView};
use ratatui::crossterm::event::{self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::backend::Backend;
use ratatui::crossterm::execute;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Marks sessions whose Claude Code process is gone (crash, closed terminal) as ended.
pub fn mark_exited(sessions: &mut HashMap<String, Session>, sys: &mut System) {
    let now = crate::now();
    let mut pids: Vec<Pid> = sessions
        .values()
        .filter(|s| s.status != Status::Ended)
        .filter_map(|s| s.pid.map(Pid::from_u32))
        .collect();
    pids.sort();
    pids.dedup();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, ProcessRefreshKind::nothing());
    for s in sessions.values_mut().filter(|s| s.status != Status::Ended) {
        let gone = match s.pid {
            Some(pid) => sys.process(Pid::from_u32(pid)).is_none(),
            None => now - s.last_event > STALE_SECS,
        };
        if gone {
            s.set_status(Status::Ended, now);
            s.detail = "process exited".into();
            for a in s.subagents.values_mut() {
                a.running = false;
            }
        }
    }
}

/// Attaches the latest status line report (exact context window, rate limits) to each session.
pub fn load_snapshots(sessions: &mut HashMap<String, Session>) {
    let dir = crate::statusline::status_dir();
    for s in sessions.values_mut().filter(|s| s.status != Status::Ended || s.snapshot.is_none()) {
        if let Some(snap) = crate::statusline::load(&dir.join(format!("{}.json", s.id))) {
            s.snapshot = Some(snap);
        }
    }
}

/// Account-wide 5-hour and 7-day usage, from the most recent status line report.
pub fn latest_rate_limits(sessions: &HashMap<String, Session>) -> Option<(Option<RateWindow>, Option<RateWindow>)> {
    sessions
        .values()
        .filter_map(|s| s.snapshot.as_ref())
        .filter(|snap| snap.five_hour.is_some() || snap.seven_day.is_some())
        .max_by(|a, b| a.ts.total_cmp(&b.ts))
        .map(|snap| (snap.five_hour, snap.seven_day))
}

/// Ended sessions stay visible this long unless `a` is toggled.
const ENDED_GRACE_SECS: f64 = 10.0 * 60.0;
/// Sessions with no known process and no events for this long are treated as gone.
const STALE_SECS: f64 = 12.0 * 3600.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    List,
    History,
    /// One session's subagents: launching skill, tokens used, context left.
    Agents,
    Usage,
    /// The shared AGENT-BOARD.md of the selected session's repo.
    Board,
    /// Caches, build output and Docker data that can be removed (`D`).
    Clean,
}

/// Which events the history shows; `f` cycles through them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryFilter {
    #[default]
    All,
    Prompts,
    Replies,
    Skills,
    Agents,
    Questions,
}

impl HistoryFilter {
    pub fn next(self) -> Self {
        match self {
            HistoryFilter::All => HistoryFilter::Prompts,
            HistoryFilter::Prompts => HistoryFilter::Replies,
            HistoryFilter::Replies => HistoryFilter::Skills,
            HistoryFilter::Skills => HistoryFilter::Agents,
            HistoryFilter::Agents => HistoryFilter::Questions,
            HistoryFilter::Questions => HistoryFilter::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HistoryFilter::All => "all",
            HistoryFilter::Prompts => "prompts",
            HistoryFilter::Replies => "replies",
            HistoryFilter::Skills => "skills",
            HistoryFilter::Agents => "subagents",
            HistoryFilter::Questions => "questions",
        }
    }

    pub fn matches(self, kind: &EventKind) -> bool {
        match self {
            HistoryFilter::All => true,
            HistoryFilter::Prompts => matches!(kind, EventKind::Prompt(_)),
            HistoryFilter::Replies => matches!(kind, EventKind::Response(_)),
            HistoryFilter::Skills => matches!(kind, EventKind::Skill(_)),
            HistoryFilter::Agents => matches!(kind, EventKind::Agent { .. }),
            HistoryFilter::Questions => matches!(kind, EventKind::Question { .. }),
        }
    }
}

pub struct App {
    pub sessions: HashMap<String, Session>,
    pub selected: Option<String>,
    pub show_ended: bool,
    pub sort: SortKey,
    pub sort_reversed: bool,
    pub view: View,
    pub history_scroll: usize,
    pub history_filter: HistoryFilter,
    /// Lines scrolled in the selected history event's detail pane.
    pub detail_scroll: u16,
    pub compact_window: Option<u64>,
    pub tick: u64,
    pub hooks_installed: bool,
    pub usage: crate::usage::Usage,
    pub update: Option<crate::update::Notice>,
    /// Sounds and notifications for sessions that need the user or finish; `m` toggles.
    pub alerts_on: bool,
    /// A failure to show in the footer, such as settings that couldn't be saved; the next key clears it.
    pub error: Option<String>,
    /// A success to show in the footer; the next key or `INFO_FOR` clears it.
    pub info: Option<String>,
    /// The footer note on screen and when it first showed, so it can expire.
    note_shown: Option<(String, Instant)>,
    /// The launch command to copy by hand when the clipboard couldn't be reached; the next key clears it.
    pub copy_note: Option<CopyNote>,
    /// The `c` picker for which rules the copied command loads, drawn over the current view while open.
    pub rules_picker: Option<RulesPicker>,
    /// Writing or revising a prompt from the picker, drawn over it and given its keys while open.
    pub prompt_view: Option<crate::prompt_view::PromptView>,
    pub board: BoardState,
    /// The rank and achievements for the badge view, refreshed after each sync.
    pub badges: Option<Badges>,
    /// Sharing to LinkedIn, drawn over the current view and given its keys while open.
    pub share: Option<ShareView>,
    /// A terminal cell's height over its width in pixels, so the badge art can be drawn square.
    pub cell_aspect: f64,
    pub announcement: Option<Announcement>,
    alerts: crate::alerts::Detector,
    update_rx: std::sync::mpsc::Receiver<crate::update::Notice>,
    /// Bytes the daily cleanup scan found removable, reported once by its thread.
    clean_hint_rx: Option<std::sync::mpsc::Receiver<u64>>,
    log: JsonlTail,
    readers: HashMap<PathBuf, TranscriptReader>,
    sys: System,
    last_slow_refresh: Instant,
    pub counter: Counter,
    /// Disk, memory and CPU for the header meters, sampled once a second.
    pub system: Option<crate::system::Snapshot>,
    /// A message being typed to a session (`t`), drawn over the list while open.
    pub compose: Option<Compose>,
    /// A stop (`x`) or interrupt (`X`) of a session waiting for `y`.
    pub control: Option<Control>,
    /// The socket server that receives sessions' permission prompts and questions; started by
    /// [`run`] only, so tests never open a socket.
    pub answers: Option<crate::answer::server::Server>,
    /// A session's waiting prompt being answered (`r`), drawn over the list while open.
    pub answer_view: Option<crate::answer_view::AnswerView>,
    /// The cleanup view's state while it is open.
    pub clean: Option<crate::clean_view::CleanView>,
    sampler: crate::system::Sampler,
    /// The always-on-top window while it is open (`P`).
    pub pin: Option<crate::pin::Pin>,
}

/// A message being typed to one session.
pub struct Compose {
    pub session: String,
    pub name: String,
    pub text: String,
}

/// Stopping or interrupting one session, once confirmed.
pub struct Control {
    pub session: String,
    pub name: String,
    /// SIGINT now, rather than a stop before the next tool call.
    pub interrupt: bool,
}

/// Shown above the footer when `c` couldn't put the launch command on a clipboard it can confirm.
pub struct CopyNote {
    pub headline: &'static str,
    pub command: String,
    pub path: String,
    /// The `jazari build rules …` command that prints the same launch command.
    pub cli: String,
}

/// The `c` picker: first the prompts to tick, then whether they start a session or a team.
pub struct RulesPicker {
    pub library: Vec<crate::rules::Rule>,
    /// One per library entry.
    pub ticked: Vec<bool>,
    /// The highlighted prompt.
    pub cursor: usize,
    /// The highlighted choice once the prompts are chosen; None while ticking them.
    pub mode: Option<crate::rules::Choice>,
    /// The prompt waiting for `y` to be deleted.
    pub deleting: Option<usize>,
    /// The earlier versions of a prompt being browsed, opened with `v`.
    pub versions: Option<VersionList>,
    /// The prompt or skill deleted last, which `u` brings back.
    pub restorable: Option<(String, crate::rules::Kind)>,
}

/// The earlier versions of one of the user's prompts, newest first.
pub struct VersionList {
    pub key: String,
    pub kind: crate::rules::Kind,
    pub title: String,
    pub list: Vec<crate::rules::Version>,
    pub cursor: usize,
}

impl RulesPicker {
    pub fn chosen(&self) -> Vec<&crate::rules::Rule> {
        self.library.iter().zip(&self.ticked).filter(|(_, t)| **t).map(|(r, _)| r).collect()
    }

    fn toggle(&mut self, i: usize) {
        if let Some(t) = self.ticked.get_mut(i) {
            *t = !*t;
        }
    }

    /// Re-reads the library, keeping the ticks, and highlights `select` when given, ticking it
    /// when `tick`.
    fn reload(&mut self, library: Vec<crate::rules::Rule>, select: Option<&str>, tick: bool) {
        let keep: Vec<String> = self.chosen().iter().map(|r| r.key.clone()).collect();
        self.ticked = library.iter().map(|r| keep.contains(&r.key) || (tick && Some(r.key.as_str()) == select)).collect();
        self.cursor = select.and_then(|k| library.iter().position(|r| r.key == k)).unwrap_or(self.cursor).min(library.len().saturating_sub(1));
        self.library = library;
    }
}

/// How long a success note stays in the footer. Errors stay until a key, so one isn't missed.
const INFO_FOR: Duration = Duration::from_secs(4);

/// Clears the info once it has been on screen for `INFO_FOR`; while an error covers it, it waits.
/// `shown` remembers which text is on screen and since when; a new text restarts the clock.
fn expire_info(error: &Option<String>, info: &mut Option<String>, shown: &mut Option<(String, Instant)>, now: Instant) {
    let Some(text) = info.as_deref().filter(|_| error.is_none()) else {
        *shown = None;
        return;
    };
    match shown {
        Some((on_screen, since)) if on_screen == text => {
            if now.duration_since(*since) >= INFO_FOR {
                *info = None;
                *shown = None;
            }
        }
        _ => *shown = Some((text.to_string(), now)),
    }
}

/// The board view's state: which board, its last good parse, and when the file was read.
#[derive(Default)]
pub struct BoardState {
    /// The session whose repo the board belongs to.
    pub session: String,
    /// The cwd the path was resolved from, and when.
    cwd: String,
    resolved: Option<Instant>,
    pub path: Option<PathBuf>,
    /// Modified time and length of the file last parsed, so it is re-read only when it changes.
    key: Option<(std::time::SystemTime, u64)>,
    pub board: Option<crate::board::Board>,
    pub truncated: bool,
    /// No board file exists yet.
    pub missing: bool,
    /// The last read failed; `board` still holds the last good parse, if any.
    pub read_error: Option<String>,
    pub scroll: usize,
    /// The furthest the view can scroll, set while drawing.
    pub max_scroll: std::cell::Cell<usize>,
}

/// How long a resolved board path is reused before git is asked again (a repo may be created meanwhile).
const BOARD_RESOLVE: Duration = Duration::from_secs(30);

impl BoardState {
    /// Re-resolves the board path when the session moved or the cached one is old, then re-reads
    /// the file if it changed.
    fn refresh(&mut self, cwd: &str) -> Result<(), &'static str> {
        if self.path.is_none() || self.cwd != cwd || self.resolved.is_none_or(|t| t.elapsed() >= BOARD_RESOLVE) {
            self.cwd = cwd.to_string();
            self.resolved = Some(Instant::now());
            let path = match crate::board::resolve(cwd) {
                crate::board::Root::Board(path) => path,
                crate::board::Root::NoCwd => return Err("this session has no working directory yet"),
                crate::board::Root::Gone => return Err("this session's directory no longer exists"),
            };
            if self.path.as_ref() != Some(&path) {
                self.path = Some(path);
                self.key = None;
                self.board = None;
                self.scroll = 0;
            }
        }
        self.reload();
        Ok(())
    }

    fn reload(&mut self) {
        let Some(path) = &self.path else { return };
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (self.missing, self.board, self.key, self.read_error) = (true, None, None, None);
                return;
            }
            Err(e) => {
                self.read_error = Some(e.to_string());
                return;
            }
        };
        self.missing = false;
        let key = (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len());
        if self.key == Some(key) && self.read_error.is_none() {
            return;
        }
        // Agents edit the board in place, so a read can land mid-edit; keep the last good parse then.
        match crate::board::read(path) {
            Ok((text, truncated)) => {
                self.board = Some(crate::board::parse(&text));
                self.truncated = truncated;
                self.key = Some(key);
                self.read_error = None;
            }
            Err(e) => self.read_error = Some(e.to_string()),
        }
    }

    fn scroll(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta).min(self.max_scroll.get());
    }
}

/// How long a badge announcement stays in the footer once it has been drawn.
const ANNOUNCE_FOR: Duration = Duration::from_secs(10);

/// A footer note for badge levels that rose. It waits while the share view is open, stays
/// `ANNOUNCE_FOR` once drawn (or until Esc) rather than clearing on the next key, and the levels
/// count as announced only once it has been drawn.
pub struct Announcement {
    pub text: String,
    badges: Badges,
    drawn: Option<Instant>,
}

impl Announcement {
    fn new(text: String, badges: Badges) -> Self {
        Announcement { text, badges, drawn: None }
    }

    /// The views whose footer shows the note.
    pub fn shows_in(view: View) -> bool {
        view == View::List
    }

    /// After a frame: whether the note was on screen for the first time, so its levels are announced now.
    fn on_drawn(&mut self, visible: bool, now: Instant) -> bool {
        let first = visible && self.drawn.is_none();
        if first {
            self.drawn = Some(now);
        }
        first
    }

    fn expired(&self, now: Instant) -> bool {
        self.drawn.is_some_and(|t| now.duration_since(t) >= ANNOUNCE_FOR)
    }
}

/// The note the footer shows, by priority: an error, a success, a badge level-up, an update.
pub enum FooterNote<'a> {
    Error(&'a str),
    Info(&'a str),
    Badge(&'a str),
    Update(&'a crate::update::Notice),
}

/// What a key asks of the run loop.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    None,
    Quit,
    /// Suspend the terminal and edit this post text in the user's editor.
    EditPost(String),
    /// Suspend the terminal and edit this prompt text (an idea or a draft) in the user's editor.
    EditPrompt(String),
}

/// Indexed (session, agent) key -> tokens.
type DayTotals = HashMap<(String, String), i64>;

/// Today's and all-time tokens for the header: the indexed counter, topped up with live usage
/// the background sync hasn't indexed yet.
#[derive(Default)]
pub struct Counter {
    conn: Option<rusqlite::Connection>,
    loaded: Option<Instant>,
    /// Local day the indexed figures below are for.
    day: String,
    indexed: DayTotals,
    all_time: i64,
    /// Live readers' tokens per (session, agent), with the day they were counted for.
    live: HashMap<(String, String), (String, i64)>,
}

/// How often the header re-reads the indexed counter.
const COUNTER_RELOAD: Duration = Duration::from_secs(10);

impl Counter {
    fn reload_if_due(&mut self) {
        let today = crate::store::day_n_ago(1);
        if self.loaded.is_some_and(|t| t.elapsed() < COUNTER_RELOAD) && self.day == today {
            return;
        }
        self.loaded = Some(Instant::now());
        if self.conn.is_none() {
            self.conn = crate::store::open_readonly().ok();
        }
        let Some(conn) = &self.conn else { return };
        if let (Ok(rows), Ok(all)) = (crate::store::day_rows(conn, &today), crate::store::lifetime(conn)) {
            self.indexed = rows;
            self.all_time = all.total;
            self.day = today;
        }
    }

    fn set_live(&mut self, session: &str, agent: &str, reader: &TranscriptReader) {
        let today = crate::store::day_n_ago(1);
        let tokens = reader.daily().get(&today).map_or(0, |d| d.tokens.total() as i64);
        self.live.insert((session.to_string(), agent.to_string()), (today, tokens));
    }

    /// (today, all time).
    pub fn totals(&self) -> (i64, i64) {
        merge_live(&self.indexed, self.all_time, &self.day, &self.live)
    }
}

/// Adds live usage that is ahead of the index, per (session, agent), to both totals.
fn merge_live(indexed: &DayTotals, all_time: i64, day: &str, live: &HashMap<(String, String), (String, i64)>) -> (i64, i64) {
    let extra: i64 = live
        .iter()
        .filter(|(_, (d, _))| d == day)
        .map(|(key, (_, tokens))| (tokens - indexed.get(key).copied().unwrap_or(0)).max(0))
        .sum();
    (indexed.values().sum::<i64>() + extra, all_time + extra)
}

impl App {
    fn new() -> Self {
        let dir = crate::claude_data_dir();
        // Generated prompt files nobody has copied a command for in a while.
        let _ = crate::rules::prune();
        let mut sessions = HashMap::new();
        // Replay the rotated log first so long-running sessions keep their history.
        for ev in JsonlTail::new(dir.join("events.1.jsonl")).read_new() {
            events::apply(&mut sessions, &ev);
        }
        let hooks_installed = std::fs::read_to_string(crate::claude_dir().join("settings.json"))
            .is_ok_and(|s| s.contains("jazari") && s.contains(" hook"));
        Self {
            sessions,
            selected: None,
            show_ended: false,
            sort: SortKey::default(),
            sort_reversed: false,
            view: View::List,
            history_scroll: 0,
            history_filter: HistoryFilter::default(),
            detail_scroll: 0,
            compact_window: crate::model::auto_compact_window(),
            tick: 0,
            hooks_installed,
            usage: crate::usage::Usage::new(),
            update: None,
            alerts_on: crate::settings::alerts(),
            error: None,
            info: None,
            note_shown: None,
            copy_note: None,
            rules_picker: None,
            prompt_view: None,
            board: BoardState::default(),
            badges: None,
            share: None,
            cell_aspect: crate::ui::DEFAULT_CELL_ASPECT,
            announcement: None,
            alerts: crate::alerts::Detector::default(),
            update_rx: crate::update::background(),
            clean_hint_rx: crate::clean::daily_scan(),
            log: JsonlTail::new(dir.join("events.jsonl")),
            readers: HashMap::new(),
            sys: System::new(),
            last_slow_refresh: Instant::now() - Duration::from_secs(60),
            counter: Counter::default(),
            system: None,
            clean: None,
            compose: None,
            control: None,
            answers: None,
            answer_view: None,
            sampler: crate::system::Sampler::default(),
            pin: None,
        }
    }

    /// Sessions to show, in the chosen sort order (working sessions on top by default).
    pub fn visible(&self) -> Vec<&Session> {
        let now = crate::now();
        let mut list: Vec<&Session> = self
            .sessions
            .values()
            .filter(|s| self.show_ended || s.status != Status::Ended || now - s.status_since < ENDED_GRACE_SECS)
            .collect();
        crate::model::sort_sessions(&mut list, self.sort, self.sort_reversed, self.compact_window);
        list
    }

    pub fn selected_session(&self) -> Option<&Session> {
        let visible = self.visible();
        self.selected
            .as_ref()
            .and_then(|id| visible.iter().find(|s| &s.id == id).copied())
            .or_else(|| visible.first().copied())
    }

    pub fn selected_index(&self) -> Option<usize> {
        let sel = self.selected_session()?;
        self.visible().iter().position(|s| s.id == sel.id)
    }

    /// Account-wide 5-hour and 7-day usage, from the most recent status line report.
    pub fn rate_limits(&self) -> Option<(Option<RateWindow>, Option<RateWindow>)> {
        latest_rate_limits(&self.sessions)
    }

    fn move_selection(&mut self, delta: isize) {
        let ids: Vec<String> = self.visible().iter().map(|s| s.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let cur = self.selected_index().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, ids.len() as isize - 1) as usize;
        self.selected = Some(ids[next].clone());
    }

    /// Opens a full-screen view of the highlighted session, scrolled to the top.
    fn open(&mut self, view: View) {
        if let Some(id) = self.selected_session().map(|s| s.id.clone()) {
            self.selected = Some(id);
            self.view = view;
            self.history_scroll = 0;
            self.detail_scroll = 0;
        }
    }

    /// Opens the board of the selected session's repo, or says why there is none.
    fn open_board(&mut self) {
        let Some((id, cwd)) = self.selected_session().map(|s| (s.id.clone(), s.cwd.clone())) else { return };
        self.selected = Some(id.clone());
        let mut board = BoardState { session: id, ..BoardState::default() };
        match board.refresh(&cwd) {
            Ok(()) => {
                self.board = board;
                self.view = View::Board;
            }
            Err(why) => self.error = Some(why.into()),
        }
    }

    fn refresh_board(&mut self) {
        // A session that moved somewhere without a board keeps showing the last one.
        if let Some(cwd) = self.sessions.get(&self.board.session).map(|s| s.cwd.clone()) {
            let _ = self.board.refresh(&cwd);
        }
    }

    /// Opens the `c` picker with the prompts chosen last time ticked.
    fn open_rules_picker(&mut self) {
        let library = crate::rules::pickable();
        let saved = crate::settings::rules();
        let ticked = library.iter().map(|r| saved.contains(&r.key)).collect();
        self.rules_picker = Some(RulesPicker { library, ticked, cursor: 0, mode: None, deleting: None, versions: None, restorable: None });
    }

    /// Routes a key to the prompt writer; a saved prompt joins the picker's library, ticked.
    fn on_prompt_key(&mut self, key: KeyEvent) -> Action {
        use crate::prompt_view::Outcome;
        let Some(view) = self.prompt_view.as_mut() else { return Action::None };
        match view.on_key(key) {
            Outcome::Stay => {}
            Outcome::Close => self.prompt_view = None,
            Outcome::Edit(text) => return Action::EditPrompt(text),
            Outcome::Saved { key, replaced } => {
                let kind = view.kind();
                self.prompt_view = None;
                let noun = kind.noun();
                // An edited skill also reaches the copy jazari installed for Claude Code, unless
                // someone changed that copy by hand.
                let installed = match (kind == crate::rules::Kind::Skill).then(|| crate::skills::sync_installed(&key)).flatten() {
                    Some(Ok(report)) => format!(" · {}", report.message()),
                    Some(Err(e)) => format!(" · couldn't update the installed copy: {e:#}"),
                    None => String::new(),
                };
                match self.rules_picker.as_mut() {
                    Some(picker) => {
                        // A new entry or a copy is ticked; an edit or rename keeps the tick it had.
                        let tick = replaced.is_none_or(|old| picker.chosen().iter().any(|r| r.key == old));
                        picker.reload(crate::rules::pickable(), Some(&key), tick);
                        let ticked = if tick { " and ticked it" } else { "" };
                        self.info = Some(format!("saved {key} to your {noun}s{ticked}{installed} "));
                    }
                    None => self.info = Some(format!("saved {key} to your {noun}s{installed} · c to start a session with it ")),
                }
            }
        }
        Action::None
    }

    /// `y` deletes the prompt waiting for it; any other key keeps it.
    fn on_delete_key(&mut self, code: KeyCode) {
        let Some(picker) = self.rules_picker.as_mut() else { return };
        let Some(i) = picker.deleting.take() else { return };
        if code != KeyCode::Char('y') {
            return;
        }
        let (key, kind) = (picker.library[i].key.clone(), picker.library[i].kind);
        let removed = match kind {
            crate::rules::Kind::Prompt => crate::rules::remove_own(&key),
            crate::rules::Kind::Skill => crate::skills::remove_own(&key),
        };
        match removed {
            Ok(_) => {
                picker.reload(crate::rules::pickable(), None, false);
                picker.restorable = Some((key.clone(), kind));
                self.info = Some(format!("deleted {key} · u brings it back "));
            }
            Err(e) => self.error = Some(format!("{e:#} ")),
        }
    }

    /// Makes version `number` of `key` (the newest when None) the prompt or skill again and
    /// highlights it.
    fn restore_prompt(&mut self, key: &str, kind: crate::rules::Kind, number: Option<u32>) {
        let Some(picker) = self.rules_picker.as_mut() else { return };
        let restored = match kind {
            crate::rules::Kind::Prompt => crate::rules::restore_own(key, number),
            crate::rules::Kind::Skill => crate::skills::restore_own(key, number),
        };
        match restored {
            Ok(v) => {
                picker.reload(crate::rules::pickable(), Some(key), false);
                (picker.versions, picker.restorable) = (None, None);
                self.info = Some(format!("restored {key} to v{} from {} · the text it replaced is kept as a version ", v.number, v.when()));
            }
            Err(e) => self.error = Some(format!("{e:#} ")),
        }
    }

    /// Keys while browsing a prompt's earlier versions: `enter` restores the highlighted one.
    fn on_versions_key(&mut self, code: KeyCode) {
        let Some(list) = self.rules_picker.as_mut().and_then(|p| p.versions.as_mut()) else { return };
        let last = list.list.len().saturating_sub(1);
        match code {
            KeyCode::Esc | KeyCode::Char('q' | 'v') => {
                if let Some(p) = self.rules_picker.as_mut() {
                    p.versions = None;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => list.cursor = (list.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => list.cursor = list.cursor.saturating_sub(1),
            KeyCode::Enter => {
                let (key, kind, number) = (list.key.clone(), list.kind, list.list.get(list.cursor).map(|v| v.number));
                self.restore_prompt(&key, kind, number);
            }
            _ => {}
        }
    }

    fn on_picker_key(&mut self, code: KeyCode) {
        use crate::rules::Choice;
        let Some(picker) = self.rules_picker.as_mut() else { return };
        if let Some(mode) = picker.mode {
            match code {
                KeyCode::Esc => picker.mode = None,
                KeyCode::Char('q' | 'c') => self.rules_picker = None,
                KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => picker.mode = Some(mode.step(1)),
                KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => picker.mode = Some(mode.step(-1)),
                KeyCode::Char(d) if Choice::numbered(d).is_some() => picker.mode = Choice::numbered(d),
                KeyCode::Enter => {
                    let keys: Vec<String> = picker.chosen().iter().map(|r| r.key.clone()).collect();
                    let library = std::mem::take(&mut picker.library);
                    self.rules_picker = None;
                    let chosen = crate::rules::known(&library, &keys);
                    if self.copy_launch_command(&chosen, mode) {
                        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                        if let Err(e) = crate::settings::set_rules(&keys).and_then(|()| crate::settings::set_launch_mode(mode)) {
                            self.error = Some(format!("couldn't save the chosen prompts: {e:#}"));
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if picker.deleting.is_some() {
            self.on_delete_key(code);
            return;
        }
        if picker.versions.is_some() {
            self.on_versions_key(code);
            return;
        }
        let last = picker.library.len().saturating_sub(1);
        let highlighted = picker.library.get(picker.cursor);
        match code {
            KeyCode::Esc | KeyCode::Char('q' | 'c') => self.rules_picker = None,
            // A new entry of the highlighted group: a prompt among the prompts, a skill among the skills.
            KeyCode::Char('n') => {
                let kind = highlighted.map(|r| r.kind).unwrap_or_default();
                self.prompt_view = Some(crate::prompt_view::PromptView::new(crate::prompt_view::Env::real(kind)));
            }
            KeyCode::Char('e') => {
                match highlighted.map(|rule| (rule, crate::rules::copy_problem(rule))) {
                    Some((_, Some(why))) => self.error = Some(format!("{why} ")),
                    Some((rule, None)) => self.prompt_view = Some(crate::prompt_view::PromptView::revise(crate::prompt_view::Env::real(rule.kind), rule)),
                    None => {}
                }
            }
            KeyCode::Char('d') => match highlighted {
                Some(rule) if rule.own => picker.deleting = Some(picker.cursor),
                Some(rule) => self.error = Some(format!("{} is built in and can't be deleted; e saves a copy you can change ", rule.title)),
                None => {}
            },
            KeyCode::Char('v') => match highlighted {
                Some(rule) if rule.own => {
                    let list = match rule.kind {
                        crate::rules::Kind::Prompt => crate::rules::versions(&rule.key),
                        crate::rules::Kind::Skill => crate::skills::versions(&rule.key),
                    };
                    if list.is_empty() {
                        self.info = Some(format!("{} has no earlier versions yet; one is kept each time it changes ", rule.title));
                    } else {
                        picker.versions = Some(VersionList { key: rule.key.clone(), kind: rule.kind, title: rule.title.clone(), list, cursor: 0 });
                    }
                }
                Some(rule) => self.error = Some(format!("{} is built in, so it has no versions ", rule.title)),
                None => {}
            },
            KeyCode::Char('u') => match picker.restorable.clone() {
                Some((key, kind)) => self.restore_prompt(&key, kind, None),
                None => self.error = Some("nothing deleted to bring back ".into()),
            },
            KeyCode::Down | KeyCode::Char('j') => picker.cursor = (picker.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => picker.cursor = picker.cursor.saturating_sub(1),
            KeyCode::Char(' ' | 'x') => picker.toggle(picker.cursor),
            KeyCode::Char(d @ '1'..='9') => picker.toggle(d as usize - '1' as usize),
            KeyCode::Enter if picker.chosen().is_empty() => self.error = Some("tick at least one with space ".into()),
            KeyCode::Enter => picker.mode = Some(crate::settings::launch_mode()),
            _ => {}
        }
    }

    /// Copies the command that starts `choice` with the prompts in `chosen`, or only their file's
    /// path, writing the files first. False when a file couldn't be written, so nothing was copied.
    fn copy_launch_command(&mut self, chosen: &[&crate::rules::Rule], choice: crate::rules::Choice) -> bool {
        let mode = choice.mode();
        // A ticked skill must be where Claude Code loads skills before a session is told to use it.
        let installed = match crate::skills::provide(chosen) {
            Ok(reports) => reports.iter().map(|r| format!(" · {}", r.message())).collect::<String>(),
            Err(e) => {
                self.error = Some(format!("{e:#} "));
                return false;
            }
        };
        let path = match crate::rules::ensure(chosen, mode) {
            Ok(path) => path,
            Err(e) => {
                self.error = Some(format!("{e:#} "));
                return false;
            }
        };
        let path_only = choice == crate::rules::Choice::Path;
        let command = if path_only { path.to_string_lossy().into_owned() } else { crate::rules::launch_command(&path) };
        let headline = match crate::clipboard::copy(&command) {
            crate::clipboard::Copied::Native(_) if path_only => {
                self.info = Some(format!("copied: the path of {} · paste it into a running session{installed} ", crate::rules::titles(chosen)));
                return true;
            }
            crate::clipboard::Copied::Native(_) => {
                let lead = if mode == crate::rules::Mode::Team { " as a team lead" } else { "" };
                self.info = Some(format!("copied: claude{lead} with {} · it applies to new conversations{installed} ", crate::rules::titles(chosen)));
                return true;
            }
            crate::clipboard::Copied::Terminal => "Sent to your terminal's clipboard, if it allows that. To copy it by hand:",
            crate::clipboard::Copied::Failed => "Couldn't reach a clipboard. Copy this command by hand:",
        };
        let path = crate::rules::quote_arg(&path.to_string_lossy());
        let cli = crate::rules::cli_command(chosen, mode) + if path_only { " --path" } else { "" };
        self.copy_note = Some(CopyNote { headline, command, path, cli });
        true
    }

    /// Opens or closes the always-on-top window with the sessions' statuses.
    fn toggle_pin(&mut self) {
        if self.pin.take().is_some() {
            self.info = Some("closed the window on top ".into());
            return;
        }
        match crate::pin::Pin::open(&self.pin_text()) {
            Ok(pin) => {
                self.pin = Some(pin);
                self.info = Some("opened a window that stays on top of other apps · P closes it ".into());
            }
            Err(e) => self.error = Some(format!("{e:#} ")),
        }
    }

    fn pin_text(&self) -> String {
        let ctx = self.selected_session().and_then(|s| s.context(self.compact_window));
        crate::pin::text(&self.visible(), ctx, self.rate_limits())
    }

    /// Keeps the window on top current, and notices when the user closed it.
    fn refresh_pin(&mut self) {
        if self.pin.is_none() {
            return;
        }
        let text = self.pin_text();
        let Some(pin) = self.pin.as_mut() else { return };
        if pin.is_open() {
            pin.show(&text);
        } else {
            self.pin = None;
        }
    }

    /// Copies the selected session's full id, for `claude --resume` or a bug report.
    fn copy_session_id(&mut self) {
        let Some(text) = self.selected_session().map(|s| s.mention()) else { return };
        match crate::clipboard::copy(&text) {
            crate::clipboard::Copied::Native(_) => self.info = Some(format!("copied {text} ")),
            crate::clipboard::Copied::Terminal => self.info = Some(format!("sent {text} to your terminal's clipboard ")),
            crate::clipboard::Copied::Failed => self.error = Some(format!("couldn't reach a clipboard; the session is {text} ")),
        }
    }

    /// The rank and achievements now, with the header's all-time figure as the rank total.
    fn load_badges(&self) -> anyhow::Result<Badges> {
        let conn = crate::store::open_readonly()?;
        Ok(crate::badges::badges(&crate::badges::shown(&conn, self.counter.totals().1)?))
    }

    /// After a background sync that changed something: the badges from the stats it recorded, and a
    /// note for any level that rose.
    fn after_sync(&mut self, mut stats: crate::badges::Stats) {
        // The header's figure also counts live usage the index hasn't caught up with.
        let rank = stats.0.entry(crate::badges::RANK_KEY).or_default();
        *rank = (*rank).max(self.counter.totals().1);
        self.announce(crate::badges::badges(&stats));
    }

    /// Shows `b` in the badge view, with a note for any level that rose since the last announcement.
    fn announce(&mut self, b: Badges) {
        match crate::badges::pending_announcement(&b) {
            Ok(Some(text)) => self.announcement = Some(Announcement::new(text, b.clone())),
            Ok(None) => {}
            Err(e) => self.error = Some(format!("couldn't save the announced badges: {e:#}")),
        }
        self.badges = Some(b);
    }

    fn open_share(&mut self) {
        // The badges show beside the post while the share syncs and freezes its own.
        if self.badges.is_none() {
            self.badges = self.load_badges().ok();
        }
        self.share = Some(ShareView::start(crate::share_view::Env::real(self.counter.totals().1)));
    }

    /// The footer's note now. A badge note shows only in its views and waits while a share is open.
    pub fn footer_note(&self) -> Option<FooterNote<'_>> {
        if let Some(e) = &self.error {
            return Some(FooterNote::Error(e));
        }
        if let Some(i) = &self.info {
            return Some(FooterNote::Info(i));
        }
        let badge = self.announcement.as_ref().filter(|_| self.share.is_none() && Announcement::shows_in(self.view));
        if let Some(a) = badge {
            return Some(FooterNote::Badge(&a.text));
        }
        self.update.as_ref().map(FooterNote::Update)
    }

    fn announcing(&self) -> bool {
        matches!(self.footer_note(), Some(FooterNote::Badge(_)))
    }

    /// After each frame: records a badge note the first time it was drawn, and lets it expire.
    fn after_draw(&mut self) {
        let now = Instant::now();
        expire_info(&self.error, &mut self.info, &mut self.note_shown, now);
        let shown = self.announcing();
        let Some(a) = self.announcement.as_mut() else { return };
        if a.on_drawn(shown, now)
            && let Err(e) = crate::badges::mark_announced(&a.badges)
        {
            self.error = Some(format!("couldn't save the announced badges: {e:#}"));
        }
        if a.expired(now) {
            self.announcement = None;
        }
    }

    /// Changes the sort order while keeping the highlighted session selected.
    fn set_sort(&mut self, key: SortKey, reversed: bool) {
        self.selected = self.selected_session().map(|s| s.id.clone());
        self.sort = key;
        self.sort_reversed = reversed;
    }

    /// The selected session's history events that pass the filter, newest first.
    pub fn history<'a>(&self, s: &'a Session) -> Vec<TimelineEntry<'a>> {
        let mut list = s.timeline();
        list.retain(|e| self.history_filter.matches(&e.event.kind));
        list.reverse();
        list
    }

    /// Scrolls the detail pane, stopping near the end of the selected event's text.
    fn scroll_detail(&mut self, delta: i32) {
        let lines = self.selected_session().map_or(0, |s| {
            let events = self.history(s);
            events.get(self.history_scroll.min(events.len().saturating_sub(1))).map_or(0, |e| match &e.event.kind {
                EventKind::Prompt(t) | EventKind::Response(t) => t.lines().count(),
                EventKind::Agent { result, .. } => result.as_deref().map_or(0, |r| r.lines().count()) + 6,
                EventKind::Question { question, answer } => question.lines().count() + answer.as_deref().map_or(1, |a| a.lines().count()) + 3,
                EventKind::Skill(_) => 0,
            })
        });
        self.detail_scroll = (self.detail_scroll as i32 + delta).clamp(0, lines as i32) as u16;
    }

    fn scroll(&mut self, delta: isize) {
        let len = match (self.view, self.selected_session()) {
            (View::Agents, Some(s)) => s.subagents.len() + 1,
            (_, Some(s)) => self.history(s).len(),
            _ => 0,
        };
        let max = len.saturating_sub(1) as isize;
        self.history_scroll = (self.history_scroll as isize + delta).clamp(0, max) as usize;
        self.detail_scroll = 0;
    }

    fn refresh(&mut self) {
        if let Some(stats) = self.usage.tick() {
            self.after_sync(stats);
        }
        if let Some(view) = self.prompt_view.as_mut() {
            view.tick();
        }
        let prepared = self.share.as_mut().and_then(|share| {
            share.tick();
            share.take_prepared()
        });
        // The share synced on its own thread, so the dashboard's next sync finds nothing new: take its
        // rows and any badge level they raised here.
        if let Some(b) = prepared {
            self.usage.reload();
            self.announce(b);
        }
        if let Some(clean) = self.clean.as_mut() {
            clean.tick();
        }
        if let Some(Ok(bytes)) = self.clean_hint_rx.as_ref().map(|rx| rx.try_recv()) {
            self.clean_hint_rx = None;
            if self.info.is_none() && self.error.is_none() {
                self.info = Some(format!("{} can be freed · press D ", crate::clean::fmt_bytes(bytes)));
            }
        }
        if let Ok(notice) = self.update_rx.try_recv() {
            self.update = Some(notice);
        }
        for ev in self.log.read_new() {
            events::apply(&mut self.sessions, &ev);
        }
        if self.last_slow_refresh.elapsed() >= Duration::from_secs(1) {
            self.last_slow_refresh = Instant::now();
            self.check_processes();
            self.refresh_transcripts();
            self.refresh_snapshots();
            self.counter.reload_if_due();
            self.sample_system();
            self.refresh_pin();
            if self.view == View::Board {
                self.refresh_board();
            }
        }
        // Every refresh, so a Stop that leaves a background shell never shows as idle.
        let now = crate::now();
        for s in self.sessions.values_mut() {
            s.reconcile(now);
        }
        let alerts = self.alerts.observe(self.sessions.values());
        if self.alerts_on {
            alerts.fire();
        }
    }

    fn on_compose_key(&mut self, code: KeyCode) {
        let Some(c) = self.compose.as_mut() else { return };
        match code {
            KeyCode::Esc => self.compose = None,
            KeyCode::Backspace => {
                c.text.pop();
            }
            KeyCode::Char(ch) => c.text.push(ch),
            KeyCode::Enter if c.text.trim().is_empty() => {}
            KeyCode::Enter => {
                let c = self.compose.take().unwrap();
                match crate::channel::tell_and_wake(&c.session, &c.text) {
                    Ok(true) => self.info = Some(format!("queued for {}, and sent a wake-up to the idle session ", c.name)),
                    Ok(false) => self.info = Some(format!("queued for {}: Claude reads it at the next tool call or prompt ", c.name)),
                    Err(e) => self.error = Some(format!("couldn't queue the message: {e:#} ")),
                }
            }
            _ => {}
        }
    }

    /// Takes in new prompts, and closes the answer box when its prompt was answered at the
    /// terminal or timed out.
    fn poll_answers(&mut self) {
        let Some(server) = self.answers.as_mut() else { return };
        server.poll(crate::now());
        if let Some(v) = &self.answer_view
            && server.waiting(&v.session) != Some(&v.req)
        {
            self.info = Some(format!("{}'s prompt was answered at the terminal or has closed ", v.name));
            self.answer_view = None;
        }
    }

    /// Opens the answer box for the selected session's waiting prompt.
    fn open_answer(&mut self) {
        let Some(s) = self.selected_session() else { return };
        let Some(answers) = self.answers.as_ref() else {
            self.info = Some("another jazari dashboard answers prompts, or this system can't (macOS and Linux only) ".into());
            return;
        };
        match answers.waiting(&s.id) {
            Some(req) => self.answer_view = Some(crate::answer_view::AnswerView::open(&s.id, s.project(), req.clone())),
            None if !crate::answer::enabled() => self.info = Some("answering from the dashboard is off: turn it on with `jazari watch answer --on` ".into()),
            None => self.info = Some(format!("{} has no prompt waiting for jazari ", s.project())),
        }
    }

    fn on_answer_key(&mut self, code: KeyCode) {
        let Some(v) = self.answer_view.as_mut() else { return };
        match v.on_key(code) {
            crate::answer_view::Key::Stay => {}
            crate::answer_view::Key::Close => self.answer_view = None,
            crate::answer_view::Key::Send(answer) => {
                let v = self.answer_view.take().unwrap();
                match self.answers.as_mut().map(|a| a.answer(&v.session, answer)) {
                    Some(Ok(())) => self.info = Some(format!("answered {}'s prompt ", v.name)),
                    Some(Err(e)) => self.error = Some(format!("couldn't answer: {e:#} ")),
                    None => {}
                }
            }
        }
    }

    /// Text pasted while composing goes into the message, on one line.
    pub fn on_compose_paste(&mut self, text: &str) -> bool {
        let Some(c) = self.compose.as_mut() else { return false };
        c.text.push_str(&text.replace(['\r', '\n'], " "));
        true
    }

    fn apply_control(&mut self, c: &Control) {
        if c.interrupt {
            let sent = self.sessions.get(&c.session).map(crate::channel::interrupt);
            match sent {
                Some(Ok(())) => self.info = Some(format!("interrupted {} ", c.name)),
                Some(Err(e)) => self.error = Some(format!("couldn't interrupt {}: {e:#} ", c.name)),
                None => self.error = Some(format!("couldn't interrupt {}: the session is gone ", c.name)),
            }
        } else {
            match crate::channel::request_stop(&c.session) {
                Ok(()) => self.info = Some(format!("{} stops before its next tool call ", c.name)),
                Err(e) => self.error = Some(format!("couldn't request the stop: {e:#} ")),
            }
        }
    }

    /// Statfs, memory counters, CPU ticks and one Finder free-space call (about 5 ms): cheap enough
    /// to stay on this thread.
    fn sample_system(&mut self) {
        let cwds = self.sessions.values().filter(|s| s.status != Status::Ended).map(|s| s.cwd.clone());
        let paths = crate::system::watched_paths(cwds);
        let t = crate::disk::Config::load().thresholds;
        self.system = Some(self.sampler.sample(paths.iter().map(PathBuf::as_path), &t));
    }

    /// Marks sessions whose Claude Code process is gone (crash, closed terminal) as ended.
    fn check_processes(&mut self) {
        mark_exited(&mut self.sessions, &mut self.sys);
    }

    fn refresh_snapshots(&mut self) {
        load_snapshots(&mut self.sessions);
    }

    fn refresh_transcripts(&mut self) {
        let ids: Vec<String> = self.visible().iter().map(|s| s.id.clone()).collect();
        for id in ids {
            let Some(session) = self.sessions.get_mut(&id) else { continue };
            let Some(path) = session.transcript_path.clone() else { continue };

            // Keyed like the indexer: the transcript's file name and the agent id.
            let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let reader = self.readers.entry(path.clone()).or_insert_with(|| TranscriptReader::new(path.clone()));
            if reader.poll() {
                self.counter.set_live(&stem, crate::model::MAIN_AGENT, reader);
                session.stats = reader.stats();
                if session.model.is_none() {
                    session.model = session.stats.model.clone();
                }
            }

            let dir = transcript::subagent_dir(&path);
            for (agent_id, file) in transcript::subagent_files(&dir) {
                let agent = session.subagents.entry(agent_id.clone()).or_insert_with(Subagent::default);
                if !agent.meta_loaded
                    && let Some(meta) = transcript::subagent_meta(&dir, &agent_id) {
                        agent.meta_loaded = true;
                        if let Some(t) = meta.agent_type {
                            agent.agent_type = t;
                        }
                        agent.description = meta.description.unwrap_or_default();
                        agent.tool_use_id = meta.tool_use_id;
                    }
                if agent.agent_type.is_empty() {
                    agent.agent_type = "agent".into();
                }
                let reader = self.readers.entry(file.clone()).or_insert_with(|| TranscriptReader::subagent(file));
                if reader.poll() {
                    self.counter.set_live(&stem, &agent_id, reader);
                    agent.stats = reader.stats();
                }
            }
        }
    }
}

impl App {
    /// Scrolls the wheel as ↓/↑ would, except over a text box or a waiting y/n question, which a
    /// stray scroll shouldn't type into or answer.
    fn on_wheel(&mut self, down: bool) {
        let asking = self.compose.is_some()
            || self.control.is_some()
            || self.answer_view.is_some()
            || self.prompt_view.is_some()
            || self.share.as_ref().is_some_and(|s| s.editing())
            || self.rules_picker.as_ref().is_some_and(|p| p.deleting.is_some())
            || (self.view == View::Clean && self.clean.as_ref().is_some_and(|c| c.confirm.is_some()));
        if !asking {
            let code = if down { KeyCode::Down } else { KeyCode::Up };
            self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    /// Handles one key press. The share view gets keys first, so `q` there cancels the share.
    fn on_key(&mut self, key: KeyEvent) -> Action {
        if let Some(share) = self.share.as_mut() {
            match share.on_key(key) {
                Outcome::Stay => {}
                Outcome::Close => self.share = None,
                Outcome::EditPost(text) => return Action::EditPost(text),
            }
            return Action::None;
        }
        if self.prompt_view.is_some() {
            (self.error, self.info, self.note_shown) = (None, None, None);
            return self.on_prompt_key(key);
        }
        if self.rules_picker.is_some() {
            (self.error, self.info, self.note_shown) = (None, None, None);
            self.on_picker_key(key.code);
            return Action::None;
        }
        if self.compose.is_some() {
            self.on_compose_key(key.code);
            return Action::None;
        }
        if self.answer_view.is_some() {
            self.on_answer_key(key.code);
            return Action::None;
        }
        if let Some(c) = self.control.take() {
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.apply_control(&c);
            }
            return Action::None;
        }
        // Esc dismisses a badge note before it does anything else.
        if key.code == KeyCode::Esc && self.announcing() {
            self.announcement = None;
            return Action::None;
        }
        (self.error, self.info, self.note_shown, self.copy_note) = (None, None, None, None);
        match (self.view, key.code) {
            (_, KeyCode::Char('q')) => return Action::Quit,
            (View::Usage, code) => {
                if !self.usage.on_key(code) {
                    self.view = View::List;
                }
            }
            (View::List, KeyCode::Char('u')) => {
                self.view = View::Usage;
                self.usage.reload();
            }
            (View::List, KeyCode::Char('t')) => {
                if let Some(s) = self.selected_session().filter(|s| s.status != Status::Ended) {
                    self.compose = Some(Compose { session: s.id.clone(), name: s.project(), text: String::new() });
                }
            }
            (View::List, KeyCode::Char('r')) => self.open_answer(),
            (View::List, KeyCode::Char(k @ ('x' | 'X'))) => {
                if let Some(s) = self.selected_session().filter(|s| s.status != Status::Ended) {
                    self.control = Some(Control { session: s.id.clone(), name: s.project(), interrupt: k == 'X' });
                }
            }
            (View::List, KeyCode::Char('D')) => {
                self.clean = Some(crate::clean_view::CleanView::open());
                self.view = View::Clean;
            }
            (View::Clean, code) => {
                if !self.clean.as_mut().is_some_and(|c| c.on_key(code)) {
                    self.view = View::List;
                }
            }
            (View::List, KeyCode::Esc) => return Action::Quit,
            (View::List, KeyCode::Down | KeyCode::Char('j')) => self.move_selection(1),
            (View::List, KeyCode::Up | KeyCode::Char('k')) => self.move_selection(-1),
            (View::List, KeyCode::Home | KeyCode::Char('g')) => self.move_selection(isize::MIN / 2),
            (View::List, KeyCode::End | KeyCode::Char('G')) => self.move_selection(isize::MAX / 2),
            (View::List, KeyCode::Char('a')) => self.show_ended = !self.show_ended,
            (View::List, KeyCode::Char('m')) => {
                self.alerts_on = !self.alerts_on;
                if let Err(e) = crate::settings::set_alerts(self.alerts_on) {
                    self.error = Some(format!("couldn't save alerts setting: {e}"));
                }
            }
            (View::List, KeyCode::Char('p')) => self.open_share(),
            (View::List, KeyCode::Char('P')) => self.toggle_pin(),
            (View::List | View::History | View::Agents, KeyCode::Char('y')) => self.copy_session_id(),
            (View::List | View::History | View::Agents, KeyCode::Char('b')) => self.open_board(),
            (View::List | View::Board, KeyCode::Char('c')) => self.open_rules_picker(),
            (View::List, KeyCode::Char('n')) => self.prompt_view = Some(crate::prompt_view::PromptView::new(crate::prompt_view::Env::real(crate::rules::Kind::Prompt))),
            (View::Board, KeyCode::Esc | KeyCode::Char('b') | KeyCode::Left | KeyCode::Char('h')) => self.view = View::List,
            (View::Board, KeyCode::Down | KeyCode::Char('j')) => self.board.scroll(1),
            (View::Board, KeyCode::Up | KeyCode::Char('k')) => self.board.scroll(-1),
            (View::Board, KeyCode::PageDown | KeyCode::Char(' ')) => self.board.scroll(10),
            (View::Board, KeyCode::PageUp) => self.board.scroll(-10),
            (View::Board, KeyCode::Home | KeyCode::Char('g')) => self.board.scroll(isize::MIN / 2),
            (View::Board, KeyCode::End | KeyCode::Char('G')) => self.board.scroll(isize::MAX / 2),
            (View::List, KeyCode::Char('s')) => self.set_sort(self.sort.next(), false),
            (View::List, KeyCode::Char('S')) => self.set_sort(self.sort, !self.sort_reversed),
            (View::List, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')) => self.open(View::History),
            (View::History, KeyCode::Char('f')) => {
                self.history_filter = self.history_filter.next();
                self.history_scroll = 0;
                self.detail_scroll = 0;
            }
            (View::History, KeyCode::Char('J')) => self.scroll_detail(3),
            (View::History, KeyCode::Char('K')) => self.scroll_detail(-3),
            (View::List | View::History, KeyCode::Tab) => self.open(View::Agents),
            (View::Agents, KeyCode::Tab) => self.open(View::History),
            (View::History | View::Agents, KeyCode::Esc | KeyCode::Enter | KeyCode::Left | KeyCode::Char('h')) => self.view = View::List,
            (View::History | View::Agents, KeyCode::Down | KeyCode::Char('j')) => self.scroll(1),
            (View::History | View::Agents, KeyCode::Up | KeyCode::Char('k')) => self.scroll(-1),
            (View::History | View::Agents, KeyCode::PageDown | KeyCode::Char(' ')) => self.scroll(10),
            (View::History | View::Agents, KeyCode::PageUp) => self.scroll(-10),
            (View::History | View::Agents, KeyCode::Home | KeyCode::Char('g')) => self.scroll(isize::MIN / 2),
            (View::History | View::Agents, KeyCode::End | KeyCode::Char('G')) => self.scroll(isize::MAX / 2),
            _ => {}
        }
        Action::None
    }
}

type Tui = ratatui::Terminal<crate::term::Aligned<ratatui::backend::CrosstermBackend<std::io::Stdout>>>;

/// ratatui's terminal setup (raw mode, alternate screen, panic hook), drawing through
/// `term::Aligned` so complex-script text can't shift a row's columns. Mouse capture sends the wheel
/// to jazari, so the terminal doesn't scroll its own history behind the dashboard.
fn init_terminal() -> std::io::Result<Tui> {
    drop(ratatui::init());
    static RELEASE_MOUSE_ON_PANIC: std::sync::Once = std::sync::Once::new();
    RELEASE_MOUSE_ON_PANIC.call_once(|| {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
            hook(info);
        }));
    });
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    ratatui::Terminal::new(crate::term::Aligned::new(ratatui::backend::CrosstermBackend::new(std::io::stdout())))
}

/// The ANSI escape codes that undo `init_terminal` and the bracketed paste that `run` turns on:
/// mouse capture off, bracketed paste off, back to the main screen, cursor shown. Raw mode is a
/// terminal setting, not an escape code, so the caller turns it off. These are the plain codes on
/// every platform; `restore_terminal` sends the same commands through crossterm, which uses the
/// console API instead on a Windows console that doesn't take ANSI.
#[cfg_attr(not(unix), allow(dead_code))]
fn restore_codes() -> String {
    use ratatui::crossterm::{Command, cursor::Show, terminal::LeaveAlternateScreen};
    let mut codes = String::new();
    let _ = DisableMouseCapture.write_ansi(&mut codes);
    let _ = DisableBracketedPaste.write_ansi(&mut codes);
    let _ = LeaveAlternateScreen.write_ansi(&mut codes);
    let _ = Show.write_ansi(&mut codes);
    codes
}

/// Undoes `init_terminal`.
fn restore_terminal() {
    use ratatui::crossterm::{cursor::Show, terminal::LeaveAlternateScreen};
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen, Show);
}

/// Restores the terminal when the dashboard is stopped by SIGTERM, SIGHUP (its tab was closed),
/// SIGINT or SIGQUIT, then exits with 128 + the signal number. Without this, the terminal stays in
/// raw mode with mouse capture on, and every mouse move is typed into the shell.
///
/// The handler only calls `write`, `tcsetattr` and `_exit`, which are safe in a signal handler.
/// So the escape codes and the terminal settings from before raw mode are prepared here.
#[cfg(unix)]
mod on_signal {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI32, Ordering};

    static CODES: OnceLock<Vec<u8>> = OnceLock::new();
    static SETTINGS: OnceLock<libc::termios> = OnceLock::new();
    static TTY: AtomicI32 = AtomicI32::new(-1);

    /// Call before raw mode is turned on, so the saved settings are the user's own.
    pub fn install() {
        let _ = CODES.set(super::restore_codes().into_bytes());
        // The same terminal that crossterm puts in raw mode: stdin, or /dev/tty when stdin isn't one.
        let fd = unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 1 { libc::STDIN_FILENO } else { libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) }
        };
        if fd >= 0 {
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut t) } == 0 && SETTINGS.set(t).is_ok() {
                TTY.store(fd, Ordering::SeqCst);
            } else if fd != libc::STDIN_FILENO {
                // A second call, or /dev/tty has no settings: the handler doesn't use this fd.
                unsafe { libc::close(fd) };
            }
        }
        let handler = handle as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
        }
    }

    extern "C" fn handle(sig: libc::c_int) {
        unsafe {
            let fd = TTY.load(Ordering::SeqCst);
            if fd >= 0 && let Some(t) = SETTINGS.get() {
                libc::tcsetattr(fd, libc::TCSANOW, t);
            }
            if let Some(codes) = CODES.get() {
                libc::write(libc::STDOUT_FILENO, codes.as_ptr().cast(), codes.len());
            }
            libc::_exit(128 + sig);
        }
    }
}

/// Windows has no such signals. Closing the console window ends the console, and with it the
/// console modes that jazari changed.
#[cfg(not(unix))]
mod on_signal {
    pub fn install() {}
}

/// Hands the terminal to the user's editor for `text`, then takes it back: a fresh terminal and what
/// was saved.
fn edit_suspended(text: &str, kind: &str, ext: &str) -> std::io::Result<(Tui, Result<String, String>)> {
    restore_terminal();
    let edited = crate::share_view::edit_in_editor(text, kind, ext);
    let terminal = init_terminal()?;
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    Ok((terminal, edited))
}

pub fn run() -> anyhow::Result<()> {
    let mut app = App::new();
    app.answers = crate::answer::server::start();
    app.refresh();
    on_signal::install();
    let mut terminal = init_terminal()?;
    app.cell_aspect = crate::ui::cell_aspect(terminal.backend_mut().window_size().ok());
    // Bracketed paste, so a pasted newline while editing the share line doesn't confirm it.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let result = (|| -> anyhow::Result<()> {
        loop {
            terminal.draw(|f| crate::ui::draw(f, &app))?;
            app.after_draw();
            if event::poll(Duration::from_millis(250))? {
                match event::read()? {
                    Event::Paste(text) => {
                        if app.on_compose_paste(&text) {
                        } else if let Some(share) = app.share.as_mut() {
                            share.on_paste(&text);
                        } else if let Some(view) = app.prompt_view.as_mut() {
                            view.on_paste(&text);
                        }
                    }
                    Event::Key(key) if key.kind == KeyEventKind::Press => match app.on_key(key) {
                        Action::None => {}
                        Action::Quit => return Ok(()),
                        Action::EditPost(text) => {
                            let edited;
                            (terminal, edited) = edit_suspended(&text, "post", ".txt")?;
                            if let Some(share) = app.share.as_mut() {
                                share.edited_post(edited);
                            }
                        }
                        Action::EditPrompt(text) => {
                            let edited;
                            (terminal, edited) = edit_suspended(&text, "prompt", ".md")?;
                            if let Some(view) = app.prompt_view.as_mut() {
                                view.edited(edited);
                            }
                        }
                    },
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::ScrollDown => app.on_wheel(true),
                        MouseEventKind::ScrollUp => app.on_wheel(false),
                        _ => {}
                    },
                    Event::Resize(..) => app.cell_aspect = crate::ui::cell_aspect(terminal.backend_mut().window_size().ok()),
                    _ => {}
                }
            }
            app.tick = app.tick.wrapping_add(1);
            app.poll_answers();
            app.refresh();
        }
    })();
    restore_terminal();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_restore_codes_turn_off_mouse_paste_and_the_alternate_screen() {
        let text = restore_codes();
        for code in ["?1000l", "?1002l", "?1003l", "?1006l", "?2004l", "?1049l", "?25h"] {
            assert!(text.contains(code), "{code} missing from {text:?}");
        }
        let at = |code: &str| text.find(code).unwrap();
        assert!(at("?1000l") < at("?1049l") && at("?2004l") < at("?1049l"), "the alternate screen is left last");
    }

    fn key(s: &str, a: &str) -> (String, String) {
        (s.to_string(), a.to_string())
    }

    #[test]
    fn live_usage_tops_up_the_indexed_counter() {
        let day = "2026-10-23";
        let indexed: DayTotals = HashMap::from([(key("s1", "main"), 1_000), (key("s2", "main"), 500)]);
        let live = HashMap::from([
            // Covers AE3: ahead of the index by 200.
            (key("s1", "main"), (day.to_string(), 1_200)),
            // Not indexed yet at all.
            (key("s3", "a1"), (day.to_string(), 50)),
            // Behind the index: the index wins.
            (key("s2", "main"), (day.to_string(), 400)),
            // Yesterday's figure from a reader that hasn't polled since midnight.
            (key("s4", "main"), ("2026-10-22".to_string(), 999)),
        ]);
        assert_eq!(merge_live(&indexed, 10_000, day, &live), (1_500 + 250, 10_000 + 250));
        assert_eq!(merge_live(&DayTotals::new(), 0, day, &HashMap::new()), (0, 0));
    }

    const ONE_AGENT: &str = "## Agents\n| agent | task | branch/worktree | status | started | last seen |\n|---|---|---|---|---|---|\n| agent-a | fix | a | active | 2026-10-23 10:00 | 2026-10-23 10:00 |\n";

    fn agents(state: &BoardState) -> Vec<String> {
        state.board.as_ref().map(|b| b.agents.iter().map(|a| a.name.clone()).collect()).unwrap_or_default()
    }

    #[test]
    fn the_board_view_follows_the_file() {
        let root = std::env::temp_dir().join(format!("jazari-app-board-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cwd = root.to_str().unwrap();
        let mut state = BoardState::default();

        state.refresh(cwd).unwrap();
        let path = state.path.clone().unwrap();
        assert!(state.missing && state.board.is_none(), "no file yet");

        std::fs::write(&path, ONE_AGENT).unwrap();
        state.refresh(cwd).unwrap();
        assert!(!state.missing);
        assert_eq!(agents(&state), ["agent-a"]);

        std::fs::write(&path, format!("{ONE_AGENT}| agent-bb | docs | b | active | 2026-10-23 10:00 | 2026-10-23 10:00 |\n")).unwrap();
        state.refresh(cwd).unwrap();
        assert_eq!(agents(&state), ["agent-a", "agent-bb"], "a changed file is re-read");

        // Same modified time and length: the parse is reused, not re-read.
        state.board = None;
        state.refresh(cwd).unwrap();
        assert!(state.board.is_none());
        state.key = None;
        state.refresh(cwd).unwrap();
        assert_eq!(agents(&state).len(), 2);

        // A failed read keeps the last good parse.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        state.refresh(cwd).unwrap();
        assert!(state.read_error.is_some());
        assert_eq!(agents(&state).len(), 2);

        std::fs::remove_dir(&path).unwrap();
        state.refresh(cwd).unwrap();
        assert!(state.missing && state.board.is_none() && state.read_error.is_none(), "deleted again");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn the_board_view_resets_when_the_session_moves() {
        let root = std::env::temp_dir().join(format!("jazari-app-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join(crate::board::FILE), ONE_AGENT).unwrap();
        let mut state = BoardState::default();
        state.refresh(a.to_str().unwrap()).unwrap();
        state.max_scroll.set(50);
        state.scroll(10);
        assert_eq!((state.scroll, agents(&state).len()), (10, 1));
        state.refresh(b.to_str().unwrap()).unwrap();
        assert_eq!(state.scroll, 0);
        assert!(state.missing && state.board.is_none());

        assert_eq!(state.refresh(""), Err("this session has no working directory yet"));
        assert_eq!(state.refresh(root.join("gone").to_str().unwrap()), Err("this session's directory no longer exists"));
        std::fs::remove_dir_all(root).ok();
    }

    fn badges() -> Badges {
        crate::badges::badges(&crate::badges::Stats::default())
    }

    #[test]
    fn an_announcement_waits_for_its_view_and_is_recorded_once_drawn() {
        let mut a = Announcement::new("rank up: Expert · new: gold streak".into(), badges());
        let start = Instant::now();
        // Arrives while a share is open: not on screen, so not recorded.
        assert!(!a.on_drawn(false, start));
        assert_eq!(a.drawn, None);
        assert!(!a.expired(start + ANNOUNCE_FOR * 2), "a note never drawn never expires");
        // The share closes over the list: drawn, recorded once.
        assert!(a.on_drawn(Announcement::shows_in(View::List), start), "both levels in one note");
        assert!(!a.on_drawn(Announcement::shows_in(View::List), start), "recorded once");
        assert!(!a.expired(start + ANNOUNCE_FOR / 2), "it outlives the next key press");
        assert!(a.expired(start + ANNOUNCE_FOR));
        for view in [View::History, View::Agents, View::Usage, View::Board] {
            assert!(!Announcement::shows_in(view));
        }
    }

    #[test]
    fn info_notes_expire_after_their_time_on_screen() {
        let (mut info, mut shown) = (Some("copied".to_string()), None);
        let start = Instant::now();
        expire_info(&None, &mut info, &mut shown, start);
        expire_info(&None, &mut info, &mut shown, start + INFO_FOR / 2);
        assert!(info.is_some(), "still within its time");
        expire_info(&None, &mut info, &mut shown, start + INFO_FOR);
        assert!(info.is_none() && shown.is_none());

        // An error covers the info and never expires; the info's clock starts once it shows.
        let error = Some("failed".to_string());
        info = Some("copied".into());
        expire_info(&error, &mut info, &mut shown, start);
        expire_info(&error, &mut info, &mut shown, start + INFO_FOR * 3);
        assert!(info.is_some() && shown.is_none());
        expire_info(&None, &mut info, &mut shown, start + INFO_FOR * 3);
        expire_info(&None, &mut info, &mut shown, start + INFO_FOR * 4);
        assert!(info.is_none());

        // A new text restarts the clock.
        info = Some("one".into());
        expire_info(&None, &mut info, &mut shown, start);
        info = Some("two".into());
        expire_info(&None, &mut info, &mut shown, start + INFO_FOR);
        assert_eq!(info.as_deref(), Some("two"));
    }

    #[test]
    fn board_scrolling_is_clamped() {
        let mut state = BoardState::default();
        state.max_scroll.set(7);
        state.scroll(isize::MAX / 2);
        assert_eq!(state.scroll, 7);
        state.scroll(isize::MIN / 2);
        assert_eq!(state.scroll, 0);
    }
}

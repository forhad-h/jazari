//! The dashboard's prompt and skill writer, opened from the `c` picker: a rough idea in, a reviewed
//! prompt or skill in the user's library out. The idea goes to `claude -p` on a thread; the review shows the draft, what
//! it assumed and what it left open, and the draft can be edited, revised with feedback or renamed
//! before it is saved. Editing an existing prompt opens on the box that asks Claude for a change. The
//! picker routes keys here while it is open.

use crate::prompt_gen::{Draft, DraftFile, Request};
use crate::rules::{Kind, Rule};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::Arc;
use std::time::Instant;

type GenerateFn = dyn Fn(&Request, &AtomicBool) -> Result<Draft, String> + Send + Sync;
type FilesFn = dyn Fn(&str) -> Vec<crate::skills::File> + Send + Sync;
/// (key, draft, the user's entry it replaces, the built-in one it copies).
type SaveFn = dyn Fn(&str, &Draft, Option<&str>, Option<&str>) -> Result<PathBuf, String> + Send + Sync;

/// What the writer runs: the real thing in the dashboard, fakes in tests.
#[derive(Clone)]
pub struct Env {
    /// Runs on a thread; stops early once the flag is set.
    pub generate: Arc<GenerateFn>,
    /// Saves the draft and returns the path of its file or folder.
    pub save: Arc<SaveFn>,
    pub library: Arc<dyn Fn() -> Vec<Rule> + Send + Sync>,
    /// A skill's files in the library, by key, for a revision.
    pub files: Arc<FilesFn>,
    /// The model named while generating.
    pub model: String,
    /// What is written: a prompt or a skill.
    pub kind: Kind,
}

impl Env {
    pub fn real(kind: Kind) -> Self {
        Env {
            generate: Arc::new(crate::prompt_gen::generate),
            save: Arc::new(move |key, draft, replacing, copy_of| {
                match kind {
                    Kind::Prompt => crate::rules::save_own(key, &draft.body, replacing),
                    Kind::Skill => crate::skills::save_draft(key, draft, replacing, copy_of),
                }
                .map_err(|e| format!("{e:#}"))
            }),
            library: Arc::new(crate::rules::library),
            files: Arc::new(|key| crate::skills::find(key).map(|s| s.files).unwrap_or_default()),
            model: crate::settings::generator_model(),
            kind,
        }
    }
}

/// What a key did to the writer.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    /// Discarded or cancelled.
    Close,
    /// Saved under `key`; `replaced` is the user's prompt it replaced (an edit or a rename).
    Saved { key: String, replaced: Option<String> },
    /// Suspend the dashboard and open this text in the user's editor.
    Edit(String),
}

/// A text field: the characters and the cursor's position among them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextBox {
    pub chars: Vec<char>,
    pub cursor: usize,
}

impl TextBox {
    pub(crate) fn of(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        TextBox { cursor: chars.len(), chars }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Handles an editing key; false when the key isn't one.
    pub(crate) fn on_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            // Ctrl+Alt together is AltGr on Windows, which types characters such as `@` and `{`.
            KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(c)
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.chars.len(),
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        true
    }
}

/// The idea screen's fields and buttons, in `tab` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Idea,
    Name,
    /// The "Write prompt" button.
    Submit,
    /// The "Cancel" button.
    Cancel,
}

impl Field {
    const ORDER: [Field; 4] = [Field::Idea, Field::Name, Field::Submit, Field::Cancel];

    /// The next field, or the previous one when `back`, wrapping around.
    fn step(self, back: bool) -> Field {
        let i = Field::ORDER.iter().position(|f| *f == self).unwrap_or(0);
        Field::ORDER[(i + if back { 3 } else { 1 }) % 4]
    }
}

/// What the input on the review screen is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineFor {
    /// Feedback for a revision.
    Feedback,
    /// An answer to the question at this index, sent as feedback.
    Answer(usize),
    /// A new key.
    Key,
}

impl LineFor {
    /// Feedback and answers go to Claude and may span lines; a key is one line.
    pub fn multiline(self) -> bool {
        self != LineFor::Key
    }
}

/// Where the editor's text goes when it comes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditTarget {
    Idea,
    Draft,
    /// The open feedback or answer box.
    Request,
}

enum Phase {
    Idea,
    Generating { rx: Receiver<Result<Draft, String>>, cancel: Arc<AtomicBool>, since: Instant },
    Review,
    Line(LineFor, TextBox),
}

/// What the screen shows, for drawing.
pub enum Screen<'a> {
    Idea { idea: &'a TextBox, name: &'a TextBox, field: Field },
    Generating { model: &'a str, secs: u64, revising: bool },
    Review { draft: &'a Draft, key: &'a str, line: Option<(LineFor, &'a TextBox)> },
}

pub struct PromptView {
    env: Env,
    phase: Phase,
    idea: TextBox,
    name: TextBox,
    field: Field,
    draft: Option<Draft>,
    key: String,
    /// The user's prompt being revised, which the save replaces.
    replacing: Option<String>,
    /// The built-in prompt this is a copy of.
    copy_of: Option<String>,
    /// The skill's files in the library before this draft, which the draft changes.
    base: Vec<crate::skills::File>,
    editing: EditTarget,
    /// The draft has changes that aren't saved: a new or revised draft, an edit or a rename.
    dirty: bool,
    /// `esc` was pressed once on an unsaved draft; a second one discards it.
    discarding: bool,
    pub warning: Option<String>,
    pub notice: Option<String>,
    pub scroll: u16,
    /// The furthest the draft can scroll, set while drawing.
    pub max_scroll: Cell<u16>,
}

impl PromptView {
    /// A new prompt, starting from the idea screen.
    pub fn new(env: Env) -> Self {
        PromptView {
            env,
            phase: Phase::Idea,
            idea: TextBox::default(),
            name: TextBox::default(),
            field: Field::Idea,
            draft: None,
            key: String::new(),
            replacing: None,
            copy_of: None,
            base: Vec::new(),
            editing: EditTarget::Idea,
            dirty: false,
            discarding: false,
            warning: None,
            notice: None,
            scroll: 0,
            max_scroll: Cell::new(0),
        }
    }

    /// Revising `rule`: the user's own is saved back in place; a built-in one is saved as a copy.
    pub fn revise(env: Env, rule: &Rule) -> Self {
        let mut view = PromptView::new(env);
        view.idea = TextBox::of(rule.body());
        view.draft = Some(Draft::of(rule));
        if rule.kind == Kind::Skill {
            view.base = (view.env.files)(&rule.key);
        }
        if rule.own {
            (view.key, view.replacing) = (rule.key.clone(), Some(rule.key.clone()));
        } else {
            view.key = crate::rules::suggest_key(&(view.env.library)(), &format!("{}-copy", rule.key), &rule.title);
            view.copy_of = Some(rule.key.clone());
            view.notice = Some(format!("{} is built in, so this saves a copy to your library as {}.", rule.title, view.key));
        }
        view.phase = Phase::Line(LineFor::Feedback, TextBox::default());
        view
    }

    /// The model that writes and revises drafts.
    pub fn model(&self) -> &str {
        &self.env.model
    }

    pub fn kind(&self) -> Kind {
        self.env.kind
    }

    pub fn title(&self) -> String {
        let verb = match (&self.phase, &self.replacing, &self.copy_of) {
            (Phase::Idea | Phase::Generating { .. }, None, None) if self.draft.is_none() => "New",
            (_, Some(_), _) => "Edit",
            (_, _, Some(_)) => "Copy",
            _ => "Review",
        };
        format!(" {verb} {} ", self.env.kind.noun())
    }

    /// Each file a skill draft changes from the library copy, with what happens to it: `new`,
    /// `changed` or `removed`. Empty for a prompt.
    pub fn changed_files(&self) -> Vec<(String, &'static str)> {
        let Some(draft) = self.draft.as_ref().filter(|_| self.env.kind == Kind::Skill) else { return Vec::new() };
        let old = |path: &str| self.base.iter().find(|f| f.path == path);
        let mut out = Vec::new();
        match old(crate::skills::SKILL_MD).and_then(crate::skills::File::text).map(str::trim) {
            None => out.push((crate::skills::SKILL_MD.to_string(), "new")),
            Some(text) if text != draft.body.trim() => out.push((crate::skills::SKILL_MD.to_string(), "changed")),
            Some(_) => {}
        }
        for f in &draft.files {
            match old(&f.path) {
                None => out.push((f.path.clone(), "new")),
                Some(o) if o.bytes != f.content.as_bytes() => out.push((f.path.clone(), "changed")),
                Some(_) => {}
            }
        }
        out.extend(draft.removed.iter().filter(|p| old(p).is_some()).map(|p| (p.clone(), "removed")));
        out
    }

    pub fn screen(&self) -> Screen<'_> {
        match (&self.phase, &self.draft) {
            (Phase::Generating { since, .. }, _) => {
                Screen::Generating { model: &self.env.model, secs: since.elapsed().as_secs(), revising: self.draft.is_some() }
            }
            (Phase::Review, Some(draft)) => Screen::Review { draft, key: &self.key, line: None },
            (Phase::Line(purpose, text), Some(draft)) => Screen::Review { draft, key: &self.key, line: Some((*purpose, text)) },
            _ => Screen::Idea { idea: &self.idea, name: &self.name, field: self.field },
        }
    }

    /// Footer key hints for the current screen.
    pub fn keys(&self) -> Vec<(&'static str, &'static str)> {
        match &self.phase {
            Phase::Idea => vec![("enter", "write it"), ("alt+enter", "new line"), ("tab", "next field"), ("ctrl+e", "editor"), ("esc", "cancel")],
            Phase::Generating { .. } => vec![("esc", "cancel")],
            Phase::Review => {
                let mut keys = vec![("enter", "save"), ("e", "edit"), ("r", "ask Claude"), ("n", "rename"), ("↑↓", "scroll")];
                if self.draft.as_ref().is_some_and(|d| !d.questions.is_empty()) {
                    keys.insert(3, ("1-9", "answer"));
                }
                keys.push(("esc", if self.discarding { "discard now" } else { "discard" }));
                keys
            }
            Phase::Line(LineFor::Key, _) => vec![("enter", "rename"), ("esc", "back")],
            Phase::Line(..) => vec![("enter", "revise with Claude"), ("alt+enter", "new line"), ("ctrl+e", "editor"), ("esc", "back to the draft")],
        }
    }

    /// Takes a finished generation.
    pub fn tick(&mut self) {
        let Phase::Generating { rx, .. } = &self.phase else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the writer stopped without an answer".into()),
        };
        match result {
            Ok(draft) => {
                // A revision returns only the files it changed, on top of the draft before it.
                let draft = match &self.draft {
                    Some(earlier) => draft.on_top_of(earlier),
                    None => draft,
                };
                // The first draft names itself; a revision keeps the name already shown.
                if self.draft.is_none() {
                    let name = self.name.text();
                    let preferred = if name.trim().is_empty() { draft.key.clone() } else { name };
                    self.key = crate::rules::suggest_key(&(self.env.library)(), &preferred, &draft.title);
                }
                (self.draft, self.scroll, self.notice, self.dirty) = (Some(draft), 0, None, true);
                self.phase = Phase::Review;
            }
            Err(why) => {
                self.warning = Some(format!("Couldn't write the {}: {why}", self.env.kind.noun()));
                self.back();
            }
        }
    }

    /// Back to the review when there is a draft, else to the idea.
    fn back(&mut self) {
        self.phase = if self.draft.is_some() { Phase::Review } else { Phase::Idea };
    }

    fn start(&mut self, req: Request) {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (flag, generate) = (cancel.clone(), self.env.generate.clone());
        std::thread::spawn(move || {
            let _ = tx.send(generate(&req, &flag));
        });
        (self.warning, self.notice) = (None, None);
        self.phase = Phase::Generating { rx, cancel, since: Instant::now() };
    }

    /// The request for a first draft, or for a revision of the current one with `feedback`.
    fn request(&self, feedback: Option<String>) -> Request {
        let mut req = Request {
            idea: self.idea.text(),
            name: self.name.text().trim().to_string(),
            previous: feedback.as_ref().and(self.draft.as_ref()).map(|d| d.body.clone()),
            feedback: feedback.unwrap_or_default(),
            kind: self.env.kind,
            ..Request::default()
        };
        if self.env.kind == Kind::Skill && req.previous.is_some() {
            // Claude sees the skill's files as the draft has them now.
            let (sent, named) = crate::skills::for_revision(&self.base);
            let draft = self.draft.as_ref();
            let mut files: Vec<DraftFile> = sent
                .iter()
                .filter(|f| !draft.is_some_and(|d| d.removed.contains(&f.path) || d.files.iter().any(|n| n.path == f.path)))
                .map(|f| DraftFile { path: f.path.clone(), content: f.text().unwrap_or_default().to_string() })
                .collect();
            files.extend(draft.map(|d| d.files.clone()).unwrap_or_default());
            files.sort_by(|a, b| a.path.cmp(&b.path));
            req.files = files;
            req.other_files = named.into_iter().filter(|p| !draft.is_some_and(|d| d.removed.contains(p))).collect();
        }
        req
    }

    fn save(&mut self) -> Outcome {
        let Some(draft) = &self.draft else { return Outcome::Stay };
        if let Some(why) = crate::rules::key_problem(&(self.env.library)(), &self.key, self.replacing.as_deref()) {
            self.warning = Some(format!("Can't save as {}: {why}. Press n to rename it.", self.key));
            return Outcome::Stay;
        }
        match (self.env.save)(&self.key, draft, self.replacing.as_deref(), self.copy_of.as_deref()) {
            Ok(_) => Outcome::Saved { key: self.key.clone(), replaced: self.replacing.clone() },
            Err(why) => {
                self.warning = Some(format!("Couldn't save: {why}"));
                Outcome::Stay
            }
        }
    }

    /// Takes the text back from the user's editor: the idea, the draft's body, or the request to Claude.
    pub fn edited(&mut self, edited: Result<String, String>) {
        self.notice = None;
        match edited {
            Err(why) => self.warning = Some(format!("Couldn't open the editor: {why}")),
            Ok(text) if text.trim().is_empty() => self.warning = Some("The edited text was empty, so it was not used.".into()),
            Ok(text) if self.editing == EditTarget::Request => {
                if let Phase::Line(_, input) = &mut self.phase {
                    *input = TextBox::of(text.trim_end());
                }
                self.warning = None;
            }
            Ok(text) if self.editing == EditTarget::Draft => {
                if let Some(d) = self.draft.take() {
                    self.draft = Some(Draft { body: text, ..d }.normalized_keeping_key());
                    self.dirty = true;
                }
                self.warning = None;
            }
            Ok(text) => {
                self.idea = TextBox::of(text.trim_end());
                self.warning = None;
            }
        }
    }

    /// Inserts pasted text; the idea and the request to Claude keep their line breaks.
    pub fn on_paste(&mut self, pasted: &str) {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let (target, multiline) = match &mut self.phase {
            Phase::Idea if self.field == Field::Name => (&mut self.name, false),
            Phase::Idea => {
                // Pasting on a button still means the idea.
                self.field = Field::Idea;
                (&mut self.idea, true)
            }
            Phase::Line(purpose, text) => (text, purpose.multiline()),
            _ => return,
        };
        for c in pasted.chars() {
            target.insert(if c == '\n' && !multiline { ' ' } else { c });
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        let code = key.code;
        match &mut self.phase {
            Phase::Idea => match code {
                KeyCode::Esc => return Outcome::Close,
                KeyCode::Tab | KeyCode::Down => self.field = self.field.step(false),
                KeyCode::BackTab | KeyCode::Up => self.field = self.field.step(true),
                KeyCode::Left | KeyCode::Right if matches!(self.field, Field::Submit | Field::Cancel) => {
                    self.field = if self.field == Field::Submit { Field::Cancel } else { Field::Submit };
                }
                KeyCode::Enter if self.field == Field::Cancel => return Outcome::Close,
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.editing = EditTarget::Idea;
                    return Outcome::Edit(self.idea.text());
                }
                KeyCode::Enter if self.field == Field::Idea && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
                    self.idea.insert('\n');
                }
                KeyCode::Enter if self.idea.text().trim().is_empty() => self.warning = Some("Write the rough idea first.".into()),
                KeyCode::Enter => self.start(self.request(None)),
                _ => {
                    let field = match self.field {
                        Field::Idea => &mut self.idea,
                        Field::Name => &mut self.name,
                        // A button takes no text.
                        Field::Submit | Field::Cancel => return Outcome::Stay,
                    };
                    if field.on_key(key) {
                        self.warning = None;
                    }
                }
            },
            Phase::Generating { cancel, .. } => {
                if code == KeyCode::Esc {
                    cancel.store(true, Ordering::SeqCst);
                    self.back();
                    self.notice = Some("Cancelled.".into());
                }
            }
            Phase::Review => {
                let questions = self.draft.as_ref().map_or(0, |d| d.questions.len());
                (self.warning, self.notice) = (None, None);
                let confirmed = std::mem::take(&mut self.discarding);
                match code {
                    KeyCode::Esc if confirmed || !self.dirty => return Outcome::Close,
                    KeyCode::Esc => {
                        self.discarding = true;
                        self.warning = Some("This draft isn't saved. Esc again discards it; enter saves it; any other key keeps it open.".into());
                    }
                    KeyCode::Enter => return self.save(),
                    KeyCode::Char('e') => {
                        self.editing = EditTarget::Draft;
                        return Outcome::Edit(self.draft.as_ref().map(|d| d.body.clone()).unwrap_or_default());
                    }
                    KeyCode::Char('r') => self.phase = Phase::Line(LineFor::Feedback, TextBox::default()),
                    KeyCode::Char('n') => self.phase = Phase::Line(LineFor::Key, TextBox::of(&self.key)),
                    KeyCode::Char(d @ '1'..='9') if (d as usize - '1' as usize) < questions => {
                        self.phase = Phase::Line(LineFor::Answer(d as usize - '1' as usize), TextBox::default());
                    }
                    KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
                    KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
                    KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(10),
                    KeyCode::PageUp => self.scroll_by(-10),
                    _ => {}
                }
            }
            Phase::Line(purpose, text) => match code {
                KeyCode::Esc => (self.phase, self.warning) = (Phase::Review, None),
                KeyCode::Enter if purpose.multiline() && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => text.insert('\n'),
                KeyCode::Char('e') if purpose.multiline() && key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.editing = EditTarget::Request;
                    return Outcome::Edit(text.text());
                }
                KeyCode::Enter => {
                    let (purpose, input) = (*purpose, text.text().trim().to_string());
                    self.submit_line(purpose, input);
                }
                _ => {
                    text.on_key(key);
                }
            },
        }
        Outcome::Stay
    }

    fn submit_line(&mut self, purpose: LineFor, input: String) {
        if input.is_empty() {
            self.phase = Phase::Review;
            return;
        }
        match purpose {
            LineFor::Key => match crate::rules::key_problem(&(self.env.library)(), &input, self.replacing.as_deref()) {
                Some(why) => self.warning = Some(format!("Can't use {input:?}: {why}.")),
                None => {
                    self.dirty |= input != self.key;
                    (self.key, self.warning) = (input, None);
                    self.phase = Phase::Review;
                }
            },
            LineFor::Feedback => self.start(self.request(Some(input))),
            LineFor::Answer(i) => {
                let question = self.draft.as_ref().and_then(|d| d.questions.get(i)).cloned().unwrap_or_default();
                self.start(self.request(Some(format!("Question: {question}\nAnswer: {input}"))));
            }
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).clamp(0, self.max_scroll.get() as i32) as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(v: &mut PromptView, text: &str) {
        for c in text.chars() {
            assert_eq!(v.on_key(key(KeyCode::Char(c))), Outcome::Stay);
        }
    }

    fn draft(title: &str, questions: &[&str]) -> Draft {
        Draft {
            key: crate::rules::slug(title),
            title: title.into(),
            summary: "Keeps pull requests short.".into(),
            body: format!("# {title}\n\nKeeps pull requests short.\n\n- Under 150 words.\n"),
            assumptions: vec!["Descriptions only.".into()],
            questions: questions.iter().map(|q| q.to_string()).collect(),
            ..Draft::default()
        }
    }

    fn own(key: &str) -> Rule {
        // Tests run at the same time and several ask for the same key, so each call gets its own
        // folder. A shared folder can be deleted by one test while another still reads it.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jazari-pv-own-{key}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{key}.md")), format!("# {key}\n\nMine.\n")).unwrap();
        let rule = crate::rules::library_in(&dir).into_iter().find(|r| r.key == key).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        rule
    }

    /// A writer whose generator answers from `answers` in turn, recording each request, and whose
    /// save records (key, body, replacing).
    #[allow(clippy::type_complexity)]
    fn fake(answers: Vec<Result<Draft, String>>, lib: Vec<Rule>) -> (Env, Arc<Mutex<Vec<Request>>>, Arc<Mutex<Vec<(String, String, Option<String>)>>>) {
        let (asked, saved) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
        let answers = Arc::new(Mutex::new(answers));
        let (a, s) = (asked.clone(), saved.clone());
        let env = Env {
            generate: Arc::new(move |req, _| {
                a.lock().unwrap().push(req.clone());
                answers.lock().unwrap().remove(0)
            }),
            save: Arc::new(move |k, d, r, _| {
                s.lock().unwrap().push((k.into(), d.body.clone(), r.map(String::from)));
                Ok(PathBuf::from(format!("/p/{k}.md")))
            }),
            library: Arc::new(move || lib.clone()),
            files: Arc::new(|_| Vec::new()),
            model: "fable".into(),
            kind: Kind::Prompt,
        };
        (env, asked, saved)
    }

    fn wait(v: &mut PromptView) {
        let start = Instant::now();
        while matches!(v.phase, Phase::Generating { .. }) {
            assert!(start.elapsed() < Duration::from_secs(5), "generation never finished");
            std::thread::sleep(Duration::from_millis(5));
            v.tick();
        }
    }

    #[test]
    fn a_new_prompt_goes_from_idea_to_review_to_saved() {
        let first = draft("Short pull requests", &["Commits too?"]);
        let second = draft("Short pull requests and commits", &[]);
        let (env, asked, saved) = fake(vec![Ok(first), Ok(second.clone())], crate::rules::library_in(Path::new("/nonexistent")));
        let mut v = PromptView::new(env);
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Stay);
        assert_eq!(v.warning.as_deref(), Some("Write the rough idea first."), "an empty idea isn't sent");
        typed(&mut v, "stop over-explaining");
        v.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        typed(&mut v, "in PRs");
        assert_eq!(v.idea.text(), "stop over-explaining\nin PRs", "alt+enter adds a line, q is just a letter");
        v.on_key(key(KeyCode::Enter));
        assert!(matches!(v.screen(), Screen::Generating { model: "fable", revising: false, .. }));
        wait(&mut v);
        assert_eq!(asked.lock().unwrap()[0], Request { idea: "stop over-explaining\nin PRs".into(), ..Request::default() });
        assert_eq!(v.key, "short-pull-requests");
        assert!(v.keys().contains(&("1-9", "answer")));

        // Answering the question revises the draft with the answer as feedback.
        v.on_key(key(KeyCode::Char('1')));
        typed(&mut v, "yes");
        v.on_key(key(KeyCode::Enter));
        assert!(matches!(v.screen(), Screen::Generating { revising: true, .. }));
        wait(&mut v);
        let revision = asked.lock().unwrap()[1].clone();
        assert_eq!(revision.feedback, "Question: Commits too?\nAnswer: yes");
        assert!(revision.previous.unwrap().starts_with("# Short pull requests\n"));
        assert_eq!(v.key, "short-pull-requests", "a revision keeps the name");

        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "short-pull-requests".into(), replaced: None });
        assert_eq!(saved.lock().unwrap()[0], ("short-pull-requests".into(), second.body, None));
    }

    #[test]
    fn esc_asks_before_throwing_away_an_unsaved_draft() {
        let (env, _, saved) = fake(vec![Ok(draft("Short pull requests", &[]))], Vec::new());
        let mut v = PromptView::new(env.clone());
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        assert_eq!(v.on_key(key(KeyCode::Esc)), Outcome::Stay, "the first esc only warns");
        assert!(v.warning.as_deref().unwrap().contains("isn't saved"));
        v.on_key(key(KeyCode::Down));
        assert_eq!(v.on_key(key(KeyCode::Esc)), Outcome::Stay, "another key in between keeps it");
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "short-pull-requests".into(), replaced: None }, "enter still saves");
        assert_eq!(saved.lock().unwrap().len(), 1);

        let (env, ..) = fake(vec![Ok(draft("Short pull requests", &[]))], Vec::new());
        let mut v = PromptView::new(env);
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        v.on_key(key(KeyCode::Esc));
        assert_eq!(v.on_key(key(KeyCode::Esc)), Outcome::Close, "a second esc discards");

        let mut v = PromptView::revise(env_with_lib(vec![own("tone")]), &own("tone"));
        assert_eq!(v.on_key(key(KeyCode::Esc)), Outcome::Stay, "esc leaves the request box for the draft");
        assert_eq!(v.on_key(key(KeyCode::Esc)), Outcome::Close, "an unchanged prompt closes at once");
    }

    fn env_with_lib(lib: Vec<Rule>) -> Env {
        fake(Vec::new(), lib).0
    }

    #[test]
    fn a_failed_or_cancelled_run_goes_back_with_the_reason() {
        let (env, ..) = fake(vec![Err("claude returned an error: quota".into())], Vec::new());
        let mut v = PromptView::new(env);
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        assert!(matches!(v.screen(), Screen::Idea { .. }));
        assert_eq!(v.warning.as_deref(), Some("Couldn't write the prompt: claude returned an error: quota"));
        assert_eq!(v.idea.text(), "idea", "the idea is kept");

        let (slow, ..) = fake(Vec::new(), Vec::new());
        let flag = Arc::new(Mutex::new(None::<bool>));
        let seen = flag.clone();
        let slow = Env {
            generate: Arc::new(move |_, cancel: &AtomicBool| {
                while !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                *seen.lock().unwrap() = Some(true);
                Err("cancelled".into())
            }),
            ..slow
        };
        let mut v = PromptView::new(slow);
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::Enter));
        assert_eq!(v.keys(), [("esc", "cancel")]);
        v.on_key(key(KeyCode::Esc));
        assert!(matches!(v.screen(), Screen::Idea { .. }));
        let start = Instant::now();
        while flag.lock().unwrap().is_none() {
            assert!(start.elapsed() < Duration::from_secs(5), "the run was never told to stop");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_name_that_clashes_is_refused_until_renamed() {
        let lib = vec![own("short-pull-requests")];
        let (env, _, saved) = fake(vec![Ok(draft("Short pull requests", &[]))], lib);
        let mut v = PromptView::new(env);
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        assert_eq!(v.key, "short-pull-requests-2", "the suggestion skips a taken name");
        v.on_key(key(KeyCode::Char('n')));
        for _ in 0..2 {
            v.on_key(key(KeyCode::Backspace));
        }
        v.on_key(key(KeyCode::Enter));
        assert!(v.warning.as_deref().unwrap().contains("already have a prompt named short-pull-requests"));
        for k in [KeyCode::Char('-'), KeyCode::Char('x'), KeyCode::Enter] {
            v.on_key(key(k));
        }
        assert_eq!(v.key, "short-pull-requests-x");
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "short-pull-requests-x".into(), replaced: None });
        assert_eq!(saved.lock().unwrap().len(), 1);
    }

    #[test]
    fn editing_your_own_replaces_it_and_a_builtin_saves_a_copy() {
        let mine = own("tone");
        let lib = [crate::rules::library_in(Path::new("/nonexistent")), vec![mine.clone()]].concat();
        let (e, _, saved) = fake(Vec::new(), lib.clone());
        let mut v = PromptView::revise(e, &mine);
        assert_eq!(v.title(), " Edit prompt ");
        v.on_key(key(KeyCode::Esc));
        assert_eq!(v.on_key(key(KeyCode::Char('e'))), Outcome::Edit("# tone\n\nMine.".into()));
        v.edited(Ok("# Tone\n\nWarmer.\n".into()));
        assert_eq!(v.draft.as_ref().unwrap().summary, "Warmer.", "the summary follows the edit");
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "tone".into(), replaced: Some("tone".into()) }, "keeping its own name is fine");
        assert_eq!(saved.lock().unwrap()[0].2.as_deref(), Some("tone"));

        let (e, _, saved) = fake(Vec::new(), lib.clone());
        let mut v = PromptView::revise(e, &lib[1]);
        assert_eq!((v.title().as_str(), v.key.as_str()), (" Copy prompt ", "output-hygiene-copy"));
        assert!(v.notice.as_deref().unwrap().contains("saves a copy"));
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Stay, "an empty request goes back to the draft");
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "output-hygiene-copy".into(), replaced: None });
        assert_eq!(saved.lock().unwrap()[0].2, None, "a copy replaces nothing");
    }

    #[test]
    fn q_is_a_letter_and_altgr_types() {
        let (env, ..) = fake(vec![Ok(draft("Short pull requests", &[]))], Vec::new());
        let mut v = PromptView::new(env);
        typed(&mut v, "q");
        v.on_key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        v.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(v.idea.text(), "q@", "AltGr types, Ctrl alone doesn't");
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        assert_eq!(v.on_key(key(KeyCode::Char('q'))), Outcome::Stay, "q doesn't throw a draft away");
        assert!(matches!(v.screen(), Screen::Review { .. }));
    }

    #[test]
    fn editing_a_builtin_skill_sends_its_files_and_saves_a_copy() {
        let skills = crate::skills::library_in(Path::new("/nonexistent"));
        let lib = [crate::rules::builtin_rules(), crate::skills::rules(&skills)].concat();
        let rule = lib.iter().find(|r| r.key == "easy-english").unwrap().clone();
        let revised = Draft {
            key: "easy-english".into(),
            body: rule.body().replace("# Easy English", "# Easier English"),
            files: vec![DraftFile { path: "data/extra.txt".into(), content: "x".into() }],
            kind: Kind::Skill,
            ..Draft::default()
        };
        let (env, asked, _) = fake(vec![Ok(revised)], lib.clone());
        let copies = Arc::new(Mutex::new(Vec::new()));
        let c = copies.clone();
        let files = skills[0].files.clone();
        let env = Env {
            kind: Kind::Skill,
            files: Arc::new(move |_| files.clone()),
            save: Arc::new(move |k, d, r, copy| {
                c.lock().unwrap().push((k.to_string(), d.files.len(), r.map(String::from), copy.map(String::from)));
                Ok(PathBuf::from(k))
            }),
            ..env
        };
        let mut v = PromptView::revise(env, &rule);
        assert_eq!(v.title(), " Copy skill ");
        assert!(v.changed_files().is_empty(), "nothing changed yet");
        typed(&mut v, "shorter");
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        let req = asked.lock().unwrap()[0].clone();
        assert_eq!(req.kind, Kind::Skill);
        assert!(req.files.iter().any(|f| f.path == "scripts/check.py"), "Claude sees the script");
        assert!(req.other_files.contains(&"data/ngsl-forms.csv".to_string()), "and is told of the word lists");
        assert_eq!(v.changed_files(), [("SKILL.md".to_string(), "changed"), ("data/extra.txt".to_string(), "new")]);
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "easy-english-copy".into(), replaced: None });
        assert_eq!(copies.lock().unwrap()[0], ("easy-english-copy".into(), 1, None, Some("easy-english".into())));
    }

    #[test]
    fn tab_reaches_the_buttons_and_enter_presses_them() {
        let (env, asked, _) = fake(vec![Ok(draft("Short pull requests", &[]))], Vec::new());
        let mut v = PromptView::new(env);
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::Tab));
        v.on_key(key(KeyCode::Tab));
        assert_eq!(v.field, Field::Submit);
        typed(&mut v, "zz");
        assert_eq!((v.idea.text(), v.name.text()), ("idea".to_string(), String::new()), "a button takes no text");
        v.on_key(key(KeyCode::Right));
        assert_eq!(v.field, Field::Cancel);
        v.on_key(key(KeyCode::Tab));
        assert_eq!(v.field, Field::Idea, "tab wraps around");
        v.on_key(key(KeyCode::BackTab));
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Close, "enter on Cancel closes");

        let mut v = PromptView::new(v.env.clone());
        typed(&mut v, "idea");
        v.on_key(key(KeyCode::BackTab));
        v.on_key(key(KeyCode::BackTab));
        assert_eq!(v.field, Field::Submit);
        v.on_key(key(KeyCode::Enter));
        wait(&mut v);
        assert_eq!(asked.lock().unwrap()[0].idea, "idea", "enter on Write prompt sends the idea");
    }

    #[test]
    fn the_editor_fills_the_idea_and_paste_keeps_its_lines() {
        let (env, ..) = fake(Vec::new(), Vec::new());
        let mut v = PromptView::new(env);
        assert_eq!(v.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)), Outcome::Edit(String::new()));
        v.edited(Ok("line one\nline two\n".into()));
        assert_eq!(v.idea.text(), "line one\nline two");
        v.on_paste("\r\nthree");
        assert_eq!(v.idea.text(), "line one\nline two\nthree");
        v.on_key(key(KeyCode::Tab));
        v.on_paste("My\nname");
        assert_eq!(v.name.text(), "My name", "a name is one line");
        v.edited(Err("vi exited".into()));
        assert!(v.warning.as_deref().unwrap().contains("vi exited"));
        assert_eq!(v.on_key(key(KeyCode::Esc)), Outcome::Close);
    }

    #[test]
    fn editing_opens_on_a_box_that_asks_claude_for_the_change() {
        let mine = own("tone");
        let revised = Draft { key: "tone".into(), ..draft("Warmer tone", &[]) };
        let (env, asked, saved) = fake(vec![Ok(revised.clone())], vec![mine.clone()]);
        let mut v = PromptView::revise(env, &mine);
        assert!(matches!(v.screen(), Screen::Review { line: Some((LineFor::Feedback, _)), .. }), "the request box is open");
        assert_eq!(v.model(), "fable");
        typed(&mut v, "warmer, and");
        v.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        v.on_paste("cover\ncommits");
        assert_eq!(v.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)), Outcome::Edit("warmer, and\ncover\ncommits".into()));
        v.edited(Ok("be warmer\nand cover commits\n".into()));
        v.on_key(key(KeyCode::Enter));
        assert!(matches!(v.screen(), Screen::Generating { revising: true, .. }));
        wait(&mut v);
        let req = asked.lock().unwrap()[0].clone();
        assert_eq!(req.feedback, "be warmer\nand cover commits", "the box's lines go to Claude as they are");
        assert_eq!(req.previous.as_deref(), Some("# tone\n\nMine."));
        assert_eq!(v.on_key(key(KeyCode::Enter)), Outcome::Saved { key: "tone".into(), replaced: Some("tone".into()) });
        assert_eq!(saved.lock().unwrap()[0].1, revised.body);

        // A new name stays on one line.
        v.on_key(key(KeyCode::Char('n')));
        v.on_paste("a\nb");
        assert!(matches!(v.screen(), Screen::Review { line: Some((LineFor::Key, t)), .. } if t.text() == "tonea b"));
    }

    use std::path::Path;
}

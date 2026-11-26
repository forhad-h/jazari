//! The dashboard's answer box (`r`): a session's waiting permission prompt or question, answered
//! with a key. The answer goes to the session's hook through [`crate::answer::server`]; the hook
//! then asks for Touch ID or the Mac password before an allow or an answer applies.

use crate::answer::{Answer, Request, clean};
use ratatui::Frame;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use std::collections::BTreeMap;

/// The most lines of a command or change that the box shows.
const DETAIL_LINES: usize = 14;

pub struct AnswerView {
    pub session: String,
    pub name: String,
    pub req: Request,
    /// How many prompts of this session wait, this one included.
    pub waiting: usize,
    /// The question being answered.
    step: usize,
    chosen: BTreeMap<String, String>,
    /// The options marked so far, for a multi-select question.
    marked: Vec<bool>,
    /// Whether this machine can confirm that the user is present. Without it, only deny is offered.
    can_allow: bool,
}

/// What a key press did.
#[derive(Debug, PartialEq)]
pub enum Key {
    Stay,
    Close,
    Send(Answer),
}

impl AnswerView {
    pub fn open(session: &str, name: String, req: Request, waiting: usize, can_allow: bool) -> AnswerView {
        let marked = req.questions.first().map_or(Vec::new(), |q| vec![false; q.options.len()]);
        AnswerView { session: session.to_string(), name, req, waiting, step: 0, chosen: BTreeMap::new(), marked, can_allow }
    }

    fn is_question(&self) -> bool {
        !self.req.questions.is_empty()
    }

    /// Records the answer to the current question and moves on; the last one sends them all.
    fn choose(&mut self, label: String) -> Key {
        let q = &self.req.questions[self.step];
        self.chosen.insert(q.question.clone(), label);
        self.step += 1;
        match self.req.questions.get(self.step) {
            Some(next) => {
                self.marked = vec![false; next.options.len()];
                Key::Stay
            }
            None => Key::Send(Answer::Choose { answers: std::mem::take(&mut self.chosen) }),
        }
    }

    pub fn on_key(&mut self, code: KeyCode) -> Key {
        if code == KeyCode::Esc {
            return Key::Close;
        }
        if !self.is_question() {
            return match code {
                KeyCode::Char('y') if self.can_allow => Key::Send(Answer::Allow),
                KeyCode::Char('A') if self.can_allow && self.req.rule.is_some() => Key::Send(Answer::AlwaysAllow),
                KeyCode::Char('n') => Key::Send(Answer::Deny),
                _ => Key::Stay,
            };
        }
        if !self.can_allow {
            return Key::Stay;
        }
        let q = self.req.questions[self.step].clone();
        match code {
            KeyCode::Char(d @ '1'..='9') => {
                let i = d as usize - '1' as usize;
                let Some(label) = q.options.get(i) else { return Key::Stay };
                if q.multi {
                    self.marked[i] = !self.marked[i];
                    Key::Stay
                } else {
                    self.choose(label.clone())
                }
            }
            KeyCode::Enter if q.multi && self.marked.contains(&true) => {
                let labels: Vec<&str> = q.options.iter().zip(&self.marked).filter(|(_, m)| **m).map(|(o, _)| o.as_str()).collect();
                self.choose(labels.join(", "))
            }
            _ => Key::Stay,
        }
    }

    /// The footer's key hints.
    pub fn keys(&self) -> Vec<(&'static str, String)> {
        let leave = ("esc", "leave it to the terminal".to_string());
        if !self.is_question() {
            let mut keys = Vec::new();
            if self.can_allow {
                keys.push(("y", "allow".to_string()));
                if self.req.rule.is_some() {
                    keys.push(("A", "always allow".into()));
                }
            }
            keys.extend([("n", "deny".into()), leave]);
            return keys;
        }
        if !self.can_allow {
            return vec![leave];
        }
        let q = &self.req.questions[self.step];
        let mut keys = vec![(if q.options.len() > 1 { "1-9" } else { "1" }, if q.multi { "mark" } else { "choose" }.to_string())];
        if q.multi {
            keys.push(("enter", "send the marked".into()));
        }
        keys.push(leave);
        keys
    }

    pub fn draw(&self, f: &mut Frame, area: Rect, now: f64) {
        let muted = Style::new().fg(Color::DarkGray);
        let left = (self.req.until - now).max(0.0) as u64;
        let mut lines = Vec::new();
        if self.is_question() {
            let q = &self.req.questions[self.step];
            if self.req.questions.len() > 1 {
                lines.push(Line::from(Span::styled(format!("Question {} of {}", self.step + 1, self.req.questions.len()), muted)));
            }
            lines.push(Line::from(clean(&q.question).bold()));
            lines.push(Line::from(""));
            for (i, o) in q.options.iter().enumerate().take(9) {
                let mark = if q.multi { if self.marked[i] { "[x] " } else { "[ ] " } } else { "" };
                lines.push(Line::from(vec![Span::styled(format!(" {} ", i + 1), Style::new().fg(Color::Cyan).bold()), Span::raw(format!("{mark}{}", clean(o)))]));
            }
        } else {
            lines.push(Line::from(vec![Span::raw(format!("{}: ", clean(&self.req.tool))), Span::raw(clean(&self.req.summary)).bold()]));
            lines.push(Line::from(""));
            let detail: Vec<&str> = self.req.detail.lines().collect();
            for l in detail.iter().take(DETAIL_LINES) {
                let color = if l.starts_with("- ") { Color::Red } else if l.starts_with("+ ") { Color::Green } else { Color::Yellow };
                lines.push(Line::from(Span::styled(clean(l), Style::new().fg(color))));
            }
            if detail.len() > DETAIL_LINES {
                lines.push(Line::from(Span::styled(format!("… {} more lines", detail.len() - DETAIL_LINES), muted)));
            }
            if let Some(rule) = &self.req.rule {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![Span::styled("A adds the rule: ", muted), Span::raw(clean(rule))]));
            }
        }
        lines.push(Line::from(""));
        if self.can_allow {
            lines.push(Line::from(Span::styled("An allow or an answer then asks for Touch ID or your password.", muted)));
        } else {
            lines.push(Line::from(Span::styled("Allowing or answering needs Touch ID (macOS); here you can only deny.", muted)));
        }
        lines.push(Line::from(Span::styled(format!("The terminal prompt stays open too; the first answer wins. {left} s left."), muted)));
        let w = area.width.saturating_sub(8).min(110);
        let h = (lines.len() as u16 + 2).min(area.height);
        let [_, rect, _] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(w), Constraint::Fill(1)]).areas(area);
        let [_, rect, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(h), Constraint::Fill(1)]).areas(rect);
        f.render_widget(Clear, rect);
        let of = if self.waiting > 1 { format!(" (1 of {})", self.waiting) } else { String::new() };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Yellow))
            .title(Line::from(format!(" {} asks{of} ", clean(&self.name))).bold());
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::Question;

    fn req(questions: Vec<Question>, rule: Option<&str>) -> Request {
        Request {
            session: "s1".into(),
            project: "marsad".into(),
            tool: "Bash".into(),
            summary: "`mkdir out`".into(),
            detail: "mkdir out".into(),
            questions,
            rule: rule.map(String::from),
            opened: 0.0,
            until: 120.0,
        }
    }

    fn q(text: &str, options: &[&str], multi: bool) -> Question {
        Question { question: text.into(), options: options.iter().map(|o| o.to_string()).collect(), multi }
    }

    #[test]
    fn a_permission_prompt_is_allowed_denied_or_left_to_the_terminal() {
        let mut v = AnswerView::open("s1", "app".into(), req(vec![], None), 1, true);
        assert_eq!(v.on_key(KeyCode::Char('A')), Key::Stay, "no rule was offered");
        assert_eq!(v.on_key(KeyCode::Char('y')), Key::Send(Answer::Allow));
        assert_eq!(v.on_key(KeyCode::Char('n')), Key::Send(Answer::Deny));
        assert_eq!(v.on_key(KeyCode::Esc), Key::Close);
        let mut v = AnswerView::open("s1", "app".into(), req(vec![], Some("Bash(mkdir:*)")), 1, true);
        assert_eq!(v.on_key(KeyCode::Char('A')), Key::Send(Answer::AlwaysAllow));
        assert!(v.keys().iter().any(|(k, _)| *k == "A"));
    }

    #[test]
    fn questions_are_answered_one_after_another() {
        let mut v = AnswerView::open("s1", "app".into(), req(vec![q("Fruit?", &["Apple", "Pear"], false), q("Extras?", &["Nuts", "Honey", "Milk"], true)], None), 1, true);
        assert_eq!(v.on_key(KeyCode::Char('y')), Key::Stay);
        assert_eq!(v.on_key(KeyCode::Char('3')), Key::Stay, "no third option");
        assert_eq!(v.on_key(KeyCode::Char('2')), Key::Stay);
        assert_eq!(v.on_key(KeyCode::Enter), Key::Stay, "nothing marked yet");
        v.on_key(KeyCode::Char('1'));
        v.on_key(KeyCode::Char('3'));
        v.on_key(KeyCode::Char('1'));
        v.on_key(KeyCode::Char('1'));
        let Key::Send(Answer::Choose { answers }) = v.on_key(KeyCode::Enter) else { panic!("sends after the last question") };
        assert_eq!(answers["Fruit?"], "Pear");
        assert_eq!(answers["Extras?"], "Nuts, Milk");
        let sent = Answer::Choose { answers };
        assert!(crate::answer::check(&v.req, &sent).is_ok());
    }

    #[test]
    fn without_a_presence_check_only_deny_is_offered() {
        let mut v = AnswerView::open("s1", "app".into(), req(vec![], Some("Bash(mkdir:*)")), 1, false);
        assert_eq!(v.on_key(KeyCode::Char('y')), Key::Stay);
        assert_eq!(v.on_key(KeyCode::Char('A')), Key::Stay);
        assert_eq!(v.on_key(KeyCode::Char('n')), Key::Send(Answer::Deny));
        assert!(!v.keys().iter().any(|(k, _)| *k == "y"));
        let mut v = AnswerView::open("s1", "app".into(), req(vec![q("Fruit?", &["Apple", "Pear"], false)], None), 1, false);
        assert_eq!(v.on_key(KeyCode::Char('1')), Key::Stay, "an answer is an allow");
    }
}

//! The prompt and skill picker in the terminal, for `jazari build project edit` and
//! `jazari claude --pick`: the same ticks and keys as the dashboard's `c` picker, without its
//! editing keys.

use crate::rules::{ALWAYS, Kind, Rule};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

/// The choice the picker ends with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub keys: Vec<String>,
    /// Save the keys for the project, not only use them for this start.
    pub save: bool,
}

pub enum Outcome {
    Stay,
    Cancel,
    Done(Picked),
}

pub struct Picker {
    library: Vec<Rule>,
    ticked: Vec<bool>,
    cursor: usize,
    title: String,
    /// Ask at the end whether to save for the project; without it, every choice is saved.
    offer_save: bool,
    /// At the last step: whether "save for this project" is highlighted.
    step: Option<bool>,
}

impl Picker {
    pub fn new(library: Vec<Rule>, ticked: &[String], title: String, offer_save: bool) -> Picker {
        let library: Vec<Rule> = library.into_iter().filter(|r| r.key != ALWAYS).collect();
        let ticked = library.iter().map(|r| ticked.contains(&r.key)).collect();
        Picker { library, ticked, cursor: 0, title, offer_save, step: None }
    }

    fn keys(&self) -> Vec<String> {
        self.library.iter().zip(&self.ticked).filter(|(_, t)| **t).map(|(r, _)| r.key.clone()).collect()
    }

    fn toggle(&mut self, i: usize) {
        if let Some(t) = self.ticked.get_mut(i) {
            *t = !*t;
        }
    }

    pub fn on_key(&mut self, code: KeyCode) -> Outcome {
        if let Some(save) = self.step {
            match code {
                KeyCode::Esc => self.step = None,
                KeyCode::Char('q') => return Outcome::Cancel,
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') | KeyCode::Tab | KeyCode::BackTab => self.step = Some(!save),
                KeyCode::Char('1') => self.step = Some(false),
                KeyCode::Char('2') => self.step = Some(true),
                KeyCode::Char('s') => return Outcome::Done(Picked { keys: self.keys(), save: true }),
                KeyCode::Enter => return Outcome::Done(Picked { keys: self.keys(), save }),
                _ => {}
            }
            return Outcome::Stay;
        }
        let last = self.library.len().saturating_sub(1);
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Cancel,
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char(' ' | 'x') => self.toggle(self.cursor),
            KeyCode::Char(d @ '1'..='9') => self.toggle(d as usize - '1' as usize),
            KeyCode::Char('s') if self.offer_save => return Outcome::Done(Picked { keys: self.keys(), save: true }),
            KeyCode::Enter if self.offer_save => self.step = Some(false),
            KeyCode::Enter => return Outcome::Done(Picked { keys: self.keys(), save: true }),
            _ => {}
        }
        Outcome::Stay
    }

    pub fn draw(&self, f: &mut Frame, area: Rect) {
        let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));
        let mut lines = vec![Line::from(dim(format!("Every session starts with \"Working with jazari\" ({ALWAYS}). Tick more:"))), Line::from("")];
        let mut cursor_line = 0;
        let first_skill = self.library.iter().position(|r| r.kind == Kind::Skill);
        lines.push(Line::from(Span::styled("Prompts", Style::new().bold())));
        for (i, rule) in self.library.iter().enumerate() {
            if Some(i) == first_skill {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Skills", Style::new().bold())));
            }
            let digit = if i < 9 { format!("{}", i + 1) } else { " ".into() };
            let tick = if self.ticked[i] { "[x]" } else { "[ ]" };
            let mark = if i == self.cursor && self.step.is_none() { "› " } else { "  " };
            let mut line = Line::from(vec![Span::raw(mark), Span::styled(format!("{digit} {tick} "), Style::new().fg(Color::Cyan)), rule.title.clone().bold()]);
            if i == self.cursor && self.step.is_none() {
                line = line.patch_style(Style::new().add_modifier(Modifier::REVERSED));
                cursor_line = lines.len();
            }
            lines.push(line);
            lines.push(Line::from(vec![Span::raw("        "), dim(rule.summary.clone())]));
        }
        let footer: Vec<Line> = match self.step {
            Some(save) => {
                let choice = |on: bool, text: &str| {
                    let l = Line::from(format!("{} {text}", if on { "›" } else { " " }));
                    if on { l.patch_style(Style::new().add_modifier(Modifier::REVERSED)) } else { l }
                };
                vec![
                    choice(!save, "1 Use for this start only"),
                    choice(save, "2 Save for this project"),
                    Line::from(dim("enter chooses · s saves · esc back · q quit".into())),
                ]
            }
            None if self.offer_save => vec![Line::from(dim("space/1-9 tick · enter next · s save for this project and start · q quit".into()))],
            None => vec![Line::from(dim("space/1-9 tick · enter save · q quit without saving".into()))],
        };
        let [body, foot] = Layout::vertical([Constraint::Min(1), Constraint::Length(footer.len() as u16)]).areas(area);
        let height = body.height.saturating_sub(2) as usize;
        let scroll = (cursor_line + 2).saturating_sub(height) as u16;
        let block = Block::bordered().title(format!(" {} ", self.title));
        f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), body);
        f.render_widget(Paragraph::new(footer), foot);
    }
}

/// Runs the picker on the terminal; None when it was closed without a choice.
pub fn run(mut picker: Picker) -> anyhow::Result<Option<Picked>> {
    let mut terminal = ratatui::init();
    let result = (|| -> anyhow::Result<Option<Picked>> {
        loop {
            terminal.draw(|f| picker.draw(f, f.area()))?;
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match picker.on_key(k.code) {
                    Outcome::Stay => {}
                    Outcome::Cancel => return Ok(None),
                    Outcome::Done(p) => return Ok(Some(p)),
                }
            }
        }
    })();
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Vec<Rule> {
        let r = |k: &str, kind| Rule::new(k.into(), format!("{k} title"), format!("{k} summary"), "b".into(), false, kind);
        vec![r("jazari", Kind::Prompt), r("coordination", Kind::Prompt), r("output-hygiene", Kind::Prompt), r("easy-english", Kind::Skill)]
    }

    fn done(o: Outcome) -> Option<Picked> {
        match o {
            Outcome::Done(p) => Some(p),
            _ => None,
        }
    }

    #[test]
    fn ticks_and_saves_or_uses_once() {
        let mut p = Picker::new(lib(), &["coordination".into()], "t".into(), true);
        assert_eq!(p.library.len(), 3, "jazari is never a choice");
        p.on_key(KeyCode::Char('3'));
        p.on_key(KeyCode::Enter);
        assert_eq!(p.step, Some(false), "this start only is the default");
        let picked = done(p.on_key(KeyCode::Enter)).unwrap();
        assert_eq!(picked, Picked { keys: vec!["coordination".into(), "easy-english".into()], save: false });
        let mut p = Picker::new(lib(), &[], "t".into(), true);
        assert!(done(p.on_key(KeyCode::Char('s'))).unwrap().save, "s saves for the project");
        let mut p = Picker::new(lib(), &[], "t".into(), false);
        p.on_key(KeyCode::Char(' '));
        assert_eq!(done(p.on_key(KeyCode::Enter)).unwrap(), Picked { keys: vec!["coordination".into()], save: true });
        assert!(matches!(Picker::new(lib(), &[], "t".into(), false).on_key(KeyCode::Char('q')), Outcome::Cancel));
    }

    #[test]
    fn draws_at_wide_and_narrow_sizes() {
        for (w, h) in [(182, 37), (110, 33), (60, 12)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            let mut p = Picker::new(lib(), &["coordination".into()], "Prompts and skills for shop".into(), true);
            t.draw(|f| p.draw(f, f.area())).unwrap();
            let text: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(text.contains("[x] coordination title") && text.contains("Skills"), "{w}x{h}");
            p.on_key(KeyCode::Enter);
            t.draw(|f| p.draw(f, f.area())).unwrap();
            let text: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!(text.contains("Save for this project"), "{w}x{h}");
        }
    }
}

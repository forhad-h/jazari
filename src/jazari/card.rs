//! Every message jazari sends the founder has one shape, so it can be read at a glance:
//! - an icon and a short title;
//! - one line that says what happened or what is needed;
//! - the reason, the options with what each one changes, and the recommendation;
//! - how to reply.
//!
//! Options become buttons. The text follows the Easy English rules: short sentences, common
//! words, one idea each.

/// One option the founder can choose: its label, and what changes when they choose it.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    pub effect: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub icon: &'static str,
    pub title: String,
    /// What happened, or what is needed, in one or two short sentences.
    pub lead: String,
    pub why: Option<String>,
    pub choices: Vec<Choice>,
    /// The label of the option jazari recommends, and why.
    pub recommend: Option<(String, String)>,
    /// Extra lines, such as a summary or a link.
    pub details: Vec<String>,
    /// How to reply when there are no buttons, or more to say.
    pub reply: Option<String>,
    /// The task the card is about; buttons answer it.
    pub task: Option<i64>,
    /// Buttons with their own commands, instead of one per option.
    pub actions: Vec<(String, String)>,
}

/// Telegram's limit for a button's data.
const MAX_BUTTON_DATA: usize = 64;

impl Card {
    pub fn new(icon: &'static str, title: impl Into<String>, lead: impl Into<String>) -> Self {
        Card { icon, title: title.into(), lead: lead.into(), ..Default::default() }
    }

    pub fn why(mut self, why: impl Into<String>) -> Self {
        self.why = Some(why.into()).filter(|w: &String| !w.trim().is_empty());
        self
    }

    pub fn detail(mut self, line: impl Into<String>) -> Self {
        let line = line.into();
        if !line.trim().is_empty() {
            self.details.push(line);
        }
        self
    }

    pub fn reply(mut self, how: impl Into<String>) -> Self {
        self.reply = Some(how.into());
        self
    }

    pub fn task(mut self, id: i64) -> Self {
        self.task = Some(id);
        self
    }

    pub fn actions(mut self, actions: Vec<(String, String)>) -> Self {
        self.actions = actions;
        self
    }

    pub fn choices(mut self, choices: Vec<Choice>, recommend: Option<(String, String)>) -> Self {
        self.choices = choices;
        self.recommend = recommend;
        self
    }

    /// The text of the message.
    pub fn text(&self) -> String {
        let mut out = format!("{} {}\n{}", self.icon, self.title.trim(), self.lead.trim());
        if let Some(why) = &self.why {
            out += &format!("\n\nWhy: {}", why.trim());
        }
        if !self.choices.is_empty() {
            out += "\n\nOptions:";
            for (i, c) in self.choices.iter().enumerate() {
                let effect = if c.effect.trim().is_empty() { String::new() } else { format!(": {}", c.effect.trim()) };
                out += &format!("\n  {}. {}{effect}", i + 1, c.label.trim());
            }
        }
        if let Some((label, because)) = &self.recommend {
            let n = self.choices.iter().position(|c| c.label == *label).map(|i| format!("{}. ", i + 1)).unwrap_or_default();
            let because = if because.trim().is_empty() { String::new() } else { format!(" ({})", because.trim()) };
            out += &format!("\nRecommended: {n}{}{because}", label.trim());
        }
        for d in &self.details {
            out += &format!("\n{}", d.trim());
        }
        if let Some(r) = &self.reply {
            out += &format!("\n\nReply: {}", r.trim());
        }
        out
    }

    /// One button per option, each answering the card's task with that option.
    pub fn buttons(&self) -> Vec<(String, String)> {
        if !self.actions.is_empty() {
            return self.actions.clone();
        }
        let Some(task) = self.task else { return Vec::new() };
        self.choices
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut data = format!("/answer {task} {}", c.label.trim());
                if data.len() > MAX_BUTTON_DATA {
                    // The full label is in the text; the number is enough to answer.
                    data = format!("/answer {task} option {}", i + 1);
                }
                (format!("{}. {}", i + 1, crate::hook::truncate(&c.label, 30)), data)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_reads_top_down_and_each_option_is_a_button() {
        let card = Card::new("❓", "Login: which SMS provider?", "The login task needs your choice.")
            .why("Users get a one-time code by SMS.")
            .choices(
                vec![
                    Choice { label: "Twilio".into(), effect: "costs more, and is reliable worldwide".into() },
                    Choice { label: "MessageBird".into(), effect: "costs less, and is good in Europe".into() },
                ],
                Some(("Twilio".into(), "most users are in Asia".into())),
            )
            .reply("tap a button, or /answer 12 <text>")
            .task(12);
        assert_eq!(
            card.text(),
            "❓ Login: which SMS provider?\nThe login task needs your choice.\n\nWhy: Users get a one-time code by SMS.\n\nOptions:\n  1. Twilio: costs more, and is reliable worldwide\n  2. MessageBird: costs less, and is good in Europe\nRecommended: 1. Twilio (most users are in Asia)\n\nReply: tap a button, or /answer 12 <text>"
        );
        assert_eq!(card.buttons(), vec![("1. Twilio".into(), "/answer 12 Twilio".into()), ("2. MessageBird".into(), "/answer 12 MessageBird".into())]);
    }

    #[test]
    fn long_labels_answer_by_number_and_a_card_without_a_task_has_no_buttons() {
        let long = "Use the managed Postgres service from the cloud provider with automatic backups";
        let card = Card::new("❓", "Database", "Pick one.").choices(vec![Choice { label: long.into(), effect: String::new() }], None).task(7);
        assert_eq!(card.buttons()[0].1, "/answer 7 option 1");
        assert!(card.buttons()[0].1.len() <= MAX_BUTTON_DATA);
        assert!(Card::new("🚀", "Merged", "Login is on main.").buttons().is_empty());
        assert_eq!(Card::new("🚀", "Merged", "Login is on main.").detail(" ").text(), "🚀 Merged\nLogin is on main.");
    }
}

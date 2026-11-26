//! The Telegram bridge: commands from the founder's paired chat, and sessions' `--attention`
//! notes sent back to it. Messages from any other chat are dropped without a reply.

use super::telegram::{Bot, Transport};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// How long one `getUpdates` call may wait for a message.
const POLL_SECS: u64 = 25;
/// How long `pair` waits for the code.
const PAIR_SECS: f64 = 600.0;
/// Wrong codes accepted before pairing gives up.
const PAIR_TRIES: u32 = 5;

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Status,
    Pause(Option<String>),
    Resume,
    Tell { session: String, text: String },
    Stop(String),
    Idea { product: String, text: String },
    New { name: String, idea: String },
    Tasks,
    Answer { task: i64, text: String },
    Decide { task: i64, approve: bool },
    Feedback { product: Option<String>, text: String },
    Prefer { product: String, area: String, rule: String },
    Forget(i64),
    Prefs,
    Mode { product: String, mode: String },
    Proposal { id: i64, accept: bool },
    Unknown(String),
}

/// `/tell@MyBot abc12 run the tests` → `Tell { session: "abc12", text: "run the tests" }`.
pub fn parse(text: &str) -> Command {
    let text = text.trim();
    // A plain message, not a command, is feedback on the product the founder last talked about.
    if !text.starts_with('/') {
        return if text.is_empty() { Command::Unknown("Send /help for the commands.".into()) } else { Command::Feedback { product: None, text: text.into() } };
    }
    let (head, rest) = text.split_once(char::is_whitespace).map_or((text, ""), |(h, r)| (h, r.trim()));
    let name = head.trim_start_matches('/').split('@').next().unwrap_or("").to_ascii_lowercase();
    let arg = |s: &str| Some(s.to_string()).filter(|s| !s.is_empty());
    match name.as_str() {
        "start" | "help" => Command::Help,
        "status" => Command::Status,
        "pause" => Command::Pause(arg(rest)),
        "resume" => Command::Resume,
        "tell" => match rest.split_once(char::is_whitespace) {
            Some((s, t)) if !t.trim().is_empty() => Command::Tell { session: s.into(), text: t.trim().into() },
            _ => Command::Unknown("usage: /tell <session> <message>".into()),
        },
        "stop" if !rest.is_empty() => Command::Stop(rest.split_whitespace().next().unwrap_or_default().into()),
        "stop" => Command::Unknown("usage: /stop <session>".into()),
        "idea" => match rest.split_once(char::is_whitespace) {
            Some((p, t)) if !t.trim().is_empty() => Command::Idea { product: p.into(), text: t.trim().into() },
            _ => Command::Unknown("usage: /idea <product> <the idea>".into()),
        },
        "new" => match rest.split_once(char::is_whitespace) {
            Some((n, t)) if !t.trim().is_empty() => Command::New { name: n.into(), idea: t.trim().into() },
            _ => Command::Unknown("usage: /new <name> <the idea>".into()),
        },
        "tasks" => Command::Tasks,
        "answer" => match rest.split_once(char::is_whitespace).and_then(|(id, t)| Some((id.trim_start_matches('#').parse().ok()?, t.trim()))) {
            Some((task, t)) if !t.is_empty() => Command::Answer { task, text: t.into() },
            _ => Command::Unknown("usage: /answer <task> <your answer>".into()),
        },
        "approve" | "deny" => match rest.trim_start_matches('#').parse() {
            Ok(task) => Command::Decide { task, approve: name == "approve" },
            Err(_) => Command::Unknown(format!("usage: /{name} <task>")),
        },
        "feedback" => match rest.split_once(char::is_whitespace) {
            Some((p, t)) if !t.trim().is_empty() => Command::Feedback { product: Some(p.into()), text: t.trim().into() },
            _ => Command::Unknown("usage: /feedback <product|all> <what you'd like>".into()),
        },
        "prefer" => {
            let mut parts = rest.splitn(3, char::is_whitespace);
            match (parts.next(), parts.next(), parts.next().map(str::trim)) {
                (Some(p), Some(a), Some(r)) if !r.is_empty() => Command::Prefer { product: p.into(), area: a.to_ascii_lowercase(), rule: r.into() },
                _ => Command::Unknown("usage: /prefer <product|all> <design|copy|code|product|process|communication> <rule>".into()),
            }
        }
        "forget" => match rest.trim_start_matches('#').parse() {
            Ok(id) => Command::Forget(id),
            Err(_) => Command::Unknown("usage: /forget <preference id>".into()),
        },
        "prefs" => Command::Prefs,
        "mode" => match rest.split_whitespace().collect::<Vec<_>>().as_slice() {
            [p, m] => Command::Mode { product: p.to_string(), mode: m.to_ascii_lowercase() },
            _ => Command::Unknown("usage: /mode <product> normal|autonomous".into()),
        },
        "accept" | "ignore" => match rest.trim_start_matches('#').parse() {
            Ok(id) => Command::Proposal { id, accept: name == "accept" },
            Err(_) => Command::Unknown(format!("usage: /{name} <proposal>")),
        },
        _ => Command::Unknown(format!("I don't know {head:?}.")),
    }
}

pub const HELP: &str = "jazari commands:
/new <name> <idea> – start a product from an idea
/idea <product> <idea> – give a product a new idea
/tasks – what the team is doing and what waits for you
/answer <task> <answer> – answer a task's question
/approve <task>, /deny <task> – decide on a deploy, spend or contact
/mode <product> normal|autonomous – stop after your tasks, or keep improving
/feedback <product|all> <text> – how you like the work (a plain message works too)
/prefer <product|all> <area> <rule> – set a preference now
/prefs, /forget <id> – see or drop what jazari learned about you
/status – sessions and usage limits
/tell <session> <message> – message a session (it reads it at its next step)
/stop <session> – stop a session before its next tool call
/pause [30m|2h|1d] – hold what marsad does on its own
/resume – turn it back on
Sessions are named by the first letters of their id, as /status shows them.";

/// What the bridge does to the machine, so tests run without one.
pub trait Ops {
    fn status(&self) -> String;
    fn pause(&self, duration: Option<&str>) -> anyhow::Result<String>;
    fn resume(&self) -> anyhow::Result<String>;
    fn tell(&self, session: &str, text: &str) -> anyhow::Result<String>;
    fn stop(&self, session: &str) -> anyhow::Result<String>;
    fn idea(&self, product: &str, text: &str) -> anyhow::Result<String>;
    fn new_product(&self, name: &str, idea: &str) -> anyhow::Result<String>;
    fn tasks(&self) -> anyhow::Result<String>;
    fn answer(&self, task: i64, text: &str) -> anyhow::Result<String>;
    fn decide(&self, task: i64, approve: bool) -> anyhow::Result<String>;
    fn feedback(&self, product: Option<&str>, text: &str) -> anyhow::Result<String>;
    fn prefer(&self, product: &str, area: &str, rule: &str) -> anyhow::Result<String>;
    fn forget(&self, id: i64) -> anyhow::Result<String>;
    fn prefs(&self) -> anyhow::Result<String>;
    fn mode(&self, product: &str, mode: &str) -> anyhow::Result<String>;
    fn proposal(&self, id: i64, accept: bool) -> anyhow::Result<String>;
}

/// The reply to one command.
pub fn handle(cmd: Command, ops: &dyn Ops) -> String {
    let done = |r: anyhow::Result<String>| r.unwrap_or_else(|e| format!("Couldn't: {e}"));
    match cmd {
        Command::Help => HELP.into(),
        Command::Status => ops.status(),
        Command::Pause(d) => done(ops.pause(d.as_deref())),
        Command::Resume => done(ops.resume()),
        Command::Tell { session, text } => done(ops.tell(&session, &text)),
        Command::Stop(session) => done(ops.stop(&session)),
        Command::Idea { product, text } => done(ops.idea(&product, &text)),
        Command::New { name, idea } => done(ops.new_product(&name, &idea)),
        Command::Tasks => done(ops.tasks()),
        Command::Answer { task, text } => done(ops.answer(task, &text)),
        Command::Decide { task, approve } => done(ops.decide(task, approve)),
        Command::Feedback { product, text } => done(ops.feedback(product.as_deref(), &text)),
        Command::Prefer { product, area, rule } => done(ops.prefer(&product, &area, &rule)),
        Command::Forget(id) => done(ops.forget(id)),
        Command::Prefs => done(ops.prefs()),
        Command::Mode { product, mode } => done(ops.mode(&product, &mode)),
        Command::Proposal { id, accept } => done(ops.proposal(id, accept)),
        Command::Unknown(why) => format!("{why}\n\n{HELP}"),
    }
}

/// The one live session whose id starts with `prefix`.
pub fn pick<'a>(sessions: &'a HashMap<String, crate::model::Session>, prefix: &str) -> anyhow::Result<&'a crate::model::Session> {
    anyhow::ensure!(prefix.len() >= 4, "give at least 4 letters of the session id");
    let hits: Vec<_> = sessions.values().filter(|s| s.id.starts_with(prefix)).collect();
    match hits.as_slice() {
        [s] => Ok(s),
        [] => anyhow::bail!("no live session starts with {prefix}"),
        _ => anyhow::bail!("{} sessions start with {prefix}; give more letters", hits.len()),
    }
}

/// The real machine: marsad's sessions, pause and inbox.
pub struct Machine;

fn pct(w: Option<crate::model::RateWindow>) -> String {
    match w {
        Some(w) => {
            let reset = w
                .resets_at
                .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                .map(|t| format!(", resets {}", t.with_timezone(&chrono::Local).format("%a %H:%M")))
                .unwrap_or_default();
            format!("{:.0}%{reset}", w.used_pct)
        }
        None => "unknown".into(),
    }
}

impl Ops for Machine {
    fn status(&self) -> String {
        let sessions = crate::live::load_sessions(false);
        let mut list: Vec<_> = sessions.values().collect();
        list.sort_by(|a, b| a.status.group().cmp(&b.status.group()).then(b.last_event.total_cmp(&a.last_event)));
        let mut out = if list.is_empty() { "No live sessions.".to_string() } else { format!("{} live session(s):", list.len()) };
        for s in list.iter().take(15) {
            let what = s.said.as_deref().filter(|t| !t.is_empty()).unwrap_or(&s.detail);
            let flag = if s.status.needs_user() { "⚠️ " } else { "" };
            out.push_str(&format!("\n{flag}{} · {} · {} — {}", &s.id[..s.id.len().min(8)], s.project(), s.status.as_str(), crate::hook::truncate(what, 80)));
        }
        if let Some((five, seven)) = crate::app::latest_rate_limits(&sessions) {
            out.push_str(&format!("\n\nUsage: 5h {} · 7d {}", pct(five), pct(seven)));
        }
        if crate::act::paused() {
            out.push_str("\nmarsad is paused.");
        }
        out
    }

    fn pause(&self, duration: Option<&str>) -> anyhow::Result<String> {
        let label = crate::act::pause(duration)?;
        Ok(format!("Paused{label}: marsad won't interrupt or end sessions, clean up or update on its own."))
    }

    fn resume(&self) -> anyhow::Result<String> {
        Ok(if crate::act::resume()? { "Resumed: marsad acts on its own again." } else { "marsad wasn't paused." }.into())
    }

    fn tell(&self, session: &str, text: &str) -> anyhow::Result<String> {
        let sessions = crate::live::load_sessions(false);
        let s = pick(&sessions, session)?;
        crate::channel::tell(&s.id, text)?;
        Ok(format!("Queued for {} ({}): it reads it at its next tool call or prompt.", s.project(), &s.id[..8.min(s.id.len())]))
    }

    fn stop(&self, session: &str) -> anyhow::Result<String> {
        let sessions = crate::live::load_sessions(false);
        let s = pick(&sessions, session)?;
        crate::channel::request_stop(&s.id)?;
        Ok(format!("{} stops before its next tool call; `claude --resume {}` continues it.", s.project(), s.id))
    }

    fn idea(&self, product: &str, text: &str) -> anyhow::Result<String> {
        let id = super::queue_idea(&super::open()?, product, text)?;
        Ok(format!("Idea queued as task #{id}: the Chief of Staff turns it into a plan and the first tasks."))
    }

    fn new_product(&self, name: &str, idea: &str) -> anyhow::Result<String> {
        let (repo, id) = super::create_product(&super::open()?, name, idea)?;
        Ok(format!("Created {name} in {}. Task #{id}: the Chief of Staff turns the idea into a plan. Run `marsad jazari run` if the supervisor isn't running.", repo.display()))
    }

    fn tasks(&self) -> anyhow::Result<String> {
        super::task_summary(&super::open()?)
    }

    fn answer(&self, task: i64, text: &str) -> anyhow::Result<String> {
        super::answer_task(&super::open()?, task, text)?;
        Ok(format!("Thanks. Task #{task} is back in the queue with your answer."))
    }

    fn decide(&self, task: i64, approve: bool) -> anyhow::Result<String> {
        super::decide(&super::open()?, task, approve)
    }

    fn feedback(&self, product: Option<&str>, text: &str) -> anyhow::Result<String> {
        let store = super::open()?;
        let product = match product {
            Some(p) => p.to_string(),
            None => store.get("last_product")?.or_else(|| store.products().ok().filter(|p| p.len() == 1).map(|p| p[0].name.clone())).unwrap_or_else(|| "all".into()),
        };
        super::feedback(&store, &product, text)
    }

    fn prefer(&self, product: &str, area: &str, rule: &str) -> anyhow::Result<String> {
        super::prefer(&super::open()?, product, area, rule)
    }

    fn forget(&self, id: i64) -> anyhow::Result<String> {
        super::forget(&super::open()?, id)
    }

    fn prefs(&self) -> anyhow::Result<String> {
        super::profile::summary(&super::open()?, None)
    }

    fn mode(&self, product: &str, mode: &str) -> anyhow::Result<String> {
        super::set_mode(&super::open()?, product, mode)
    }

    fn proposal(&self, id: i64, accept: bool) -> anyhow::Result<String> {
        super::proposal(&super::open()?, id, accept)
    }
}

// ---------- state ----------

/// Where the bridge keeps its Telegram offset.
fn offset_path(dir: &Path) -> PathBuf {
    dir.join("telegram-offset")
}

fn load_offset(dir: &Path) -> Option<i64> {
    std::fs::read_to_string(offset_path(dir)).ok()?.trim().parse().ok()
}

fn save_offset(dir: &Path, offset: Option<i64>) {
    if let Some(o) = offset {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(offset_path(dir), o.to_string());
    }
}

// ---------- pairing ----------

/// A 6-digit code from the system's random source.
fn pairing_code() -> String {
    let mut buf = [0u8; 4];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf)).is_ok();
    let n = if random { u32::from_le_bytes(buf) } else { (crate::now().fract() * 1e9) as u32 ^ std::process::id().rotate_left(16) };
    format!("{:06}", n % 1_000_000)
}

/// Waits for `code` in a private chat and returns that chat's id. Wrong codes count against
/// [`PAIR_TRIES`]; other chats get no reply.
pub fn wait_for_code<T: Transport>(bot: &Bot<T>, code: &str, mut offset: Option<i64>, deadline: f64, now: impl Fn() -> f64) -> anyhow::Result<(i64, Option<i64>)> {
    let mut wrong = 0;
    while now() < deadline {
        let (updates, next) = bot.updates(offset, POLL_SECS)?;
        offset = next.or(offset);
        for u in updates.iter().filter(|u| u.private) {
            if u.text.trim() == code {
                return Ok((u.chat_id, offset));
            }
            wrong += 1;
            anyhow::ensure!(wrong < PAIR_TRIES, "{PAIR_TRIES} wrong codes; run `marsad jazari telegram pair` again");
        }
    }
    anyhow::bail!("no code arrived in {} minutes; run `marsad jazari telegram pair` again", PAIR_SECS / 60.0)
}

pub fn pair<T: Transport>(bot: &Bot<T>, dir: &Path) -> anyhow::Result<()> {
    let name = bot.username()?;
    // Whatever was sent before the code was shown is acknowledged and ignored.
    let (_, offset) = bot.updates(Some(-1), 0)?;
    let code = pairing_code();
    println!("Open a private chat with @{name} in Telegram and send this code within 10 minutes:\n\n    {code}\n");
    let (chat, offset) = wait_for_code(bot, &code, offset, crate::now() + PAIR_SECS, crate::now)?;
    super::secrets::set(super::secrets::TELEGRAM_CHAT, &chat.to_string())?;
    save_offset(dir, offset);
    bot.send(chat, "Paired. Only this chat can control jazari now. Send /help for the commands.")?;
    println!("Paired with that chat. `marsad jazari bridge` starts listening for commands.");
    Ok(())
}

// ---------- the bridge loop ----------

/// The text sent for a session's `marsad say --attention`.
pub fn attention_text(ev: &serde_json::Value, project: Option<&str>) -> Option<String> {
    if ev["hook_event_name"] != crate::channel::SAID_EVENT || ev["attention"] != true {
        return None;
    }
    let id = ev["session_id"].as_str().unwrap_or_default();
    let msg = ev["message"].as_str().unwrap_or_default();
    Some(format!("🔔 {} ({}) needs you:\n{msg}", project.unwrap_or("a session"), &id[..id.len().min(8)]))
}

/// One round: answer the paired chat's commands, drop everything else. Returns the next offset.
/// Each update is acknowledged (`seen`) before it is acted on, so a reply that fails to send never
/// makes a command run twice.
pub fn serve_once<T: Transport>(bot: &Bot<T>, chat: i64, offset: Option<i64>, ops: &dyn Ops, seen: &mut dyn FnMut(i64)) -> anyhow::Result<Option<i64>> {
    let (updates, next) = bot.updates(offset, POLL_SECS)?;
    for u in updates {
        seen(u.id + 1);
        if u.chat_id != chat || !u.private {
            eprintln!("[jazari] ignored a message from an unpaired chat");
            continue;
        }
        if let Some(cb) = &u.callback {
            bot.answer_button(cb);
        }
        bot.send(chat, &handle(parse(&u.text), ops))?;
    }
    Ok(next.or(offset))
}

fn try_lock(dir: &Path) -> anyhow::Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("bridge.lock"))?;
    file.try_lock().map_err(|_| anyhow::anyhow!("a jazari bridge is already running"))?;
    Ok(file)
}

pub fn run<T: Transport>(bot: &Bot<T>, chat: i64, dir: &Path) -> anyhow::Result<()> {
    let _lock = try_lock(dir)?;
    crate::act::set_origin(crate::act::Origin::Telegram);
    let mut offset = load_offset(dir);
    // Only notes posted from now on are forwarded.
    let mut events = crate::jsonl::JsonlTail::new(crate::claude_data_dir().join("events.jsonl"));
    events.read_new();
    bot.send(chat, "jazari bridge is on. Send /help for the commands.")?;
    println!("Listening for commands from the paired chat. Ctrl-C stops the bridge.");
    let mut failures = 0u32;
    loop {
        let mut seen = offset;
        let round = serve_once(bot, chat, offset, &Machine, &mut |o| {
            save_offset(dir, Some(o));
            seen = Some(o);
        });
        offset = seen;
        match round {
            Ok(next) => {
                failures = 0;
                if next != offset {
                    offset = next;
                    save_offset(dir, offset);
                }
            }
            Err(e) => {
                failures += 1;
                eprintln!("[jazari] {e}");
                std::thread::sleep(std::time::Duration::from_secs((5 * u64::from(failures)).min(120)));
            }
        }
        let notes: Vec<_> = events.read_new();
        if notes.iter().any(|ev| ev["attention"] == true) {
            let sessions = crate::live::load_sessions(true);
            for ev in &notes {
                let project = ev["session_id"].as_str().and_then(|id| sessions.get(id)).map(|s| s.project());
                if let Some(text) = attention_text(ev, project.as_deref()) {
                    let _ = bot.send(chat, &text);
                }
            }
        }
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jazari::telegram::tests::Fake;
    use serde_json::json;
    use std::cell::RefCell;

    #[test]
    fn commands_parse_with_bot_names_and_arguments() {
        assert_eq!(parse("/start"), Command::Help);
        assert_eq!(parse("/Status@ForhadJazariBot"), Command::Status);
        assert_eq!(parse("/pause 2h"), Command::Pause(Some("2h".into())));
        assert_eq!(parse("/pause"), Command::Pause(None));
        assert_eq!(parse("/tell 1fe6ab run the tests\nthen push"), Command::Tell { session: "1fe6ab".into(), text: "run the tests\nthen push".into() });
        assert!(matches!(parse("/tell 1fe6ab"), Command::Unknown(_)));
        assert_eq!(parse("/stop 1fe6ab now"), Command::Stop("1fe6ab".into()));
        assert!(matches!(parse("/stop"), Command::Unknown(_)));
        assert_eq!(parse("rm -rf /"), Command::Feedback { product: None, text: "rm -rf /".into() }, "plain text is feedback, never a command");
        assert_eq!(parse("make the buttons green"), Command::Feedback { product: None, text: "make the buttons green".into() });
        assert_eq!(parse("/feedback shop shorter sentences"), Command::Feedback { product: Some("shop".into()), text: "shorter sentences".into() });
        assert_eq!(parse("/prefer all Copy Keep UI sentences short"), Command::Prefer { product: "all".into(), area: "copy".into(), rule: "Keep UI sentences short".into() });
        assert_eq!(parse("/forget #4"), Command::Forget(4));
        assert_eq!(parse("/mode shop Autonomous"), Command::Mode { product: "shop".into(), mode: "autonomous".into() });
        assert_eq!(parse("/accept 2"), Command::Proposal { id: 2, accept: true });
        assert!(matches!(parse("/mode shop"), Command::Unknown(_)) && matches!(parse("/frobnicate"), Command::Unknown(_)));
        assert_eq!(parse("/new notes-app A private notes app"), Command::New { name: "notes-app".into(), idea: "A private notes app".into() });
        assert_eq!(parse("/idea shop Add gift cards"), Command::Idea { product: "shop".into(), text: "Add gift cards".into() });
        assert_eq!(parse("/answer #12 Use Twilio"), Command::Answer { task: 12, text: "Use Twilio".into() });
        assert!(matches!(parse("/answer 12"), Command::Unknown(_)) && matches!(parse("/approve x"), Command::Unknown(_)));
        assert_eq!(parse("/deny 7"), Command::Decide { task: 7, approve: false });
        assert_eq!(parse("/tasks"), Command::Tasks);
    }

    #[derive(Default)]
    struct FakeOps {
        log: RefCell<Vec<String>>,
    }

    impl Ops for FakeOps {
        fn status(&self) -> String {
            "2 live sessions".into()
        }
        fn pause(&self, d: Option<&str>) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("pause {d:?}"));
            Ok("paused".into())
        }
        fn resume(&self) -> anyhow::Result<String> {
            anyhow::bail!("nothing to resume")
        }
        fn tell(&self, s: &str, t: &str) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("tell {s} {t}"));
            Ok("queued".into())
        }
        fn stop(&self, s: &str) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("stop {s}"));
            Ok("stopping".into())
        }
        fn idea(&self, p: &str, t: &str) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("idea {p} {t}"));
            Ok("queued".into())
        }
        fn new_product(&self, n: &str, i: &str) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("new {n} {i}"));
            Ok("created".into())
        }
        fn tasks(&self) -> anyhow::Result<String> {
            Ok("no tasks".into())
        }
        fn answer(&self, t: i64, a: &str) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("answer {t} {a}"));
            Ok("thanks".into())
        }
        fn decide(&self, t: i64, yes: bool) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("decide {t} {yes}"));
            Ok("decided".into())
        }
        fn feedback(&self, p: Option<&str>, t: &str) -> anyhow::Result<String> {
            self.log.borrow_mut().push(format!("feedback {p:?} {t}"));
            Ok("noted".into())
        }
        fn prefer(&self, _p: &str, _a: &str, _r: &str) -> anyhow::Result<String> {
            Ok("set".into())
        }
        fn forget(&self, _id: i64) -> anyhow::Result<String> {
            Ok("forgot".into())
        }
        fn prefs(&self) -> anyhow::Result<String> {
            Ok("none".into())
        }
        fn mode(&self, _p: &str, _m: &str) -> anyhow::Result<String> {
            Ok("mode".into())
        }
        fn proposal(&self, _id: i64, _a: bool) -> anyhow::Result<String> {
            Ok("ok".into())
        }
    }

    fn msg(id: i64, chat: i64, kind: &str, text: &str) -> serde_json::Value {
        json!({"update_id": id, "message": {"chat": {"id": chat, "type": kind}, "text": text}})
    }

    #[test]
    fn only_the_paired_private_chat_is_answered() {
        let fake = Fake::default();
        fake.replies.borrow_mut().push(json!([
            msg(5, 999, "private", "/stop abcd1234"),
            msg(6, 42, "group", "/stop abcd1234"),
            msg(7, 42, "private", "/tell abcd1234 use staging"),
            msg(8, 42, "private", "/resume"),
            json!({"update_id": 9, "callback_query": {"id": "cb", "from": {"id": 42}, "data": "/approve 3", "message": {"chat": {"id": 42, "type": "private"}}}}),
        ]));
        let bot = Bot { transport: fake };
        let ops = FakeOps::default();
        let mut seen = Vec::new();
        assert_eq!(serve_once(&bot, 42, Some(5), &ops, &mut |o| seen.push(o)).unwrap(), Some(10));
        assert_eq!(seen, vec![6, 7, 8, 9, 10], "each update is acknowledged before it is acted on");
        assert_eq!(*ops.log.borrow(), vec!["tell abcd1234 use staging", "decide 3 true"]);
        let calls = bot.transport.calls.borrow();
        assert!(calls.iter().any(|c| c.0 == "answerCallbackQuery"));
        let sent: Vec<_> = calls.iter().filter(|c| c.0 == "sendMessage").collect();
        assert_eq!(sent.len(), 3);
        assert!(sent.iter().all(|c| c.1.contains(&("chat_id".into(), "42".into()))));
        assert!(sent[1].1.iter().any(|(k, v)| k == "text" && v.starts_with("Couldn't: nothing to resume")));
        assert!(calls[0].1.contains(&("offset".into(), "5".into())));
    }

    /// Answers getUpdates from a script and fails every sendMessage.
    struct NoSend(RefCell<Vec<serde_json::Value>>);

    impl Transport for NoSend {
        fn call(&self, method: &str, _p: &[(&str, String)], _w: u64) -> anyhow::Result<serde_json::Value> {
            anyhow::ensure!(method != "sendMessage", "network down");
            let mut r = self.0.borrow_mut();
            Ok(if r.is_empty() { json!([]) } else { r.remove(0) })
        }
    }

    #[test]
    fn a_reply_that_fails_to_send_does_not_run_the_command_again() {
        let bot = Bot { transport: NoSend(RefCell::new(vec![json!([msg(7, 42, "private", "/stop abcd1234")])])) };
        let ops = FakeOps::default();
        let mut offset = Some(7);
        let mut seen = offset;
        assert!(serve_once(&bot, 42, offset, &ops, &mut |o| seen = Some(o)).is_err());
        offset = seen;
        assert_eq!(offset, Some(8), "the update was acknowledged before the reply failed");
        serve_once(&bot, 42, offset, &ops, &mut |_| {}).unwrap();
        assert_eq!(*ops.log.borrow(), vec!["stop abcd1234"], "it ran once");
    }

    #[test]
    fn pairing_takes_the_code_from_a_private_chat_only() {
        let fake = Fake::default();
        fake.replies.borrow_mut().extend([json!([msg(1, -5, "group", "123456"), msg(2, 77, "private", "hello")]), json!([msg(3, 42, "private", " 123456 ")])]);
        let bot = Bot { transport: fake };
        assert_eq!(wait_for_code(&bot, "123456", None, f64::MAX, || 0.0).unwrap(), (42, Some(4)));

        let fake = Fake::default();
        fake.replies.borrow_mut().push(json!((0..5).map(|i| msg(i, 42, "private", "000000")).collect::<Vec<_>>()));
        let err = wait_for_code(&Bot { transport: fake }, "123456", None, f64::MAX, || 0.0).unwrap_err();
        assert!(err.to_string().contains("wrong codes"), "{err}");

        let err = wait_for_code(&Bot { transport: Fake::default() }, "123456", None, 1.0, || 2.0).unwrap_err();
        assert!(err.to_string().contains("no code arrived"), "{err}");
        assert_eq!(pairing_code().len(), 6);
    }

    #[test]
    fn only_attention_notes_are_forwarded() {
        let ev = json!({"hook_event_name": crate::channel::SAID_EVENT, "session_id": "1fe6ab99-x", "message": "need the staging key", "attention": true});
        assert_eq!(attention_text(&ev, Some("shop")).unwrap(), "🔔 shop (1fe6ab99) needs you:\nneed the staging key");
        let quiet = json!({"hook_event_name": crate::channel::SAID_EVENT, "session_id": "1", "message": "tests next", "attention": false});
        assert_eq!(attention_text(&quiet, None), None);
    }

    #[test]
    fn a_session_is_picked_only_by_a_unique_prefix_of_four_or_more() {
        let mut sessions = HashMap::new();
        for id in ["abcd1111", "abcd2222", "ffff0000"] {
            sessions.insert(id.to_string(), crate::model::Session::new(id, 0.0));
        }
        assert_eq!(pick(&sessions, "ffff").unwrap().id, "ffff0000");
        assert!(pick(&sessions, "abcd").unwrap_err().to_string().contains("2 sessions"));
        assert!(pick(&sessions, "ff").is_err() && pick(&sessions, "9999").is_err());
    }
}

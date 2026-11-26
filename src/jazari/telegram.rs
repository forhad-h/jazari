//! The Telegram Bot API, over `curl`. The bot token is part of the URL, so the URL goes to curl
//! on stdin (`-K -`) and never appears in the process list. Everything sent is redacted first.

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

/// Telegram refuses longer messages.
const MAX_MESSAGE_CHARS: usize = 4000;

/// One Bot API call: the method, its form fields and how long the server may hold it.
pub trait Transport {
    fn call(&self, method: &str, params: &[(&str, String)], wait_secs: u64) -> anyhow::Result<Value>;
}

pub struct Curl {
    token: String,
}

impl Curl {
    pub fn new(token: String) -> Self {
        Curl { token }
    }
}

impl Transport for Curl {
    fn call(&self, method: &str, params: &[(&str, String)], wait_secs: u64) -> anyhow::Result<Value> {
        anyhow::ensure!(
            !self.token.is_empty() && self.token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-')),
            "the Telegram bot token looks wrong"
        );
        let mut args: Vec<String> = ["--silent", "--show-error", "--proto", "=https", "-K", "-", "--max-time"].map(String::from).to_vec();
        args.push((wait_secs + 15).to_string());
        for (k, v) in params {
            args.push("--data-urlencode".into());
            args.push(format!("{k}={v}"));
        }
        let mut child = Command::new("curl").args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "url = \"https://api.telegram.org/bot{}/{method}\"", self.token)?;
        }
        let out = child.wait_with_output()?;
        anyhow::ensure!(out.status.success(), "Telegram couldn't be reached: {}", String::from_utf8_lossy(&out.stderr).trim());
        let v: Value = serde_json::from_slice(&out.stdout)?;
        if v["ok"] != Value::Bool(true) {
            anyhow::bail!("Telegram refused {method}: {}", v["description"].as_str().unwrap_or("no reason given"));
        }
        Ok(v["result"].clone())
    }
}

/// A message someone sent the bot.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub id: i64,
    pub chat_id: i64,
    /// A one-to-one chat with the bot, not a group or a channel.
    pub private: bool,
    pub text: String,
}

/// The messages in a `getUpdates` result, and the offset that acknowledges all of it (other kinds
/// of update are acknowledged too, so they don't come back).
pub fn parse_updates(result: &Value) -> (Vec<Update>, Option<i64>) {
    let list = result.as_array().map(Vec::as_slice).unwrap_or_default();
    let next = list.iter().filter_map(|u| u["update_id"].as_i64()).max().map(|id| id + 1);
    let updates = list
        .iter()
        .filter_map(|u| {
            let m = &u["message"];
            Some(Update {
                id: u["update_id"].as_i64()?,
                chat_id: m["chat"]["id"].as_i64()?,
                private: m["chat"]["type"] == "private",
                text: m["text"].as_str()?.to_string(),
            })
        })
        .collect();
    (updates, next)
}

pub struct Bot<T: Transport> {
    pub transport: T,
}

impl<T: Transport> Bot<T> {
    /// The bot's @username.
    pub fn username(&self) -> anyhow::Result<String> {
        let me = self.transport.call("getMe", &[], 0)?;
        Ok(me["username"].as_str().unwrap_or("your bot").to_string())
    }

    /// Waits up to `wait_secs` for messages after `offset`, acknowledging everything before it.
    pub fn updates(&self, offset: Option<i64>, wait_secs: u64) -> anyhow::Result<(Vec<Update>, Option<i64>)> {
        let mut params = vec![("timeout", wait_secs.to_string()), ("allowed_updates", r#"["message"]"#.to_string())];
        if let Some(o) = offset {
            params.push(("offset", o.to_string()));
        }
        Ok(parse_updates(&self.transport.call("getUpdates", &params, wait_secs)?))
    }

    /// Sends `text` to `chat`, redacted and split into parts Telegram accepts.
    pub fn send(&self, chat: i64, text: &str) -> anyhow::Result<()> {
        for part in split(&redact(text), MAX_MESSAGE_CHARS) {
            self.transport.call("sendMessage", &[("chat_id", chat.to_string()), ("text", part), ("disable_web_page_preview", "true".into())], 0)?;
        }
        Ok(())
    }
}

/// Splits on line breaks where it can, so no part is longer than `max` characters.
fn split(text: &str, max: usize) -> Vec<String> {
    let mut parts = vec![String::new()];
    for line in text.split_inclusive('\n') {
        let mut line = line.to_string();
        while line.chars().count() > max {
            let cut = line.char_indices().nth(max).map_or(line.len(), |(i, _)| i);
            parts.push(line[..cut].to_string());
            line = line[cut..].to_string();
        }
        let last = parts.last_mut().expect("parts is never empty");
        if last.chars().count() + line.chars().count() > max {
            parts.push(line);
        } else {
            last.push_str(&line);
        }
    }
    parts.into_iter().map(|p| p.trim_end().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Prefixes of well-known credentials.
const SECRET_PREFIXES: [&str; 12] =
    ["sk-ant-", "sk-proj-", "ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "AKIA", "AIza"];
/// Words that, before `=` or `:`, mark the value after them as a secret.
const SECRET_KEYS: [&str; 6] = ["token", "secret", "password", "passwd", "api_key", "apikey"];

/// Replaces anything that looks like a credential with `[redacted]`: known token prefixes, Telegram
/// bot tokens, and the value in `KEY=value` when the key names a secret.
pub fn redact(text: &str) -> String {
    let is_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.' | '/' | '+' | '=');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(is_word) {
        out.push_str(&rest[..start]);
        let end = rest[start..].find(|c: char| !is_word(c)).map_or(rest.len(), |e| start + e);
        out.push_str(&redact_word(&rest[start..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn redact_word(word: &str) -> String {
    if let Some(i) = word.find(['=', ':']) {
        let key = word[..i].to_ascii_lowercase();
        if i + 1 < word.len() && SECRET_KEYS.iter().any(|k| key.ends_with(k)) {
            return format!("{}[redacted]", &word[..=i]);
        }
    }
    let secret = SECRET_PREFIXES.iter().any(|p| word.starts_with(p) && word.len() >= p.len() + 12) || is_bot_token(word);
    if secret { "[redacted]".into() } else { word.to_string() }
}

/// `123456789:AA…`: a bot id, a colon and a 35-character key.
fn is_bot_token(word: &str) -> bool {
    word.split_once(':').is_some_and(|(id, key)| {
        id.len() >= 6 && id.chars().all(|c| c.is_ascii_digit()) && key.len() >= 30 && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    /// A recorded call: the method and its form fields.
    pub type Call = (String, Vec<(String, String)>);

    /// Answers each call with the next canned result and records what was asked.
    #[derive(Default)]
    pub struct Fake {
        pub calls: RefCell<Vec<Call>>,
        pub replies: RefCell<Vec<Value>>,
    }

    impl Transport for Fake {
        fn call(&self, method: &str, params: &[(&str, String)], _wait: u64) -> anyhow::Result<Value> {
            self.calls.borrow_mut().push((method.into(), params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()));
            let mut r = self.replies.borrow_mut();
            Ok(if r.is_empty() { json!([]) } else { r.remove(0) })
        }
    }

    #[test]
    fn updates_keep_messages_and_acknowledge_everything() {
        let result = json!([
            {"update_id": 10, "message": {"chat": {"id": 42, "type": "private"}, "text": "/status"}},
            {"update_id": 11, "message": {"chat": {"id": -7, "type": "group"}, "text": "hi"}},
            {"update_id": 12, "edited_message": {"chat": {"id": 42, "type": "private"}, "text": "x"}},
            {"update_id": 13, "message": {"chat": {"id": 42, "type": "private"}, "photo": []}}
        ]);
        let (u, next) = parse_updates(&result);
        assert_eq!(next, Some(14));
        assert_eq!(u, vec![
            Update { id: 10, chat_id: 42, private: true, text: "/status".into() },
            Update { id: 11, chat_id: -7, private: false, text: "hi".into() },
        ]);
        assert_eq!(parse_updates(&json!([])), (vec![], None));
    }

    #[test]
    fn credentials_are_redacted_and_ordinary_text_is_not() {
        let bot = format!("1234567890:{}", "A".repeat(35));
        let text = format!("token {bot} and sk-ant-oat01-abcdefghijklmnop, GITHUB_TOKEN=ghx123 password: hunter2 ok");
        let r = redact(&text);
        assert!(!r.contains(&bot) && !r.contains("sk-ant-oat01") && !r.contains("ghx123"), "{r}");
        assert!(r.contains("GITHUB_TOKEN=[redacted]"), "{r}");
        assert_eq!(redact("5h 8% · resets 14:05, see src/main.rs:12"), "5h 8% · resets 14:05, see src/main.rs:12");
        assert_eq!(redact("sk-ant-"), "sk-ant-");
    }

    #[test]
    fn long_messages_are_split_on_lines() {
        let text = format!("{}\n{}\n{}", "a".repeat(6), "b".repeat(6), "c".repeat(25));
        let parts = split(&text, 10);
        assert_eq!(parts, vec!["aaaaaa", "bbbbbb", "cccccccccc", "cccccccccc", "ccccc"]);
        assert!(split("", 10).is_empty());
    }

    #[test]
    fn send_redacts_and_names_the_chat() {
        let bot = Bot { transport: Fake::default() };
        bot.send(42, "key sk-ant-api03-abcdefghijklmnop").unwrap();
        let calls = bot.transport.calls.borrow();
        assert_eq!(calls[0].0, "sendMessage");
        assert!(calls[0].1.contains(&("chat_id".into(), "42".into())));
        assert!(calls[0].1.contains(&("text".into(), "key [redacted]".into())));
    }
}

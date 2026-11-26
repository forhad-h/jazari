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
    /// Set when this is a button press: the id Telegram wants answered.
    pub callback: Option<String>,
}

/// The messages in a `getUpdates` result, and the offset that acknowledges all of it (other kinds
/// of update are acknowledged too, so they don't come back).
pub fn parse_updates(result: &Value) -> (Vec<Update>, Option<i64>) {
    let list = result.as_array().map(Vec::as_slice).unwrap_or_default();
    let next = list.iter().filter_map(|u| u["update_id"].as_i64()).max().map(|id| id + 1);
    let updates = list
        .iter()
        .filter_map(|u| {
            let id = u["update_id"].as_i64()?;
            if let Some(q) = u.get("callback_query") {
                // A button press: its data is the command the button stands for.
                let m = &q["message"];
                return Some(Update {
                    id,
                    chat_id: m["chat"]["id"].as_i64()?,
                    private: m["chat"]["type"] == "private" && q["from"]["id"] == m["chat"]["id"],
                    text: q["data"].as_str()?.to_string(),
                    callback: q["id"].as_str().map(String::from),
                });
            }
            let m = &u["message"];
            Some(Update {
                id,
                chat_id: m["chat"]["id"].as_i64()?,
                private: m["chat"]["type"] == "private",
                text: m["text"].as_str()?.to_string(),
                callback: None,
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
        let mut params = vec![("timeout", wait_secs.to_string()), ("allowed_updates", r#"["message","callback_query"]"#.to_string())];
        if let Some(o) = offset {
            params.push(("offset", o.to_string()));
        }
        Ok(parse_updates(&self.transport.call("getUpdates", &params, wait_secs)?))
    }

    /// Sends `text` to `chat`, redacted and split into parts Telegram accepts.
    pub fn send(&self, chat: i64, text: &str) -> anyhow::Result<()> {
        self.send_with_buttons(chat, text, &[])
    }

    /// Sends `text` with one row of buttons under its last part; each button sends back its
    /// command (label, command).
    pub fn send_with_buttons(&self, chat: i64, text: &str, buttons: &[(String, String)]) -> anyhow::Result<()> {
        let parts = split(&redact(text), MAX_MESSAGE_CHARS);
        let last = parts.len().saturating_sub(1);
        for (i, part) in parts.into_iter().enumerate() {
            let mut params = vec![("chat_id", chat.to_string()), ("text", part), ("disable_web_page_preview", "true".into())];
            if i == last && !buttons.is_empty() {
                let row: Vec<_> = buttons.iter().map(|(label, data)| serde_json::json!({ "text": label, "callback_data": data })).collect();
                params.push(("reply_markup", serde_json::json!({ "inline_keyboard": [row] }).to_string()));
            }
            self.transport.call("sendMessage", &params, 0)?;
        }
        Ok(())
    }

    /// Stops the spinner on a pressed button.
    pub fn answer_button(&self, callback: &str) {
        let _ = self.transport.call("answerCallbackQuery", &[("callback_query_id", callback.to_string())], 0);
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

/// Credential patterns, each with the part to replace: the whole match, or its `secret` group.
/// They drive both what reaches Telegram and the secret scan of every branch.
fn patterns() -> &'static [regex::Regex] {
    static P: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        [
            // A PEM private key block, whatever its kind.
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
            // Well-known token formats.
            r"\b(sk-ant-[A-Za-z0-9_-]{12,}|sk-proj-[A-Za-z0-9_-]{12,}|sk-[A-Za-z0-9]{20,}|[sr]k_live_[A-Za-z0-9]{10,}|pk_live_[A-Za-z0-9]{10,}|gh[poushr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_-]{16,}|xox[abprs]-[A-Za-z0-9-]{10,}|xapp-[A-Za-z0-9-]{10,}|AKIA[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{30,})",
            // A JSON Web Token.
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            // A Telegram bot token.
            r"\b\d{6,}:[A-Za-z0-9_-]{30,}",
            // Authorization headers.
            r"(?i)\b(authorization\s*[:=]\s*)?(bearer|basic|token)\s+(?P<secret>[A-Za-z0-9._~+/=-]{12,})",
            // Credentials in a URL: scheme://user:pass@host.
            r"[a-z][a-z0-9+.-]*://[^\s:/@]+:(?P<secret>[^\s@/]+)@",
            // NAME = value, NAME: value or "name": "value", when the name says it holds a secret.
            r#"(?i)["']?\b[a-z0-9_.-]*(password|passwd|pwd|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|client[_-]?secret|_key)["']?\s*[:=]\s*["']?(?P<secret>[^\s"',;)}\]]{6,})"#,
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("valid credential pattern"))
        .collect()
    })
}

/// Values that look like a secret's but are code: a lookup, a placeholder, a literal.
fn harmless(value: &str) -> bool {
    let v = value.trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase();
    ["true", "false", "none", "null", "undefined", "required", "optional"].contains(&v.as_str())
        || v.starts_with("os.environ")
        || v.starts_with("process.env")
        || v.starts_with("env(")
        || v.starts_with("getenv")
        || v.starts_with("os.getenv")
        || v.starts_with("${")
        || v.starts_with("$(")
        || v.starts_with('<')
        || v.starts_with("[redacted")
        || v.chars().all(|c| c.is_ascii_digit())
        || v.contains("example")
        || v.contains("xxxx")
        || v.contains("****")
}

/// Replaces anything that looks like a credential with `[redacted]`.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for re in patterns() {
        out = re
            .replace_all(&out, |c: &regex::Captures| match c.name("secret") {
                Some(s) if harmless(s.as_str()) => c[0].to_string(),
                Some(s) => {
                    let start = c.get(0).expect("a match has group 0").start();
                    format!("{}[redacted]{}", &c[0][..s.start() - start], &c[0][s.end() - start..])
                }
                None => "[redacted]".into(),
            })
            .into_owned();
    }
    out
}

/// Whether the text holds anything that looks like a credential.
pub fn has_secret(text: &str) -> bool {
    redact(text) != text
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
            Update { id: 10, chat_id: 42, private: true, text: "/status".into(), callback: None },
            Update { id: 11, chat_id: -7, private: false, text: "hi".into(), callback: None },
        ]);
        let press = json!([{"update_id": 20, "callback_query": {"id": "cb1", "from": {"id": 42}, "data": "/approve 7",
            "message": {"chat": {"id": 42, "type": "private"}}}}]);
        assert_eq!(parse_updates(&press).0, vec![Update { id: 20, chat_id: 42, private: true, text: "/approve 7".into(), callback: Some("cb1".into()) }]);
        assert_eq!(parse_updates(&json!([])), (vec![], None));
    }

    #[test]
    fn credentials_are_redacted_and_ordinary_text_is_not() {
        let bot = format!("1234567890:{}", "A".repeat(35));
        let leaks = [
            format!("token {bot}"),
            "key sk-ant-oat01-abcdefghijklmnop".to_string(),
            "GITHUB_TOKEN=ghx1234567890".to_string(),
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCY".to_string(),
            "PRIVATE_KEY = 'MIIEvQIBADANBgkqhkiG9w0BAQEFAASC'".to_string(),
            "api_key = \"a1b2c3d4e5f6g7h8\"".to_string(),
            "{\"password\": \"hunter2hunter2\"}".to_string(),
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123".to_string(),
            "DATABASE_URL=postgres://app:s3cretPassw0rd@db.internal:5432/app".to_string(),
            "openai sk-abcdefghijklmnopqrstuvwxyz0123".to_string(),
            "stripe sk_live_abcdefghijklmnop rk_live_abcdefghijklmnop".to_string(),
            "slack xapp-1-ABCDEFGHIJ-123 xoxa-2-abcdefghijkl".to_string(),
            "gh ghr_abcdefghijklmnopqrstuvwxyz0123".to_string(),
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U".to_string(),
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----".to_string(),
        ];
        for leak in &leaks {
            assert!(has_secret(leak), "missed: {leak}");
            let r = redact(leak);
            assert!(r.contains("[redacted]"), "{r}");
        }
        assert_eq!(redact("GITHUB_TOKEN=ghx1234567890"), "GITHUB_TOKEN=[redacted]");
        assert_eq!(redact("postgres://app:s3cretPassw0rd@db:5432"), "postgres://app:[redacted]@db:5432");
        for fine in [
            "5h 8% · resets 14:05, see src/main.rs:12",
            "primary_key=True",
            "API_KEY = os.environ['API_KEY']",
            "token = process.env.TOKEN",
            "password: <your password>",
            "SECRET_KEY=${SECRET_KEY}",
            "sk-ant-",
            "https://github.com/forhad-h/marsad",
        ] {
            assert!(!has_secret(fine), "flagged harmless text: {fine}");
        }
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
        drop(calls);
        bot.send_with_buttons(42, "Deploy?", &[("Approve".into(), "/approve 3".into())]).unwrap();
        let calls = bot.transport.calls.borrow();
        let markup = &calls[1].1.iter().find(|(k, _)| k == "reply_markup").unwrap().1;
        assert_eq!(markup, r#"{"inline_keyboard":[[{"text":"Approve","callback_data":"/approve 3"}]]}"#);
    }
}

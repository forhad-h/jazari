//! One worker run: a fresh headless `claude -p` inside the product's container, for one task.
//! The run streams JSON lines; the `rate_limit_event` lines give the usage windows, and the final
//! `result` line carries the structured outcome.

use super::limits::Limits;
use super::store::Task;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

pub const PERSONA: &str = include_str!("../../assets/jazari/personas/worker.md");

/// The shape of a worker's final answer.
pub const RESULT_SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","enum":["done","blocked","failed"]},"summary":{"type":"string"},"branch":{"type":"string"},"follow_ups":{"type":"array","maxItems":3,"items":{"type":"object","properties":{"role":{"type":"string"},"title":{"type":"string"}},"required":["role","title"]}},"needs_founder":{"type":"string"}},"required":["status","summary"]}"#;

/// Follow-up tasks a run may add; more are dropped, so the queue can't grow without end.
pub const MAX_FOLLOW_UPS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Done,
    Blocked,
    Failed,
    /// A usage window refused the run: it goes back in the queue without counting as a failure.
    Limited,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Done => "done",
            Kind::Blocked => "blocked",
            Kind::Failed => "failed",
            Kind::Limited => "limited",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub kind: Kind,
    pub summary: String,
    pub branch: Option<String>,
    pub follow_ups: Vec<(String, String)>,
    pub needs_founder: Option<String>,
    pub limits: Option<Limits>,
    pub session: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
}

impl Outcome {
    fn new(kind: Kind, summary: impl Into<String>) -> Self {
        Outcome {
            kind,
            summary: summary.into(),
            branch: None,
            follow_ups: Vec::new(),
            needs_founder: None,
            limits: None,
            session: None,
            input: 0,
            output: 0,
            cache_read: 0,
        }
    }
}

/// The branch a task's commits go on.
pub fn branch(task: &Task) -> String {
    format!("jazari/task-{}", task.id)
}

pub fn prompt(task: &Task) -> String {
    let brief = if task.brief.is_empty() { String::new() } else { format!("\n\n{}", task.brief) };
    format!(
        "Product: {}\nYour role: {}\nTask #{}: {}{brief}\n\nWork on the git branch `{}`. End with the structured result.",
        task.product,
        task.role,
        task.id,
        task.title,
        branch(task)
    )
}

/// The `claude` arguments for a task.
pub fn args(task: &Task, model: &str, max_turns: u32) -> Vec<String> {
    let mut a: Vec<String> = vec!["-p".into(), prompt(task)];
    a.extend(
        [
            "--output-format", "stream-json", "--verbose", "--model", model, "--max-turns", &max_turns.to_string(),
            "--permission-mode", "bypassPermissions", "--append-system-prompt", PERSONA, "--json-schema", RESULT_SCHEMA,
        ]
        .map(String::from),
    );
    a
}

/// Runs `claude` with these arguments in a container, handing each stdout line to `line`.
pub trait Runner {
    fn run(&self, container: &str, args: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<()>;
}

/// `docker exec` into the product's container. The Claude login token reaches docker through
/// its environment (`-e NAME` with no value), never through its arguments.
pub struct Docker {
    pub token: String,
}

impl Runner for Docker {
    fn run(&self, container: &str, args: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<()> {
        let git = [
            "GIT_AUTHOR_NAME=jazari", "GIT_AUTHOR_EMAIL=jazari@localhost", "GIT_COMMITTER_NAME=jazari", "GIT_COMMITTER_EMAIL=jazari@localhost",
            // The repo is mounted from the host, so its owner differs from the container user.
            "GIT_CONFIG_COUNT=1", "GIT_CONFIG_KEY_0=safe.directory", "GIT_CONFIG_VALUE_0=/work",
        ];
        let mut cmd = Command::new("docker");
        cmd.args(["exec", "-i", "-w", "/work", "-e", "CLAUDE_CODE_OAUTH_TOKEN"]);
        for g in git {
            cmd.args(["-e", g]);
        }
        cmd.arg(container).arg("claude").args(args);
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", &self.token).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        if let Some(out) = child.stdout.take() {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                line(&l);
            }
        }
        child.wait()?;
        Ok(())
    }
}

/// Reads a run's JSON lines into its outcome.
#[derive(Default)]
pub struct Reader {
    limits: Option<Limits>,
    result: Option<Value>,
    session: Option<String>,
}

impl Reader {
    pub fn line(&mut self, text: &str) {
        let Ok(v) = serde_json::from_str::<Value>(text) else { return };
        if let Some(l) = Limits::from_event(&v) {
            self.limits = Some(l);
        }
        if self.session.is_none() {
            self.session = v["session_id"].as_str().map(String::from);
        }
        if v["type"] == "result" {
            self.result = Some(v);
        }
    }

    pub fn outcome(self) -> Outcome {
        let limited = self.limits.is_some_and(|l| l.refused());
        let mut out = match &self.result {
            _ if limited => Outcome::new(Kind::Limited, "a usage limit refused the run"),
            None => Outcome::new(Kind::Failed, "the worker ended without a result"),
            Some(r) if r["is_error"] == true || r["subtype"] != "success" => {
                let why = r["result"].as_str().or(r["subtype"].as_str()).unwrap_or("unknown error");
                // A run refused for usage can end before any rate_limit_event arrives.
                let kind = if why.to_ascii_lowercase().contains("usage limit") || why.contains("rate limit") { Kind::Limited } else { Kind::Failed };
                Outcome::new(kind, crate::hook::truncate(why, 300))
            }
            Some(r) => {
                let s = &r["structured_output"];
                let kind = match s["status"].as_str() {
                    Some("done") => Kind::Done,
                    Some("blocked") => Kind::Blocked,
                    _ => Kind::Failed,
                };
                let mut o = Outcome::new(kind, crate::hook::truncate(s["summary"].as_str().unwrap_or("no summary"), 600));
                o.branch = s["branch"].as_str().filter(|b| !b.is_empty()).map(String::from);
                o.needs_founder = s["needs_founder"].as_str().filter(|n| !n.trim().is_empty()).map(|n| crate::hook::truncate(n, 600));
                o.follow_ups = s["follow_ups"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|f| Some((f["role"].as_str()?.to_string(), f["title"].as_str()?.trim().to_string())))
                    .filter(|(_, t)| !t.is_empty())
                    .take(MAX_FOLLOW_UPS)
                    .collect();
                o
            }
        };
        if let Some(r) = &self.result {
            let n = |k: &str| r["usage"][k].as_i64().unwrap_or(0);
            (out.input, out.output, out.cache_read) = (n("input_tokens") + n("cache_creation_input_tokens"), n("output_tokens"), n("cache_read_input_tokens"));
        }
        out.limits = self.limits;
        out.session = self.session;
        out
    }
}

/// Runs one task and reads its outcome. A worker that can't start is a failure, not an error.
pub fn run(runner: &dyn Runner, container: &str, task: &Task, model: &str, max_turns: u32) -> Outcome {
    let mut reader = Reader::default();
    match runner.run(container, &args(task, model, max_turns), &mut |l| reader.line(l)) {
        Ok(()) => reader.outcome(),
        Err(e) => Outcome::new(Kind::Failed, format!("the worker couldn't start: {e}")),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub fn task() -> Task {
        Task {
            id: 7,
            product: "shop".into(),
            role: "backend engineer".into(),
            title: "Add rate limiting to login".into(),
            brief: "5 tries per minute per IP.".into(),
            priority: 0,
            status: "queued".into(),
            attempts: 0,
            summary: String::new(),
            branch: None,
        }
    }

    pub fn limit_line(five: f64, status: &str) -> String {
        json!({"type": "rate_limit_event", "session_id": "s-1", "rate_limit_info": {"status": status, "rateLimitType": "five_hour",
            "unifiedWindows": {"five_hour": {"utilization": five, "resetsAt": 9e9, "status": status}}}})
        .to_string()
    }

    pub fn result_line(structured: Value) -> String {
        json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "s-1",
            "usage": {"input_tokens": 10, "cache_creation_input_tokens": 5, "output_tokens": 20, "cache_read_input_tokens": 300},
            "structured_output": structured})
        .to_string()
    }

    fn read(lines: &[String]) -> Outcome {
        let mut r = Reader::default();
        for l in lines {
            r.line(l);
        }
        r.outcome()
    }

    #[test]
    fn the_prompt_names_the_task_and_its_branch() {
        let a = args(&task(), "claude-sonnet-5-5", 30);
        assert!(a[1].contains("Task #7: Add rate limiting to login") && a[1].contains("jazari/task-7") && a[1].contains("5 tries"));
        let has = |k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
        assert!(has("--output-format", "stream-json") && has("--max-turns", "30") && has("--permission-mode", "bypassPermissions"));
        assert!(has("--json-schema", RESULT_SCHEMA) && serde_json::from_str::<Value>(RESULT_SCHEMA).is_ok());
    }

    #[test]
    fn a_finished_run_gives_its_result_usage_and_follow_ups() {
        let o = read(&[
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s-1\"}".into(),
            limit_line(0.2, "allowed"),
            "not json".into(),
            result_line(json!({"status": "done", "summary": "Added it.", "branch": "jazari/task-7",
                "follow_ups": [{"role": "qa", "title": "Load test login"}, {"role": "x", "title": " "}, {"role": "a", "title": "1"}, {"role": "b", "title": "2"}, {"role": "c", "title": "3"}]})),
        ]);
        assert_eq!(o.kind, Kind::Done);
        assert_eq!((o.summary.as_str(), o.branch.as_deref(), o.session.as_deref()), ("Added it.", Some("jazari/task-7"), Some("s-1")));
        assert_eq!(o.follow_ups.len(), MAX_FOLLOW_UPS);
        assert_eq!(o.follow_ups[0], ("qa".into(), "Load test login".into()));
        assert_eq!((o.input, o.output, o.cache_read), (15, 20, 300));
        assert_eq!(o.limits.unwrap().five_hour.unwrap().used_pct, 20.0);
    }

    #[test]
    fn refusals_errors_and_silence_are_told_apart() {
        assert_eq!(read(&[limit_line(1.0, "rejected")]).kind, Kind::Limited);
        let err = json!({"type": "result", "subtype": "success", "is_error": true, "result": "Claude usage limit reached. Resets 5pm"}).to_string();
        assert_eq!(read(&[err]).kind, Kind::Limited);
        let err = json!({"type": "result", "subtype": "error_max_turns", "is_error": false}).to_string();
        let o = read(&[err]);
        assert_eq!((o.kind, o.summary.as_str()), (Kind::Failed, "error_max_turns"));
        assert_eq!(read(&[]).kind, Kind::Failed);
        let blocked = read(&[result_line(json!({"status": "blocked", "summary": "Need a key.", "needs_founder": "Which SMS provider?"}))]);
        assert_eq!((blocked.kind, blocked.needs_founder.as_deref()), (Kind::Blocked, Some("Which SMS provider?")));
    }
}

//! One worker run: a fresh headless `claude -p` inside the product's container, for one task.
//! The run streams JSON lines; the `rate_limit_event` lines give the usage windows, and the final
//! `result` line carries the structured outcome.

use super::limits::Limits;
use super::store::Task;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// The shape of a worker's final answer.
pub const RESULT_SCHEMA: &str = r#"{"type":"object","properties":{"status":{"type":"string","enum":["done","blocked","failed"]},"summary":{"type":"string"},"verdict":{"type":"string","enum":["approve","changes"]},"score":{"type":"integer","minimum":1,"maximum":5},"criteria":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"pass":{"type":"boolean"},"evidence":{"type":"string"}},"required":["name","pass"]}},"follow_ups":{"type":"array","maxItems":8,"items":{"type":"object","properties":{"role":{"type":"string"},"title":{"type":"string"},"brief":{"type":"string"}},"required":["role","title"]}},"needs_founder":{"type":"string"},"approval_needed":{"type":"object","properties":{"kind":{"type":"string","enum":["deploy","spend","contact"]},"detail":{"type":"string"}},"required":["kind","detail"]}},"required":["status","summary"]}"#;

/// The longest a run may take before it is stopped, in seconds.
pub const RUN_TIMEOUT_SECS: u64 = 45 * 60;

/// A follow-up task a run asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct FollowUp {
    pub role: String,
    pub title: String,
    pub brief: String,
}

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
    pub verdict: Option<String>,
    /// The evaluator's overall score (1-5) and the criteria it failed.
    pub score: Option<i64>,
    pub failed: Vec<String>,
    pub follow_ups: Vec<FollowUp>,
    pub needs_founder: Option<String>,
    /// `(kind, detail)`: deploy, spend or contact.
    pub approval: Option<(String, String)>,
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
            verdict: None,
            score: None,
            failed: Vec::new(),
            follow_ups: Vec::new(),
            needs_founder: None,
            approval: None,
            limits: None,
            session: None,
            input: 0,
            output: 0,
            cache_read: 0,
        }
    }
}

/// The branch a task's commits go on: its own, or the one it reviews or fixes.
pub fn branch(task: &Task) -> String {
    task.branch.clone().unwrap_or_else(|| format!("jazari/task-{}", task.id))
}

pub fn prompt(task: &Task, base: &str) -> String {
    let brief = if task.brief.is_empty() { String::new() } else { format!("\n\n{}", task.brief) };
    let what = match task.kind.as_str() {
        "eval" | "review" | "security" => format!("You are reviewing the branch `{}` against `{base}`.", branch(task)),
        _ => format!("You are on the branch `{}`, made from `{base}`.", branch(task)),
    };
    format!("Product: {}\nTask #{}: {}{brief}\n\n{what} End with the structured result.", task.product, task.id, task.title)
}

/// What a run needs besides the task.
pub struct Setup<'a> {
    pub container: &'a str,
    pub workdir: &'a str,
    pub base: &'a str,
    pub model: &'a str,
    pub max_turns: u32,
    pub system_prompt: &'a str,
    pub max_follow_ups: usize,
    /// A judge: it may read and run read-only git commands, never write.
    pub read_only: bool,
}

/// What a read-only judge may use.
const READ_ONLY_TOOLS: &str = "Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*),Bash(git status:*)";

/// The command run in the container: `claude` under a time limit.
pub fn args(task: &Task, s: &Setup) -> Vec<String> {
    let mut a: Vec<String> = ["timeout", "--kill-after=30", &RUN_TIMEOUT_SECS.to_string(), "claude", "-p"].map(String::from).to_vec();
    a.push(prompt(task, s.base));
    a.extend(["--output-format", "stream-json", "--verbose", "--model", s.model, "--max-turns", &s.max_turns.to_string()].map(String::from));
    if s.read_only {
        // In print mode a tool that isn't allowed is refused, never asked about.
        a.extend(["--permission-mode", "default", "--allowedTools", READ_ONLY_TOOLS, "--disallowedTools", "Write,Edit,NotebookEdit"].map(String::from));
    } else {
        a.extend(["--permission-mode", "bypassPermissions"].map(String::from));
    }
    a.extend(["--append-system-prompt", s.system_prompt, "--json-schema", RESULT_SCHEMA].map(String::from));
    a
}

/// Runs a command in a container folder, handing each stdout line to `line`; returns its exit
/// code.
pub trait Runner {
    fn run(&self, container: &str, workdir: &str, command: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<i32>;
}

/// `docker exec` into the product's container. The Claude login token reaches docker through
/// its environment (`-e NAME` with no value), never through its arguments.
pub struct Docker {
    pub token: String,
}

impl Runner for Docker {
    fn run(&self, container: &str, workdir: &str, command: &[String], line: &mut dyn FnMut(&str)) -> anyhow::Result<i32> {
        let git = [
            "GIT_AUTHOR_NAME=jazari", "GIT_AUTHOR_EMAIL=jazari@localhost", "GIT_COMMITTER_NAME=jazari", "GIT_COMMITTER_EMAIL=jazari@localhost",
            // The repo is mounted from the host, so its owner differs from the container user.
            "GIT_CONFIG_COUNT=1", "GIT_CONFIG_KEY_0=safe.directory", "GIT_CONFIG_VALUE_0=*",
        ];
        let mut cmd = Command::new("docker");
        cmd.args(["exec", "-i", "-w", workdir, "-e", "CLAUDE_CODE_OAUTH_TOKEN"]);
        for g in git {
            cmd.args(["-e", g]);
        }
        cmd.arg(container).args(command);
        // stderr joins stdout for checks (`2>&1` is in their command); a worker's stderr is noise.
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", &self.token).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        if let Some(out) = child.stdout.take() {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                line(&l);
            }
        }
        Ok(child.wait()?.code().unwrap_or(-1))
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

    pub fn outcome(self, max_follow_ups: usize) -> Outcome {
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
                o.verdict = s["verdict"].as_str().map(String::from);
                o.score = s["score"].as_i64().filter(|n| (1..=5).contains(n));
                o.failed = s["criteria"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter(|c| c["pass"] == false)
                    .filter_map(|c| c["name"].as_str().map(|n| crate::hook::truncate(n, 80)))
                    .collect();
                o.needs_founder = s["needs_founder"].as_str().filter(|n| !n.trim().is_empty()).map(|n| crate::hook::truncate(n, 600));
                let a = &s["approval_needed"];
                o.approval = a["kind"].as_str().filter(|k| ["deploy", "spend", "contact"].contains(k)).map(|k| (k.to_string(), crate::hook::truncate(a["detail"].as_str().unwrap_or(""), 600)));
                o.follow_ups = s["follow_ups"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|f| {
                        Some(FollowUp {
                            role: f["role"].as_str()?.trim().to_string(),
                            title: crate::hook::truncate(f["title"].as_str()?, 200),
                            brief: crate::hook::truncate(f["brief"].as_str().unwrap_or(""), 2000),
                        })
                    })
                    .filter(|f| !f.title.is_empty() && !f.role.is_empty())
                    .take(max_follow_ups)
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
pub fn run(runner: &dyn Runner, task: &Task, s: &Setup) -> Outcome {
    let mut reader = Reader::default();
    match runner.run(s.container, s.workdir, &args(task, s), &mut |l| reader.line(l)) {
        Ok(_) => reader.outcome(s.max_follow_ups),
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
            kind: "work".into(),
            parent: None,
            owner_role: None,
            round: 0,
            question: None,
            approval: None,
            after_task: None,
            spec: "5 tries per minute per IP.".into(),
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
        r.outcome(3)
    }

    #[test]
    fn the_prompt_names_the_task_and_its_branch() {
        let mut setup = Setup { container: "c", workdir: "/w", base: "main", model: "claude-sonnet-5-5", max_turns: 30, system_prompt: "rules", max_follow_ups: 3, read_only: false };
        let a = args(&task(), &setup);
        setup.read_only = true;
        let judge = args(&task(), &setup);
        let has_j = |k: &str, v: &str| judge.windows(2).any(|w| w[0] == k && w[1] == v);
        assert!(has_j("--allowedTools", READ_ONLY_TOOLS) && has_j("--disallowedTools", "Write,Edit,NotebookEdit") && !judge.contains(&"bypassPermissions".to_string()));
        assert_eq!(a[..5], ["timeout", "--kill-after=30", "2700", "claude", "-p"]);
        assert!(a[5].contains("Task #7: Add rate limiting to login") && a[5].contains("jazari/task-7") && a[5].contains("5 tries"));
        let mut review = task();
        (review.kind, review.branch) = ("review".into(), Some("jazari/task-3".into()));
        assert!(prompt(&review, "main").contains("reviewing the branch `jazari/task-3` against `main`"));
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
            result_line(json!({"status": "done", "summary": "Added it.", "verdict": "approve", "score": 4,
                "criteria": [{"name": "Scope", "pass": true}, {"name": "Tests prove the behavior", "pass": false, "evidence": "no test"}],
                "follow_ups": [{"role": "qa", "title": "Load test login", "brief": "100 rps"}, {"role": "x", "title": " "}, {"role": "a", "title": "1"}, {"role": "b", "title": "2"}, {"role": "c", "title": "3"}]})),
        ]);
        assert_eq!(o.kind, Kind::Done);
        assert_eq!((o.summary.as_str(), o.verdict.as_deref(), o.session.as_deref()), ("Added it.", Some("approve"), Some("s-1")));
        assert_eq!(o.follow_ups.len(), 3);
        assert_eq!((o.score, o.failed.clone()), (Some(4), vec!["Tests prove the behavior".to_string()]));
        assert_eq!(o.follow_ups[0], FollowUp { role: "qa".into(), title: "Load test login".into(), brief: "100 rps".into() });
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
        let ask = read(&[result_line(json!({"status": "blocked", "summary": "Ready to ship.", "approval_needed": {"kind": "deploy", "detail": "Deploy v1 to fly.io"}}))]);
        assert_eq!(ask.approval, Some(("deploy".into(), "Deploy v1 to fly.io".into())));
        let odd = read(&[result_line(json!({"status": "blocked", "summary": "x", "approval_needed": {"kind": "rm -rf", "detail": "y"}}))]);
        assert_eq!(odd.approval, None);
    }
}

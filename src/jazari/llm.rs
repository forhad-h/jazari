//! Small one-shot Claude calls on the host for jazari's own bookkeeping: calibrating the judges,
//! learning the founder's preferences, and drawing lessons from rework. Each call has no tools,
//! saves no session, is hidden from the dashboard, returns structured output and is stopped after
//! a timeout.

use serde_json::Value;
use std::io::{Read, Write};

/// One call: the instructions, the input, the shape of the answer, the model and the effort.
pub struct Ask<'a> {
    pub system: &'a str,
    pub prompt: &'a str,
    pub schema: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    pub timeout: std::time::Duration,
}

/// The small model, for cheap bookkeeping.
pub const SMALL: &str = "claude-haiku-5-5";

pub fn claude(a: &Ask) -> anyhow::Result<Value> {
    let program = std::env::var_os("MARSAD_CLAUDE").filter(|p| !p.is_empty()).unwrap_or_else(|| "claude".into());
    let args = [
        "-p", "--no-session-persistence", "--tools", "", "--max-turns", "3", "--model", a.model, "--effort", a.effort, "--output-format", "json",
        "--json-schema", a.schema, "--append-system-prompt", a.system,
    ];
    let mut child = std::process::Command::new(program)
        .args(args)
        .env(crate::prompt_gen::MARKER_ENV, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    child.stdin.take().expect("stdin is piped").write_all(a.prompt.as_bytes())?;
    // Read stdout on a thread so a full pipe can't stall the wait below.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let started = std::time::Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if started.elapsed() > a.timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Claude took longer than {} seconds", a.timeout.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let out = reader.join().unwrap_or_default();
    let v: Value = serde_json::from_slice(&out).map_err(|_| anyhow::anyhow!("Claude gave no JSON (is `claude` installed and logged in?)"))?;
    anyhow::ensure!(v["is_error"] != true, "Claude failed: {}", v["result"].as_str().unwrap_or("unknown error"));
    let s = &v["structured_output"];
    anyhow::ensure!(s.is_object(), "Claude gave no structured answer");
    Ok(s.clone())
}

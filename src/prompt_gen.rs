//! Writing a library prompt from a rough idea: one isolated `claude -p` call that returns a draft
//! as structured JSON, with what it assumed and what it left for the author to decide.
//!
//! The run carries none of the user's context: `--safe-mode` skips CLAUDE.md, hooks, plugins and
//! MCP servers, the working directory is marsad's own `generator/` folder, and
//! `--no-session-persistence` leaves no transcript. `MARKER_ENV` is set as well, so `marsad hook`
//! drops the run's events should a hook still fire (a managed plugin's, say), and it never shows
//! on the dashboard. `claude` reads only the last `--append-system-prompt-file`, so the
//! generator's instructions and the output hygiene rules go in one merged file.

use crate::share::{Captured, RunError, run_captured};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// The strongest model alias Claude Code accepts (`claude --help` lists fable, opus, sonnet).
pub const DEFAULT_MODEL: &str = "fable";

/// Tried when the configured alias is rejected, before the session default.
const FALLBACK_MODEL: &str = "opus";

/// Set on the `claude` run, so `marsad hook` ignores its events.
pub const MARKER_ENV: &str = "MARSAD_GENERATOR";

/// Overrides the `claude` program, for a stub in tests or a non-standard install.
const PROGRAM_ENV: &str = "MARSAD_CLAUDE";

/// The longest a run may take before it is killed. A strong model at high effort can take minutes.
const WAIT: Duration = Duration::from_secs(10 * 60);

const INSTRUCTIONS: &str = include_str!("../assets/PROMPT-GENERATOR.md");
const OUTPUT_HYGIENE: &str = include_str!("../assets/OUTPUT-HYGIENE.md");

/// The shape the reply must take.
pub const SCHEMA: &str = r#"{"type":"object","required":["key","title","summary","body","assumptions","questions"],"properties":{"key":{"type":"string","pattern":"^[a-z0-9][a-z0-9-]{1,40}$"},"title":{"type":"string"},"summary":{"type":"string","description":"one line, at most 80 characters"},"body":{"type":"string","description":"the whole file, starting with '# <title>'"},"assumptions":{"type":"array","items":{"type":"string"}},"questions":{"type":"array","items":{"type":"string"}}}}"#;

/// A generated prompt and the decisions it leaves open.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Draft {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub body: String,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub questions: Vec<String>,
}

impl Draft {
    /// A draft of an existing prompt, for revising it: nothing assumed, nothing asked.
    pub fn of(rule: &crate::rules::Rule) -> Self {
        Draft { key: rule.key.clone(), body: rule.body().to_string(), ..Draft::default() }.normalized()
    }

    /// `normalized`, for a body the user edited: the title and summary follow the new text.
    pub fn normalized_keeping_key(self) -> Self {
        Draft { title: String::new(), summary: String::new(), ..self }.normalized()
    }

    /// The body with a `# <title>` first line, and the title and summary as the library will read
    /// them back from it. A key that isn't well formed is replaced by the title's slug.
    pub fn normalized(mut self) -> Self {
        let body = self.body.trim().to_string();
        let title = self.title.trim().to_string();
        self.body = if body.starts_with("# ") || title.is_empty() { body } else { format!("# {title}\n\n{body}") };
        if let Some((t, s)) = crate::rules::title_and_summary(&self.key, &self.body) {
            (self.title, self.summary) = (t, s);
        }
        if crate::rules::key_problem(&[], &self.key, None).is_some() {
            self.key = crate::rules::slug(&self.title);
        }
        self.assumptions.retain(|a| !a.trim().is_empty());
        self.questions.retain(|q| !q.trim().is_empty());
        self
    }
}

/// What to write: the rough idea, an optional name, and when revising, the draft and the feedback.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub idea: String,
    pub name: String,
    /// The body of the draft to revise.
    pub previous: Option<String>,
    pub feedback: String,
}

/// The user message: the request's parts, each in its own tags, sent on stdin.
pub fn stdin_text(req: &Request) -> String {
    let mut out = String::new();
    let mut tag = |name: &str, text: &str| {
        if !text.trim().is_empty() {
            out += &format!("<{name}>\n{}\n</{name}>\n", text.trim());
        }
    };
    tag("rough_idea", &req.idea);
    tag("preferred_name", &req.name);
    if let Some(previous) = &req.previous {
        tag("previous_draft", previous);
        tag("feedback", &req.feedback);
    }
    out
}

/// The generator's system prompt: its instructions, then the output hygiene rules.
pub fn system_text() -> String {
    format!("{}\n\n{}\n", INSTRUCTIONS.trim_end(), OUTPUT_HYGIENE.trim_end())
}

/// The neutral working directory for the run, so no project's CLAUDE.md is in reach.
fn generator_dir(data: &Path) -> PathBuf {
    let dir = data.join("generator");
    std::path::absolute(&dir).unwrap_or(dir)
}

/// Writes the system prompt to `<data>/generator/<hash>.md` and returns its path.
fn prompt_file(data: &Path) -> anyhow::Result<PathBuf> {
    let text = system_text();
    let path = generator_dir(data).join(format!("{:016x}.md", crate::rules::fnv1a(text.as_bytes())));
    crate::rules::write_if_changed(&path, &text)?;
    Ok(path)
}

/// The arguments for one run; without a model, Claude Code's default is used.
pub fn args(model: Option<&str>, prompt_file: &Path) -> Vec<String> {
    let mut args: Vec<String> =
        ["-p", "--safe-mode", "--no-session-persistence", "--tools", "", "--max-turns", "1"].map(String::from).to_vec();
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        args.extend(["--model".into(), model.trim().into()]);
    }
    args.extend(["--effort", "high", "--output-format", "json", "--json-schema", SCHEMA].map(String::from));
    args.extend(["--append-system-prompt-file".into(), prompt_file.to_string_lossy().into_owned()]);
    args
}

/// Why a run produced no draft.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// The model alias isn't available to this account or version.
    ModelRejected,
    Other(String),
}

/// The draft in a `claude -p --output-format json` reply: `structured_output`, or else the JSON in
/// `result`.
fn parse(out: &Captured) -> Result<Draft, Failure> {
    let json: Option<serde_json::Value> = serde_json::from_slice(&out.stdout).ok();
    let result = json.as_ref().and_then(|v| v["result"].as_str()).unwrap_or("");
    if let Some(v) = json.as_ref().filter(|v| v["is_error"].as_bool() == Some(true)) {
        if v["api_error_status"].as_u64() == Some(404) || result.contains("selected model") {
            return Err(Failure::ModelRejected);
        }
        let why = first_line(result).or_else(|| v["subtype"].as_str()).unwrap_or("no details");
        return Err(Failure::Other(format!("claude returned an error: {why}")));
    }
    if !out.success {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("unknown option") {
            return Err(Failure::Other("your Claude Code is too old for this; update it with `claude update`".into()));
        }
        return Err(Failure::Other(format!("claude failed: {}", first_line(&stderr).unwrap_or("non-zero exit"))));
    }
    let json = json.as_ref().ok_or_else(|| Failure::Other("claude's output wasn't JSON".into()))?;
    let value = match &json["structured_output"] {
        v @ serde_json::Value::Object(_) => v.clone(),
        _ => embedded_json(result).ok_or_else(|| Failure::Other("claude's reply held no draft".into()))?,
    };
    let draft: Draft = serde_json::from_value(value).map_err(|e| Failure::Other(format!("claude's draft was malformed: {e}")))?;
    if draft.body.trim().is_empty() {
        return Err(Failure::Other("claude returned an empty prompt".into()));
    }
    Ok(draft.normalized())
}

/// The JSON object in `text`, which may be wrapped in a code fence or a sentence.
fn embedded_json(text: &str) -> Option<serde_json::Value> {
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    serde_json::from_str(text.get(start..=end)?).ok().filter(serde_json::Value::is_object)
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

/// The `claude` to run: `MARSAD_CLAUDE`, or `claude` on the PATH.
fn program() -> std::ffi::OsString {
    std::env::var_os(PROGRAM_ENV).filter(|p| !p.is_empty()).unwrap_or_else(|| "claude".into())
}

/// Writes a draft with the real `claude` and the configured model.
pub fn generate(req: &Request, cancel: &AtomicBool) -> Result<Draft, String> {
    generate_with(&program(), &crate::settings::generator_model(), &crate::data_dir(), req, WAIT, cancel)
}

/// Runs `program` for `req`. When `model` is rejected, tries the fallback alias, then the session
/// default.
fn generate_with(program: &std::ffi::OsStr, model: &str, data: &Path, req: &Request, wait: Duration, cancel: &AtomicBool) -> Result<Draft, String> {
    let file = prompt_file(data).map_err(|e| format!("couldn't write the generator's prompt: {e:#}"))?;
    let input = stdin_text(req);
    let mut models = vec![Some(model)];
    if model != FALLBACK_MODEL {
        models.push(Some(FALLBACK_MODEL));
    }
    models.push(None);
    for m in models {
        let mut cmd = Command::new(program);
        cmd.args(args(m, &file)).current_dir(generator_dir(data)).env(MARKER_ENV, "1");
        let out = run_captured(&mut cmd, input.as_bytes(), wait, cancel).map_err(|e| match e {
            RunError::NotFound => format!("Claude Code (`{}`) was not found", program.to_string_lossy()),
            RunError::Spawn(why) => format!("couldn't run claude: {why}"),
            RunError::Timeout => format!("claude did not finish within {} minutes", wait.as_secs() / 60),
            RunError::Cancelled => "cancelled".into(),
        })?;
        match parse(&out) {
            Ok(draft) => return Ok(draft),
            Err(Failure::ModelRejected) => continue,
            Err(Failure::Other(why)) => return Err(why),
        }
    }
    Err("claude rejected every model it was offered".into())
}

// ---------- `marsad prompt` ----------

#[derive(clap::Subcommand)]
pub enum PromptCmd {
    /// Draft a prompt from a rough idea with Claude, review it, and save it to your library.
    New(NewArgs),
    /// Open one of your prompts in $EDITOR, or revise it with Claude (--with).
    Edit(EditArgs),
    /// Save a copy of a prompt, such as a built-in one, to your library under a new name.
    Copy(CopyArgs),
    /// Delete one of your prompts.
    Rm(RmArgs),
}

#[derive(clap::Args)]
pub struct NewArgs {
    /// The rough idea, e.g. "make Claude stop over-explaining in PRs". Without it, read from --from or stdin.
    idea: Vec<String>,
    /// Read the idea from this file (`-` for stdin).
    #[arg(long, value_name = "FILE")]
    from: Option<PathBuf>,
    /// What to call the prompt; its key is made from this.
    #[arg(long)]
    name: Option<String>,
    /// Save under this key instead of the suggested one.
    #[arg(long)]
    key: Option<String>,
    /// Save without reviewing (needed when not in a terminal).
    #[arg(long, short)]
    yes: bool,
    #[arg(long, value_enum, default_value = "table")]
    format: crate::report::Format,
}

#[derive(clap::Args)]
pub struct EditArgs {
    key: String,
    /// Revise the prompt with Claude using this feedback, instead of opening an editor.
    #[arg(long, value_name = "FEEDBACK")]
    with: Option<String>,
    /// Save the revision without reviewing it.
    #[arg(long, short)]
    yes: bool,
}

#[derive(clap::Args)]
pub struct CopyArgs {
    key: String,
    /// The copy's key (default: `<key>-copy`).
    #[arg(long = "as", value_name = "KEY")]
    new_key: Option<String>,
}

#[derive(clap::Args)]
pub struct RmArgs {
    key: String,
}

pub fn run(cmd: PromptCmd) -> anyhow::Result<()> {
    match cmd {
        PromptCmd::New(args) => run_new(args),
        PromptCmd::Edit(args) => run_edit(args),
        PromptCmd::Copy(args) => run_copy(args),
        PromptCmd::Rm(args) => {
            let path = crate::rules::remove_own(&args.key)?;
            println!("Deleted {}", path.display());
            Ok(())
        }
    }
}

/// The rough idea from the arguments, `--from`, or stdin, and whether it used stdin.
fn read_idea(args: &NewArgs) -> anyhow::Result<(String, bool)> {
    use std::io::{IsTerminal, Read};
    if !args.idea.is_empty() {
        return Ok((args.idea.join(" "), false));
    }
    let stdin = match &args.from {
        Some(path) if path.as_os_str() != "-" => {
            let text = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
            return Ok((text, false));
        }
        Some(_) => true,
        None => !std::io::stdin().is_terminal(),
    };
    anyhow::ensure!(stdin, "give the rough idea as an argument, with --from <file>, or on stdin");
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    Ok((text, true))
}

fn run_new(args: NewArgs) -> anyhow::Result<()> {
    let (idea, used_stdin) = read_idea(&args)?;
    anyhow::ensure!(!idea.trim().is_empty(), "the rough idea is empty");
    let lib = crate::rules::library();
    if let Some(why) = args.key.as_deref().and_then(|k| crate::rules::key_problem(&lib, k, None)) {
        anyhow::bail!("can't use that key: {why}");
    }
    let req = Request { idea, name: args.name.clone().unwrap_or_default(), ..Request::default() };
    let draft = cli_generate(&req)?;
    let preferred = args.name.as_deref().unwrap_or(&draft.key);
    let key = args.key.clone().unwrap_or_else(|| crate::rules::suggest_key(&lib, preferred, &draft.title));
    let interactive = !args.yes && !used_stdin && !matches!(args.format, crate::report::Format::Json) && std::io::IsTerminal::is_terminal(&std::io::stdin());
    finish(draft, key, None, req, args.yes, interactive, args.format)
}

fn run_edit(args: EditArgs) -> anyhow::Result<()> {
    let lib = crate::rules::library();
    let rule = lib.iter().find(|r| r.key == args.key).ok_or_else(|| anyhow::anyhow!("no prompt named {:?}", args.key))?;
    anyhow::ensure!(
        rule.own,
        "{} is built in and can't be edited; `marsad prompt copy {}` saves a copy you can edit",
        rule.key,
        rule.key
    );
    let Some(feedback) = args.with else {
        let edited = crate::share_view::edit_in_editor(rule.body()).map_err(|e| anyhow::anyhow!(e))?;
        if edited.trim() == rule.body().trim() {
            println!("No changes.");
            return Ok(());
        }
        let path = crate::rules::save_own(&rule.key, &edited, Some(&rule.key))?;
        println!("Saved {}", path.display());
        return Ok(());
    };
    let req = Request { idea: rule.body().to_string(), previous: Some(rule.body().to_string()), feedback, ..Request::default() };
    let draft = cli_generate(&req)?;
    let interactive = !args.yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    finish(draft, rule.key.clone(), Some(rule.key.clone()), req, args.yes, interactive, crate::report::Format::Table)
}

fn run_copy(args: CopyArgs) -> anyhow::Result<()> {
    let lib = crate::rules::library();
    let rule = lib.iter().find(|r| r.key == args.key).ok_or_else(|| anyhow::anyhow!("no prompt named {:?}", args.key))?;
    let key = args.new_key.unwrap_or_else(|| crate::rules::suggest_key(&lib, &format!("{}-copy", rule.key), &rule.title));
    let path = crate::rules::save_own(&key, rule.body(), None)?;
    println!("Saved a copy of {} as {key}: {}", rule.key, path.display());
    println!("Edit it with `marsad prompt edit {key}`.");
    Ok(())
}

fn cli_generate(req: &Request) -> anyhow::Result<Draft> {
    eprintln!("Writing the prompt with Claude ({}); this can take a minute or two…", crate::settings::generator_model());
    generate(req, &AtomicBool::new(false)).map_err(|e| anyhow::anyhow!(e))
}

/// Shows the draft, then saves it: at once with `--yes`, after a review loop in a terminal, and
/// otherwise not at all.
fn finish(mut draft: Draft, mut key: String, replacing: Option<String>, mut req: Request, yes: bool, interactive: bool, format: crate::report::Format) -> anyhow::Result<()> {
    use std::io::Write;
    loop {
        if !matches!(format, crate::report::Format::Json) {
            println!("{}", render_draft(&draft, &key));
        }
        if yes || !interactive {
            let saved = if yes { Some(crate::rules::save_own(&key, &draft.body, replacing.as_deref())?) } else { None };
            match format {
                crate::report::Format::Json => {
                    let mut v = serde_json::to_value(&draft)?;
                    v["key"] = key.clone().into();
                    v["saved"] = saved.as_ref().map(|p| p.display().to_string()).into();
                    println!("{}", serde_json::to_string_pretty(&v)?);
                }
                _ => match &saved {
                    Some(path) => println!("Saved {}", path.display()),
                    None => println!("Not saved. Run again with --yes to save it, or in a terminal to review it."),
                },
            }
            return Ok(());
        }
        print!("[s]ave  [e]dit  [r]evise with feedback  [n]ame  [q]uit: ");
        std::io::stdout().flush()?;
        match read_line()?.trim() {
            "s" | "S" | "" => {
                let path = crate::rules::save_own(&key, &draft.body, replacing.as_deref())?;
                println!("Saved {}", path.display());
                return Ok(());
            }
            "e" | "E" => match crate::share_view::edit_in_editor(&draft.body) {
                Ok(text) if !text.trim().is_empty() => draft = Draft { body: text, ..draft }.normalized_keeping_key(),
                Ok(_) => eprintln!("The edited prompt was empty, so it was not used."),
                Err(why) => eprintln!("Couldn't edit the prompt: {why}"),
            },
            "r" | "R" => {
                print!("What should change? ");
                std::io::stdout().flush()?;
                let feedback = read_line()?;
                if feedback.trim().is_empty() {
                    continue;
                }
                (req.previous, req.feedback) = (Some(draft.body.clone()), feedback.trim().to_string());
                match cli_generate(&req) {
                    Ok(d) => draft = d,
                    Err(e) => eprintln!("{e:#}"),
                }
            }
            "n" | "N" => {
                print!("Key [{key}]: ");
                std::io::stdout().flush()?;
                let new = read_line()?.trim().to_string();
                match crate::rules::key_problem(&crate::rules::library(), &new, replacing.as_deref()) {
                    _ if new.is_empty() => {}
                    Some(why) => eprintln!("can't use {new:?}: {why}"),
                    None => key = new,
                }
            }
            "q" | "Q" => {
                println!("Discarded.");
                return Ok(());
            }
            _ => {}
        }
    }
}

fn read_line() -> anyhow::Result<String> {
    let mut line = String::new();
    anyhow::ensure!(std::io::stdin().read_line(&mut line)? > 0, "stdin closed");
    Ok(line)
}

/// The draft as the CLI prints it: the file, what was assumed, the open questions and where it goes.
fn render_draft(d: &Draft, key: &str) -> String {
    let rule = "─".repeat(60);
    let mut out = format!("{rule}\n{}\n{rule}\n", d.body.trim_end());
    let list = |out: &mut String, heading: &str, items: &[String], numbered: bool| {
        if !items.is_empty() {
            *out += &format!("\n{heading}\n");
            for (i, item) in items.iter().enumerate() {
                *out += &if numbered { format!("  {}. {}\n", i + 1, item.trim()) } else { format!("  - {}\n", item.trim()) };
            }
        }
    };
    list(&mut out, "What I assumed:", &d.assumptions, false);
    list(&mut out, "Questions for you (answer with [r]evise):", &d.questions, true);
    out + &format!("\nSaves as {} ({})", key, crate::rules::prompts_dir().join(format!("{key}.md")).display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(stdout: &str) -> Captured {
        Captured { success: true, stdout: stdout.as_bytes().to_vec(), stderr: Vec::new() }
    }

    fn reply(draft: serde_json::Value) -> String {
        serde_json::json!({ "type": "result", "is_error": false, "result": "", "structured_output": draft }).to_string()
    }

    fn sample() -> serde_json::Value {
        serde_json::json!({
            "key": "short-pull-requests",
            "title": "Short pull requests",
            "summary": "Keeps pull request descriptions short and to the point.",
            "body": "# Short pull requests\n\nKeeps pull request descriptions short and to the point.\n\n- Keep descriptions under 150 words.\n",
            "assumptions": ["Applies to descriptions, not commit messages."],
            "questions": ["Should commit messages follow the same limit?", " "]
        })
    }

    #[test]
    fn the_command_is_isolated_single_turn_and_structured() {
        let file = Path::new("/d/generator/abc.md");
        let a = args(Some("fable"), file);
        let has = |flag: &str, value: &str| a.windows(2).any(|w| w[0] == flag && w[1] == value);
        assert_eq!(a[0], "-p");
        for flag in ["--safe-mode", "--no-session-persistence"] {
            assert!(a.iter().any(|x| x == flag), "{flag} missing: {a:?}");
        }
        assert!(has("--tools", "") && has("--max-turns", "1") && has("--model", "fable") && has("--effort", "high"));
        assert!(has("--output-format", "json") && has("--json-schema", SCHEMA));
        assert_eq!(a[a.len() - 2..], ["--append-system-prompt-file".to_string(), "/d/generator/abc.md".into()], "the only, so last, prompt file");
        assert!(!a.iter().any(|x| x == "--bare"), "--bare needs an API key");
        assert!(!args(None, file).iter().any(|x| x == "--model"), "no model: the session default");
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(schema["required"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn the_idea_and_the_revision_go_in_their_own_tags() {
        let first = stdin_text(&Request { idea: "  stop over-explaining in PRs \n".into(), ..Request::default() });
        assert_eq!(first, "<rough_idea>\nstop over-explaining in PRs\n</rough_idea>\n");
        let again = stdin_text(&Request {
            idea: "idea".into(),
            name: "Short PRs".into(),
            previous: Some("# Old\n".into()),
            feedback: "commits too".into(),
        });
        assert_eq!(
            again,
            "<rough_idea>\nidea\n</rough_idea>\n<preferred_name>\nShort PRs\n</preferred_name>\n<previous_draft>\n# Old\n</previous_draft>\n<feedback>\ncommits too\n</feedback>\n"
        );
        let no_previous = stdin_text(&Request { idea: "x".into(), feedback: "ignored".into(), ..Request::default() });
        assert!(!no_previous.contains("feedback"), "feedback only revises a draft");
    }

    #[test]
    fn the_system_prompt_is_the_generator_then_output_hygiene() {
        let text = system_text();
        assert!(text.starts_with("# Writing a standing prompt"));
        let (g, h) = (text.find("# Writing a standing prompt").unwrap(), text.find("# Output hygiene").unwrap());
        assert!(g < h);
        assert!(text.trim_end().ends_with(OUTPUT_HYGIENE.trim_end()));
        let data = std::env::temp_dir().join(format!("marsad-gen-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let path = prompt_file(&data).unwrap();
        assert!(path.is_absolute() && path.starts_with(generator_dir(&data)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert_eq!(prompt_file(&data).unwrap(), path, "the same text reuses its file");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn structured_output_is_read_first() {
        let d = parse(&captured(&reply(sample()))).unwrap();
        assert_eq!((d.key.as_str(), d.title.as_str()), ("short-pull-requests", "Short pull requests"));
        assert_eq!(d.summary, "Keeps pull request descriptions short and to the point.");
        assert_eq!(d.questions, ["Should commit messages follow the same limit?"], "blank questions are dropped");
        assert_eq!(d.assumptions.len(), 1);
    }

    #[test]
    fn the_result_text_is_the_fallback() {
        let text = format!("Here it is:\n```json\n{}\n```", sample());
        let out = serde_json::json!({ "type": "result", "is_error": false, "result": text }).to_string();
        assert_eq!(parse(&captured(&out)).unwrap().key, "short-pull-requests");
        let none = serde_json::json!({ "type": "result", "is_error": false, "result": "I can't help with that." }).to_string();
        assert_eq!(parse(&captured(&none)), Err(Failure::Other("claude's reply held no draft".into())));
    }

    #[test]
    fn bad_output_and_errors_are_explained() {
        assert_eq!(parse(&captured("not json")), Err(Failure::Other("claude's output wasn't JSON".into())));
        let err = serde_json::json!({ "is_error": true, "result": "Credit balance is too low\nmore" }).to_string();
        assert_eq!(parse(&captured(&err)), Err(Failure::Other("claude returned an error: Credit balance is too low".into())));
        let model = serde_json::json!({ "is_error": true, "api_error_status": 404, "result": "There's an issue with the selected model (x)." });
        assert_eq!(parse(&captured(&model.to_string())), Err(Failure::ModelRejected));
        let failed = Captured { success: false, stdout: Vec::new(), stderr: b"error: unknown option '--safe-mode'\n".to_vec() };
        assert!(matches!(parse(&failed), Err(Failure::Other(why)) if why.contains("too old")));
        let empty = reply(serde_json::json!({ "key": "x", "title": "x", "summary": "", "body": "  ", "assumptions": [], "questions": [] }));
        assert_eq!(parse(&captured(&empty)), Err(Failure::Other("claude returned an empty prompt".into())));
        let wrong = reply(serde_json::json!({ "key": "x", "body": 3 }));
        assert!(matches!(parse(&captured(&wrong)), Err(Failure::Other(why)) if why.contains("malformed")));
    }

    #[test]
    fn drafts_read_back_the_way_the_library_reads_them() {
        let d = Draft {
            key: "Bad Key!".into(),
            title: "Plain prose".into(),
            summary: "ignored, the body's first line wins".into(),
            body: "Write prose paragraphs.\n\n- One idea per paragraph.".into(),
            ..Draft::default()
        }
        .normalized();
        assert!(d.body.starts_with("# Plain prose\n\nWrite prose paragraphs."), "a heading is added: {}", d.body);
        assert_eq!((d.title.as_str(), d.summary.as_str()), ("Plain prose", "Write prose paragraphs."));
        assert_eq!(d.key, "plain-prose", "a malformed key becomes the title's slug");
    }

    /// A stand-in `claude` that records its arguments, working directory, stdin and the hook marker,
    /// then prints `reply`.
    #[cfg(unix)]
    fn stub(dir: &Path, body: &str) -> std::ffi::OsString {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("claude");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{d}/args'\npwd > '{d}/cwd'\ncat > '{d}/stdin'\nprintf '%s' \"${MARKER_ENV}\" > '{d}/marker'\n{body}\n",
            d = dir.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.into_os_string()
    }

    #[cfg(unix)]
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marsad-gen-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn a_run_is_marked_and_kept_out_of_any_project() {
        let dir = scratch("run");
        let program = stub(&dir, &format!("printf '%s' '{}'", reply(sample()).replace('\'', "'\\''")));
        let req = Request { idea: "stop over-explaining".into(), ..Request::default() };
        let d = generate_with(&program, "fable", &dir, &req, Duration::from_secs(10), &AtomicBool::new(false)).unwrap();
        assert_eq!(d.key, "short-pull-requests");
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        assert_eq!(read("stdin"), stdin_text(&req));
        assert_eq!(read("marker"), "1", "marsad hook skips the run");
        assert_eq!(Path::new(read("cwd").trim()).canonicalize().unwrap(), generator_dir(&dir).canonicalize().unwrap());
        assert!(read("args").contains("--safe-mode\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_rejected_model_falls_back_to_opus_then_the_default() {
        let dir = scratch("fallback");
        let rejected = serde_json::json!({ "is_error": true, "api_error_status": 404, "result": "There's an issue with the selected model" });
        // Rejects any run given --model, answers the one without.
        let body = format!(
            "if grep -q -- '^--model$' '{d}/args.now' 2>/dev/null; then printf '%s' '{rejected}'; else printf '%s' '{ok}'; fi",
            d = dir.display(),
            rejected = rejected.to_string().replace('\'', "'\\''"),
            ok = reply(sample()).replace('\'', "'\\''"),
        );
        let program = stub(&dir, &format!("printf '%s\\n' \"$@\" > '{}/args.now'\n{body}", dir.display()));
        let req = Request { idea: "x".into(), ..Request::default() };
        let d = generate_with(&program, "fable", &dir, &req, Duration::from_secs(10), &AtomicBool::new(false)).unwrap();
        assert_eq!(d.key, "short-pull-requests");
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        let models: Vec<&str> = args.lines().collect::<Vec<_>>().windows(2).filter(|w| w[0] == "--model").map(|w| w[1]).collect();
        assert_eq!(models, ["fable", "opus"], "then a run without --model");
        assert_eq!(args.matches("-p\n").count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_kills_the_run() {
        let dir = scratch("cancel");
        let program = stub(&dir, "sleep 30");
        let cancel = AtomicBool::new(true);
        let started = std::time::Instant::now();
        let req = Request { idea: "x".into(), ..Request::default() };
        assert_eq!(generate_with(&program, "fable", &dir, &req, Duration::from_secs(60), &cancel), Err("cancelled".into()));
        assert!(started.elapsed() < Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_claude_says_so() {
        let data = std::env::temp_dir().join(format!("marsad-gen-missing-{}", std::process::id()));
        let req = Request { idea: "x".into(), ..Request::default() };
        let err = generate_with("marsad-no-such-claude".as_ref(), "fable", &data, &req, Duration::from_secs(5), &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("was not found"), "{err}");
        let _ = std::fs::remove_dir_all(&data);
    }
}

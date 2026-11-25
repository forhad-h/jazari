//! Writing a library prompt from a rough idea: one isolated `claude -p` call that returns a draft
//! as structured JSON, with what it assumed and what it left for the author to decide.
//!
//! The run carries none of the user's context: `--safe-mode` skips CLAUDE.md, hooks, plugins and
//! MCP servers, the working directory is marsad's own `generator/` folder, and
//! `--no-session-persistence` leaves no transcript. `MARKER_ENV` is set as well, so `marsad hook`
//! drops the run's events should a hook still fire (a managed plugin's, say), and it never shows
//! on the dashboard. `claude` reads only the last `--append-system-prompt-file`, so the
//! generator's instructions and the output hygiene rules go in one merged file.

use crate::rules::Kind;
use crate::share::{Captured, RunError, run_captured};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// The strongest model alias Claude Code accepts (`claude --help` lists fable, opus, sonnet).
pub const DEFAULT_MODEL: &str = "fable";

/// Tried when the configured alias is rejected, before the session default.
const FALLBACK_MODEL: &str = "opus";

/// Turns the run may take. Tools are off, but the structured reply comes back through a tool call,
/// and Claude Code asks again when the model first answers in plain text.
const MAX_TURNS: &str = "3";

/// Set on the `claude` run, so `marsad hook` ignores its events.
pub const MARKER_ENV: &str = "MARSAD_GENERATOR";

/// Overrides the `claude` program, for a stub in tests or a non-standard install.
const PROGRAM_ENV: &str = "MARSAD_CLAUDE";

/// The longest a run may take before it is killed. A strong model at high effort can take minutes.
const WAIT: Duration = Duration::from_secs(10 * 60);

const INSTRUCTIONS: &str = include_str!("../assets/PROMPT-GENERATOR.md");
const SKILL_INSTRUCTIONS: &str = include_str!("../assets/SKILL-GENERATOR.md");
const OUTPUT_HYGIENE: &str = include_str!("../assets/OUTPUT-HYGIENE.md");

/// The most questions a draft carries.
pub const MAX_QUESTIONS: usize = 9;

/// The shape the reply must take.
pub const SCHEMA: &str = r#"{"type":"object","required":["key","title","summary","body","assumptions","questions"],"properties":{"key":{"type":"string","pattern":"^[a-z0-9][a-z0-9-]{1,40}$"},"title":{"type":"string"},"summary":{"type":"string","description":"one line, at most 80 characters"},"body":{"type":"string","description":"the whole file, starting with '# <title>'"},"assumptions":{"type":"array","items":{"type":"string"}},"questions":{"type":"array","maxItems":9,"items":{"type":"string"}}}}"#;

/// The shape a skill's reply must take: a prompt's, with the files to write and remove.
pub const SKILL_SCHEMA: &str = r#"{"type":"object","required":["key","title","summary","body","files","removed","assumptions","questions"],"properties":{"key":{"type":"string","pattern":"^[a-z0-9][a-z0-9-]{1,40}$"},"title":{"type":"string"},"summary":{"type":"string","description":"one line, at most 80 characters"},"body":{"type":"string","description":"the whole SKILL.md, starting with its --- frontmatter"},"files":{"type":"array","items":{"type":"object","required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}},"removed":{"type":"array","items":{"type":"string"}},"assumptions":{"type":"array","items":{"type":"string"}},"questions":{"type":"array","maxItems":9,"items":{"type":"string"}}}}"#;

/// A file of a skill draft other than its `SKILL.md`: new, or changed from the library copy.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct DraftFile {
    pub path: String,
    pub content: String,
}

/// A generated prompt or skill and the decisions it leaves open.
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
    /// A skill's files besides `SKILL.md` that the draft writes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<DraftFile>,
    /// A skill's files that the draft deletes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<String>,
    #[serde(skip)]
    pub kind: Kind,
}

impl Draft {
    /// A draft of an existing prompt or skill, for revising it: nothing assumed, nothing asked.
    pub fn of(rule: &crate::rules::Rule) -> Self {
        Draft { key: rule.key.clone(), body: rule.body().to_string(), kind: rule.kind, ..Draft::default() }.normalized()
    }

    /// The files this draft writes over `earlier`'s, and the removals of both: what a revision
    /// changed on top of the draft before it.
    pub fn on_top_of(mut self, earlier: &Draft) -> Self {
        let mut files: Vec<DraftFile> = earlier.files.iter().filter(|f| !self.files.iter().any(|n| n.path == f.path)).cloned().collect();
        files.extend(std::mem::take(&mut self.files));
        files.retain(|f| !self.removed.contains(&f.path));
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut removed: Vec<String> = earlier.removed.iter().filter(|r| !files.iter().any(|f| &f.path == *r)).cloned().collect();
        removed.extend(std::mem::take(&mut self.removed));
        removed.sort();
        removed.dedup();
        Draft { files, removed, ..self }
    }

    /// `normalized`, for a body the user edited: the title and summary follow the new text.
    pub fn normalized_keeping_key(self) -> Self {
        Draft { title: String::new(), summary: String::new(), ..self }.normalized()
    }

    /// The body with a `# <title>` first line, and the title and summary as the library will read
    /// them back from it. A key that isn't well formed is replaced by the title's slug.
    pub fn normalized(mut self) -> Self {
        if self.kind == Kind::Skill {
            return self.normalized_skill();
        }
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
        // One key answers each, 1 to 9.
        self.questions.truncate(MAX_QUESTIONS);
        self
    }

    /// A skill: the key named in the frontmatter (or made from the draft's), the title the
    /// library shows, and only files that stay inside the folder.
    fn normalized_skill(mut self) -> Self {
        use crate::skills::{SKILL_MD, front, safe_path};
        if crate::rules::key_problem(&[], &self.key, None).is_some() {
            let (name, _) = front(&self.body);
            self.key = [name.as_str(), self.title.as_str()].into_iter().map(crate::rules::slug).find(|k| crate::rules::key_problem(&[], k, None).is_none()).unwrap_or_default();
        }
        self.body = format!("{}\n", self.body.trim());
        let (name, description) = front(&self.body);
        self.title = if name.trim().is_empty() { self.key.clone() } else { name.trim().to_string() };
        self.summary = crate::rules::shorten(description.trim(), 80);
        self.files.retain(|f| safe_path(&f.path) && f.path != SKILL_MD);
        self.removed.retain(|p| safe_path(p) && p != SKILL_MD);
        self.assumptions.retain(|a| !a.trim().is_empty());
        self.questions.retain(|q| !q.trim().is_empty());
        self.questions.truncate(MAX_QUESTIONS);
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
    pub kind: Kind,
    /// A skill's other text files as they are now, for a revision.
    pub files: Vec<DraftFile>,
    /// A skill's files that are not sent, such as large data.
    pub other_files: Vec<String>,
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
    for f in &req.files {
        out += &format!("<skill_file path=\"{}\">\n{}\n</skill_file>\n", f.path, f.content.trim_end());
    }
    if !req.other_files.is_empty() {
        out += &format!("<other_files>\n{}\n</other_files>\n", req.other_files.join("\n"));
    }
    out
}

/// The generator's system prompt: its instructions for a prompt or a skill, then the output
/// hygiene rules.
pub fn system_text_for(kind: Kind) -> String {
    let instructions = if kind == Kind::Skill { SKILL_INSTRUCTIONS } else { INSTRUCTIONS };
    format!("{}\n\n{}\n", instructions.trim_end(), OUTPUT_HYGIENE.trim_end())
}

#[cfg(test)]
pub fn system_text() -> String {
    system_text_for(Kind::Prompt)
}

/// The neutral working directory for the run, so no project's CLAUDE.md is in reach.
fn generator_dir(data: &Path) -> PathBuf {
    let dir = data.join("generator");
    std::path::absolute(&dir).unwrap_or(dir)
}

/// Writes the system prompt to `<data>/generator/<hash>.md` and returns its path.
#[cfg(test)]
fn prompt_file(data: &Path) -> anyhow::Result<PathBuf> {
    prompt_file_for(data, Kind::Prompt)
}

fn prompt_file_for(data: &Path, kind: Kind) -> anyhow::Result<PathBuf> {
    let text = system_text_for(kind);
    let path = generator_dir(data).join(format!("{:016x}.md", crate::rules::fnv1a(text.as_bytes())));
    crate::rules::write_if_changed(&path, &text)?;
    Ok(path)
}

/// The arguments for one run; without a model, Claude Code's default is used.
#[cfg(test)]
pub fn args(model: Option<&str>, prompt_file: &Path) -> Vec<String> {
    args_for(model, prompt_file, Kind::Prompt)
}

pub fn args_for(model: Option<&str>, prompt_file: &Path, kind: Kind) -> Vec<String> {
    let mut args: Vec<String> =
        ["-p", "--safe-mode", "--no-session-persistence", "--tools", "", "--max-turns", MAX_TURNS].map(String::from).to_vec();
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        args.extend(["--model".into(), model.trim().into()]);
    }
    let schema = if kind == Kind::Skill { SKILL_SCHEMA } else { SCHEMA };
    args.extend(["--effort", "high", "--output-format", "json", "--json-schema", schema].map(String::from));
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
#[cfg(test)]
fn parse(out: &Captured) -> Result<Draft, Failure> {
    parse_as(out, Kind::Prompt)
}

fn parse_as(out: &Captured, kind: Kind) -> Result<Draft, Failure> {
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
    let mut draft: Draft = serde_json::from_value(value).map_err(|e| Failure::Other(format!("claude's draft was malformed: {e}")))?;
    if draft.body.trim().is_empty() {
        return Err(Failure::Other(format!("claude returned an empty {}", kind.noun())));
    }
    draft.kind = kind;
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
    let file = prompt_file_for(data, req.kind).map_err(|e| format!("couldn't write the generator's prompt: {e:#}"))?;
    let input = stdin_text(req);
    let mut models = vec![Some(model)];
    if model != FALLBACK_MODEL {
        models.push(Some(FALLBACK_MODEL));
    }
    models.push(None);
    for m in models {
        let mut cmd = Command::new(program);
        cmd.args(args_for(m, &file, req.kind)).current_dir(generator_dir(data)).env(MARKER_ENV, "1");
        let out = run_captured(&mut cmd, input.as_bytes(), wait, cancel).map_err(|e| match e {
            RunError::NotFound => format!("Claude Code (`{}`) was not found", program.to_string_lossy()),
            RunError::Spawn(why) => format!("couldn't run claude: {why}"),
            RunError::Timeout => format!("claude did not finish within {} minutes", wait.as_secs() / 60),
            RunError::Cancelled => "cancelled".into(),
        })?;
        match parse_as(&out, req.kind) {
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
    /// Delete one of your prompts; its text is kept as a version you can restore.
    Rm(RmArgs),
    /// List the earlier versions of one of your prompts, or print one; without a key, list deleted
    /// prompts that can be restored.
    Versions(VersionsArgs),
    /// Make an earlier version the prompt again (the newest by default), or bring back a deleted one.
    Restore(RestoreArgs),
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

#[derive(clap::Args)]
pub struct VersionsArgs {
    key: Option<String>,
    /// Print this version's text.
    number: Option<u32>,
}

#[derive(clap::Args)]
pub struct RestoreArgs {
    key: String,
    /// The version to restore (default: the newest).
    number: Option<u32>,
}

pub fn run(cmd: PromptCmd) -> anyhow::Result<()> {
    match cmd {
        PromptCmd::New(args) => run_new(args),
        PromptCmd::Edit(args) => run_edit(args),
        PromptCmd::Copy(args) => run_copy(args),
        PromptCmd::Rm(args) => {
            let path = crate::rules::remove_own(&args.key)?;
            println!("Deleted {}", path.display());
            println!("`marsad prompt restore {}` brings it back.", args.key);
            Ok(())
        }
        PromptCmd::Versions(args) => run_versions(args),
        PromptCmd::Restore(args) => {
            let v = crate::rules::restore_own(&args.key, args.number)?;
            println!("Restored {} to version {} ({}, replaced {}).", args.key, v.number, v.title, v.when());
            Ok(())
        }
    }
}

// ---------- `marsad skill` ----------

#[derive(clap::Subcommand)]
pub enum SkillCmd {
    /// List the skills: built-in ones and your own.
    List {
        #[arg(long, value_enum, default_value = "table")]
        format: crate::report::Format,
    },
    /// Write a skill from a rough idea with Claude, review it, and save it to your library.
    New(NewArgs),
    /// Open one of your skills' SKILL.md in $EDITOR, or revise the skill with Claude (--with).
    Edit(EditArgs),
    /// Save a copy of a skill, such as a built-in one, to your library under a new name.
    Copy(CopyArgs),
    /// Delete one of your skills; its folder is kept as a version you can restore.
    Rm(RmArgs),
    /// List the earlier versions of one of your skills, or print one's SKILL.md; without a key,
    /// list deleted skills that can be restored.
    Versions(VersionsArgs),
    /// Make an earlier version the skill again (the newest by default), or bring back a deleted one.
    Restore(RestoreArgs),
    /// Install a skill into Claude Code's skills folder (~/.claude/skills), unless a copy is there.
    Install(RmArgs),
}

pub fn run_skill(cmd: SkillCmd) -> anyhow::Result<()> {
    use crate::skills;
    match cmd {
        SkillCmd::List { format } => {
            let lib: Vec<crate::rules::Rule> = crate::rules::library().into_iter().filter(|r| r.kind == Kind::Skill).collect();
            println!("{}", skill_list(format, &lib));
            Ok(())
        }
        SkillCmd::New(args) => run_new_skill(args),
        SkillCmd::Edit(args) => run_edit_skill(args),
        SkillCmd::Copy(args) => {
            let skill = skills::find(&args.key).ok_or_else(|| anyhow::anyhow!("no skill named {:?}", args.key))?;
            let lib = crate::rules::library();
            let key = args.new_key.unwrap_or_else(|| crate::rules::suggest_key(&lib, &format!("{}-copy", skill.key), &skill.name));
            let path = skills::save_own(&key, &skills::renamed(&skill.files, Some(&skill.key), &key), None)?;
            println!("Saved a copy of {} as {key}: {}", skill.key, path.display());
            println!("Edit it with `marsad skill edit {key}`.");
            Ok(())
        }
        SkillCmd::Rm(args) => {
            let path = skills::remove_own(&args.key)?;
            println!("Deleted {}", path.display());
            println!("`marsad skill restore {}` brings it back. A copy installed in {} is left as it is.", args.key, skills::home_relative(&skills::claude_skills_dir()));
            Ok(())
        }
        SkillCmd::Versions(args) => run_skill_versions(args),
        SkillCmd::Restore(args) => {
            let v = skills::restore_own(&args.key, args.number)?;
            println!("Restored {} to version {} ({}, replaced {}).", args.key, v.number, v.title, v.when());
            if let Some(Ok(report)) = skills::sync_installed(&args.key) {
                println!("{}", report.message());
            }
            Ok(())
        }
        SkillCmd::Install(args) => {
            let lib = crate::rules::library();
            let rule = lib.iter().find(|r| r.key == args.key && r.kind == Kind::Skill).ok_or_else(|| anyhow::anyhow!("no skill named {:?}", args.key))?;
            for report in skills::provide(&[rule])? {
                println!("{}", report.message());
            }
            Ok(())
        }
    }
}

fn skill_list(format: crate::report::Format, lib: &[crate::rules::Rule]) -> String {
    use crate::report::Format;
    let installed = |key: &str| crate::skills::claude_skills_dir().join(key).join(crate::skills::SKILL_MD).exists();
    match format {
        Format::Table => {
            let width = lib.iter().map(|r| r.key.chars().count()).max().unwrap_or(0);
            let mut out: Vec<String> = lib
                .iter()
                .map(|r| format!("{:<width$}  {:<10}  {:<9}  {}", r.key, if r.own { "[yours]" } else { "[built-in]" }, if installed(&r.key) { "installed" } else { "" }, r.summary))
                .collect();
            out.push(String::new());
            out.push("`marsad rules <name>…` starts Claude Code with skills and prompts; a session must use each skill it is given.".into());
            out.push(format!("Write your own with `marsad skill new \"<rough idea>\"`. Your skills are folders in {}", crate::skills::skills_dir().display()));
            out.join("\n")
        }
        Format::Json => {
            let list: Vec<_> = lib
                .iter()
                .map(|r| serde_json::json!({ "name": r.key, "title": r.title, "summary": r.summary, "own": r.own, "installed": installed(&r.key) }))
                .collect();
            serde_json::to_string_pretty(&list).unwrap_or_default()
        }
        Format::Csv => {
            let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            let mut out = vec!["name,title,summary,own,installed".to_string()];
            out.extend(lib.iter().map(|r| format!("{},{},{},{},{}", quote(&r.key), quote(&r.title), quote(&r.summary), r.own, installed(&r.key))));
            out.join("\n")
        }
    }
}

fn run_edit_skill(args: EditArgs) -> anyhow::Result<()> {
    use crate::skills;
    let skill = skills::find(&args.key).ok_or_else(|| anyhow::anyhow!("no skill named {:?}", args.key))?;
    anyhow::ensure!(skill.own, "{} is built in and can't be edited; `marsad skill copy {}` saves a copy you can edit", skill.key, skill.key);
    let Some(feedback) = args.with else {
        let edited = crate::share_view::edit_in_editor(skill.skill_md(), "skill", ".md").map_err(|e| anyhow::anyhow!(e))?;
        if edited.trim() == skill.skill_md().trim() {
            println!("No changes.");
            return Ok(());
        }
        let draft = Draft { key: skill.key.clone(), body: edited, kind: Kind::Skill, ..Draft::default() }.normalized();
        let path = save_draft(&draft, &skill.key, Some(&skill.key))?;
        println!("Saved {}", path.display());
        return Ok(());
    };
    let (sent, named) = skills::for_revision(&skill.files);
    let req = Request {
        idea: skill.skill_md().to_string(),
        previous: Some(skill.skill_md().to_string()),
        feedback,
        kind: Kind::Skill,
        files: sent.iter().map(|f| DraftFile { path: f.path.clone(), content: f.text().unwrap_or_default().to_string() }).collect(),
        other_files: named,
        ..Request::default()
    };
    let draft = cli_generate(&req)?;
    let interactive = !args.yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    finish(draft, skill.key.clone(), Some(skill.key.clone()), req, args.yes, interactive, crate::report::Format::Table)
}

fn run_skill_versions(args: VersionsArgs) -> anyhow::Result<()> {
    use crate::skills;
    let Some(key) = args.key else {
        let deleted = skills::deleted();
        if deleted.is_empty() {
            println!("No deleted skills to restore.");
        }
        for key in deleted {
            let n = skills::versions(&key).len();
            println!("{key}  ({n} version{})", if n == 1 { "" } else { "s" });
        }
        return Ok(());
    };
    if skills::find(&key).is_some_and(|s| !s.own) {
        anyhow::bail!("{key} is built in and has no versions");
    }
    let list = skills::versions(&key);
    if let Some(n) = args.number {
        let v = list.iter().find(|v| v.number == n).ok_or_else(|| anyhow::anyhow!("{key} has no version {n}"))?;
        print!("{}", v.body);
        return Ok(());
    }
    if list.is_empty() {
        println!("{key} has no earlier versions yet; one is kept each time it is edited, renamed or deleted.");
        return Ok(());
    }
    for v in &list {
        println!("{:>4}  {}  {}  {}", format!("v{}", v.number), v.when(), v.title, v.summary);
    }
    println!();
    println!("`marsad skill versions {key} <n>` prints one's SKILL.md; `marsad skill restore {key} <n>` brings the whole folder back.");
    Ok(())
}

fn run_versions(args: VersionsArgs) -> anyhow::Result<()> {
    let Some(key) = args.key else {
        let deleted = crate::rules::deleted();
        if deleted.is_empty() {
            println!("No deleted prompts to restore.");
        }
        for key in deleted {
            let n = crate::rules::versions(&key).len();
            println!("{key}  ({n} version{})", if n == 1 { "" } else { "s" });
        }
        return Ok(());
    };
    let lib = crate::rules::library();
    if lib.iter().any(|r| r.key == key && !r.own) {
        anyhow::bail!("{key} is built in and has no versions");
    }
    let list = crate::rules::versions(&key);
    if let Some(n) = args.number {
        let v = list.iter().find(|v| v.number == n).ok_or_else(|| anyhow::anyhow!("{key} has no version {n}"))?;
        print!("{}", v.body);
        return Ok(());
    }
    if list.is_empty() {
        println!("{key} has no earlier versions yet; one is kept each time it is edited, renamed or deleted.");
        return Ok(());
    }
    for v in &list {
        println!("{:>4}  {}  {}  {}", format!("v{}", v.number), v.when(), v.title, v.summary);
    }
    println!();
    println!("`marsad prompt versions {key} <n>` prints one; `marsad prompt restore {key} <n>` brings it back.");
    Ok(())
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

fn run_new_skill(args: NewArgs) -> anyhow::Result<()> {
    let (idea, used_stdin) = read_idea(&args)?;
    anyhow::ensure!(!idea.trim().is_empty(), "the rough idea is empty");
    let lib = crate::rules::library();
    if let Some(why) = args.key.as_deref().and_then(|k| crate::rules::key_problem(&lib, k, None)) {
        anyhow::bail!("can't use that key: {why}");
    }
    let req = Request { idea, name: args.name.clone().unwrap_or_default(), kind: Kind::Skill, ..Request::default() };
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
        "{} is built in and can't be edited{}",
        rule.key,
        if crate::rules::copy_problem(rule).is_none() { format!("; `marsad prompt copy {}` saves a copy you can edit", rule.key) } else { String::new() }
    );
    let Some(feedback) = args.with else {
        let edited = crate::share_view::edit_in_editor(rule.body(), "prompt", ".md").map_err(|e| anyhow::anyhow!(e))?;
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
    if let Some(why) = crate::rules::copy_problem(rule) {
        anyhow::bail!(why);
    }
    let key = args.new_key.unwrap_or_else(|| crate::rules::suggest_key(&lib, &format!("{}-copy", rule.key), &rule.title));
    let path = crate::rules::save_own(&key, rule.body(), None)?;
    println!("Saved a copy of {} as {key}: {}", rule.key, path.display());
    println!("Edit it with `marsad prompt edit {key}`.");
    Ok(())
}

fn cli_generate(req: &Request) -> anyhow::Result<Draft> {
    eprintln!("Writing the {} with Claude ({}); this can take a minute or two…", req.kind.noun(), crate::settings::generator_model());
    generate(req, &AtomicBool::new(false)).map_err(|e| anyhow::anyhow!(e))
}

/// Shows the draft, then saves it: at once with `--yes`, after a review loop in a terminal, and
/// otherwise not at all.
/// Saves a reviewed draft: a prompt's file, or a skill's folder (the one it revises with the
/// draft's changes), then the skill's installed copy when marsad may update it.
fn save_draft(draft: &Draft, key: &str, replacing: Option<&str>) -> anyhow::Result<PathBuf> {
    match draft.kind {
        Kind::Prompt => crate::rules::save_own(key, &draft.body, replacing),
        Kind::Skill => {
            let path = crate::skills::save_draft(key, draft, replacing, None)?;
            match crate::skills::sync_installed(key) {
                Some(Ok(report)) => eprintln!("{}", report.message()),
                Some(Err(e)) => eprintln!("couldn't update the installed copy: {e:#}"),
                None => {}
            }
            Ok(path)
        }
    }
}

fn finish(mut draft: Draft, mut key: String, replacing: Option<String>, mut req: Request, yes: bool, interactive: bool, format: crate::report::Format) -> anyhow::Result<()> {
    use std::io::Write;
    loop {
        if !matches!(format, crate::report::Format::Json) {
            println!("{}", render_draft(&draft, &key));
        }
        if yes || !interactive {
            let saved = if yes { Some(save_draft(&draft, &key, replacing.as_deref())?) } else { None };
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
                let path = save_draft(&draft, &key, replacing.as_deref())?;
                println!("Saved {}", path.display());
                return Ok(());
            }
            "e" | "E" => match crate::share_view::edit_in_editor(&draft.body, draft.kind.noun(), ".md") {
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
                if draft.kind == Kind::Skill {
                    // Claude sees the files as this draft has them.
                    let mut files: Vec<DraftFile> = req.files.iter().filter(|f| !draft.removed.contains(&f.path) && !draft.files.iter().any(|n| n.path == f.path)).cloned().collect();
                    files.extend(draft.files.iter().cloned());
                    req.files = files;
                }
                match cli_generate(&req) {
                    Ok(d) => draft = d.on_top_of(&draft),
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
    for f in &d.files {
        out += &format!("\n{} (written)\n{rule}\n{}\n{rule}\n", f.path, f.content.trim_end());
    }
    for path in &d.removed {
        out += &format!("\n{path} (removed)\n");
    }
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
    let at = match d.kind {
        Kind::Prompt => crate::rules::prompts_dir().join(format!("{key}.md")),
        Kind::Skill => crate::skills::skills_dir().join(key),
    };
    out + &format!("\nSaves as {} ({})", key, at.display())
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
        assert!(has("--tools", "") && has("--max-turns", MAX_TURNS) && has("--model", "fable") && has("--effort", "high"));
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
            ..Request::default()
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
        let many = Draft { body: "# T\n\nx".into(), questions: (1..=12).map(|n| format!("q{n}")).collect(), ..Draft::default() }.normalized();
        assert_eq!(many.questions.len(), MAX_QUESTIONS, "a key answers each");
        let schema: serde_json::Value = serde_json::from_str(SCHEMA).unwrap();
        assert_eq!(schema["properties"]["questions"]["maxItems"], MAX_QUESTIONS);
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
    fn a_skill_draft_keeps_its_folder_and_names_itself() {
        let reply_json = reply(serde_json::json!({
            "key": "Bad Key", "title": "x", "summary": "",
            "body": "---\nname: release-notes\ndescription: \"Load it before you write release notes. Do not load it for code.\"\n---\n\n# Release notes\n",
            "files": [{ "path": "scripts/check.sh", "content": "echo ok" }, { "path": "../escape", "content": "x" }, { "path": "SKILL.md", "content": "x" }],
            "removed": ["/etc/passwd", "data/old.txt"],
            "assumptions": [], "questions": []
        }));
        let d = parse_as(&captured(&reply_json), Kind::Skill).unwrap();
        assert_eq!((d.key.as_str(), d.title.as_str()), ("release-notes", "release-notes"));
        assert!(d.summary.starts_with("Load it before you write release notes"));
        assert_eq!(d.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["scripts/check.sh"], "only paths inside the folder");
        assert_eq!(d.removed, ["data/old.txt"]);
        let newer = Draft { files: vec![DraftFile { path: "a.txt".into(), content: "a".into() }], removed: vec!["scripts/check.sh".into()], ..d.clone() };
        let merged = newer.on_top_of(&d);
        assert_eq!(merged.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt"], "a later removal wins");
        assert_eq!(merged.removed, ["data/old.txt", "scripts/check.sh"]);
        let schema: serde_json::Value = serde_json::from_str(SKILL_SCHEMA).unwrap();
        assert_eq!(schema["required"].as_array().unwrap().len(), 8);
        let file = Path::new("/g.md");
        assert!(args_for(None, file, Kind::Skill).windows(2).any(|w| w[0] == "--json-schema" && w[1] == SKILL_SCHEMA));
        let text = system_text_for(Kind::Skill);
        assert!(text.starts_with("# Writing a skill") && text.trim_end().ends_with(OUTPUT_HYGIENE.trim_end()));
        assert!(text.contains("when Claude must not load it"), "the description says when not to load it too");
    }

    #[test]
    fn a_skill_revision_sends_its_files() {
        let req = Request {
            idea: "x".into(),
            previous: Some("---\nname: k\n---\n".into()),
            feedback: "shorter".into(),
            kind: Kind::Skill,
            files: vec![DraftFile { path: "scripts/a.py".into(), content: "print(1)\n".into() }],
            other_files: vec!["data/big.csv".into()],
            ..Request::default()
        };
        let text = stdin_text(&req);
        assert!(text.contains("<skill_file path=\"scripts/a.py\">\nprint(1)\n</skill_file>\n"), "{text}");
        assert!(text.contains("<other_files>\ndata/big.csv\n</other_files>"));
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

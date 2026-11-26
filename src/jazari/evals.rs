//! Regression checks for the team itself: when a persona, a rubric or the evaluator changes,
//! check that the judges still catch bad work. The effort matches the size of the change.
//!
//! - **Fingerprints** of each item (the evaluator, each rubric group, each persona) show what
//!   changed since the last approved check. Nothing changed means no tokens are spent.
//! - **Tier 0, lint:** always, and free. Structure, roles the pipeline needs, rubric groups, unsafe
//!   instructions and prompt size.
//! - **Tier 1, calibration:** the judges see known-good and known-bad cases from
//!   `assets/jazari/evals.json`, each bad case with a planted defect. There is one batched call per
//!   rubric group, with no tools and no repo. How much runs depends on the change:
//!   - minor (a few words): no calls; the canary watches;
//!   - moderate: the bad cases only, on the small model, at low effort;
//!   - major (new or removed criteria, a different tier or rubric, a large rewrite, or the first
//!     check): every case, on the production judge model, at high effort.
//!
//!   A missed defect blocks the supervisor: a judge that approves bad work can't guard the
//!   pipeline.
//! - **Tier 2, canary:** live judgments are tagged with the version of the role's prompt. When a
//!   changed role passes clearly less often than before, the founder is told.

use super::personas::{self, Persona};
use super::store::Store;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const CASES: &str = include_str!("../../assets/jazari/evals.json");

/// The judge roles; changing one changes how all work is judged.
pub const JUDGES: [&str; 3] = ["evaluator", "code-reviewer", "appsec-engineer"];
/// Roles the pipeline hands work to by name.
const REQUIRED: [&str; 5] = ["chief-of-staff", "engineer", "evaluator", "code-reviewer", "appsec-engineer"];
/// Instructions no role may carry: they would undo the harness's guarantees.
const FORBIDDEN: [&str; 12] = [
    "git push", "push to main", "force push", "--force", "sudo ", "skip the tests", "skip tests", "always approve",
    "approve everything", "ignore the rubric", "disable the checks", "commit the .env",
];
/// A system prompt is sent with every run, so its size is a recurring cost (about 4 characters a token).
const PROMPT_TOKEN_BUDGET: usize = 2500;
const MAX_CRITERIA: usize = 12;

pub fn fingerprint(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// A role's version: the fingerprint of everything that shapes its runs.
pub fn version(p: &Persona) -> String {
    fingerprint(&format!("{:?}|{}|{}", p.tier, p.rubric, p.system_prompt()))
}

// ---------- tier 0: lint ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub error: bool,
    pub what: String,
}

pub fn lint(roster: &[Persona], rubrics: &str) -> Vec<Issue> {
    let mut out = Vec::new();
    let err = |what: String| Issue { error: true, what };
    let mut keys: Vec<&str> = roster.iter().map(|p| p.key.as_str()).collect();
    keys.sort();
    for w in keys.windows(2).filter(|w| w[0] == w[1]) {
        out.push(err(format!("the role `{}` is defined twice", w[0])));
    }
    for k in REQUIRED {
        if !roster.iter().any(|p| p.key == k) {
            out.push(err(format!("the pipeline needs a `{k}` role")));
        }
    }
    let groups: Vec<String> = rubrics.lines().filter_map(|l| l.strip_prefix("## ")).map(|g| g.trim().to_string()).collect();
    if !groups.iter().any(|g| g == "all") {
        out.push(err("rubrics.md has no `## all` section".into()));
    }
    for g in &groups {
        let body = personas::section_of(rubrics, g);
        let criteria: Vec<&str> = body.lines().filter(|l| l.trim_start().starts_with("- ")).collect();
        if !criteria.iter().any(|c| c.contains("[critical]")) {
            out.push(err(format!("rubric `{g}` has no [critical] criterion, so nothing can fail it")));
        }
        if criteria.len() > MAX_CRITERIA {
            out.push(Issue { error: false, what: format!("rubric `{g}` has {} criteria; judges get less reliable past {MAX_CRITERIA}", criteria.len()) });
        }
    }
    let warn = |what: String| Issue { error: false, what };
    for p in roster {
        if !groups.contains(&p.rubric) {
            out.push(Issue { error: true, what: format!("`{}` uses the rubric `{}`, which rubrics.md doesn't define", p.key, p.rubric) });
        }
        if p.mandate.trim().is_empty() {
            out.push(Issue { error: true, what: format!("`{}` has no mandate", p.key) });
        }
        let lower = p.mandate.to_lowercase();
        for f in FORBIDDEN.iter().filter(|f| lower.contains(*f)) {
            out.push(Issue { error: true, what: format!("`{}` tells workers to {f:?}, which the harness forbids", p.key) });
        }
        let tokens = p.system_prompt().len() / 4;
        if tokens > PROMPT_TOKEN_BUDGET {
            out.push(warn(format!("`{}`'s prompt is about {tokens} tokens, sent on every run (budget {PROMPT_TOKEN_BUDGET})", p.key)));
        }
    }
    out
}

// ---------- change size ----------

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Eq, Ord)]
pub enum Level {
    None,
    /// A few words: lint and the canary only.
    Minor,
    /// The bad cases, on the small model, at low effort.
    Moderate,
    /// Every case, on the production judge model, at high effort.
    Major,
}

/// The share of words that differ between two texts, from their longest common subsequence.
pub fn changed_share(old: &str, new: &str) -> f64 {
    let a: Vec<&str> = old.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let mut row = vec![0usize; b.len() + 1];
    for x in &a {
        let mut prev = 0;
        for (j, y) in b.iter().enumerate() {
            let keep = row[j + 1];
            row[j + 1] = if x == y { prev + 1 } else { row[j + 1].max(row[j]) };
            prev = keep;
        }
    }
    let common = row[b.len()];
    1.0 - common as f64 / a.len().max(b.len()) as f64
}

/// Rubric lines that are criteria, with their weight: `[critical]` or not.
fn criteria(text: &str) -> Vec<(bool, String)> {
    text.lines()
        .filter_map(|l| l.trim_start().strip_prefix("- "))
        .map(|c| (c.contains("[critical]"), c.split(':').next().unwrap_or(c).replace("[critical]", "").trim().to_lowercase()))
        .collect()
}

/// How big a change is. A first check, a criterion added, removed or re-weighted, or a
/// different tier or rubric is major whatever the word count.
pub fn level(old: Option<&str>, new: &str, structural: bool) -> Level {
    let Some(old) = old else { return Level::Major };
    if old == new {
        return Level::None;
    }
    let (co, cn) = (criteria(old), criteria(new));
    if structural || co.len() != cn.len() || co.iter().map(|c| c.0).ne(cn.iter().map(|c| c.0)) {
        return Level::Major;
    }
    match changed_share(old, new) {
        s if s <= 0.10 => Level::Minor,
        s if s <= 0.35 => Level::Moderate,
        _ => Level::Major,
    }
}

// ---------- tier 1: calibration ----------

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Case {
    pub id: String,
    pub rubric: String,
    pub builder: String,
    pub expect: String,
    #[serde(default)]
    pub defect: String,
    pub spec: String,
    pub claims: String,
    pub evidence: String,
}

pub fn cases() -> Vec<Case> {
    serde_json::from_str(CASES).unwrap_or_default()
}

/// One batched judge call.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub judge: String,
    pub rubric: String,
    pub system: String,
    pub prompt: String,
    pub model: String,
    pub effort: &'static str,
    pub ids: Vec<String>,
}

pub const VERDICTS_SCHEMA: &str = r#"{"type":"object","properties":{"cases":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"verdict":{"type":"string","enum":["approve","changes"]},"score":{"type":"integer"},"failed":{"type":"array","items":{"type":"string"}}},"required":["id","verdict"]}}},"required":["cases"]}"#;

/// The judge's system prompt for a rubric group: its own prompt and, for the evaluator, the
/// scoring rules and the group's rubric, exactly as live runs get them.
fn judge_system(judge: &Persona, rubric: &str) -> String {
    if judge.key == "evaluator" {
        format!("{}\n# How to score\n\n{}\n\n# The rubric\n\n{}\n{}\n", judge.system_prompt(), personas::scoring_rules(), personas::rubric_section("all"), personas::rubric_section(rubric))
    } else {
        judge.system_prompt()
    }
}

fn call_for(judge: &Persona, rubric: &str, cases: &[&Case], model: &str, effort: &'static str) -> Call {
    let mut prompt = String::from(
        "These are calibration cases for a quality gate. You have no tools: each case's evidence is given in full below, so judge from it alone. Judge each case on its own, as you would live work, and return one verdict per case id.\n",
    );
    for c in cases {
        prompt += &format!("\n<case id=\"{}\">\n<builder>{}</builder>\n<spec>{}</spec>\n<claims>{}</claims>\n<evidence>\n{}\n</evidence>\n</case>\n", c.id, c.builder, c.spec, c.claims, c.evidence);
    }
    Call { judge: judge.key.clone(), rubric: rubric.into(), system: judge_system(judge, rubric), prompt, model: model.into(), effort, ids: cases.iter().map(|c| c.id.clone()).collect() }
}

/// What one case came to.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    pub judge: String,
    pub id: String,
    pub expected: String,
    pub got: String,
    /// The defect planted in a bad case.
    pub defect: String,
}

impl Verdict {
    /// Bad work approved: the judge can't be trusted.
    pub fn missed(&self) -> bool {
        self.expected == "changes" && self.got != "changes"
    }
    /// Good work rejected: costs a fix round, but lets nothing bad through.
    pub fn too_strict(&self) -> bool {
        self.expected == "approve" && self.got != "approve"
    }
}

// ---------- the check ----------

/// What changed, and how hard to check it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// (item, level), for every item that changed.
    pub changed: Vec<(String, Level)>,
    /// (judge, rubric group, level), for each batched call to make.
    pub calls: Vec<(String, String, Level)>,
}

/// The items fingerprinted, with their text: the judges, each rubric group, each persona.
fn items(roster: &[Persona]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    out.push(("scoring".to_string(), personas::scoring_rules(), String::new()));
    for g in std::iter::once("all".to_string()).chain(personas::rubric_groups()) {
        out.push((format!("rubric:{g}"), personas::rubric_section(&g), String::new()));
    }
    for p in roster {
        out.push((format!("persona:{}", p.key), p.system_prompt(), format!("{:?}|{}", p.tier, p.rubric)));
    }
    out
}

/// Works out what to check from what changed since the last approved check.
pub fn plan(store: &Store, roster: &[Persona], force: bool) -> anyhow::Result<Plan> {
    let mut p = Plan::default();
    let groups = personas::rubric_groups();
    let mut by_group: std::collections::BTreeMap<(String, String), Level> = Default::default();
    let mut raise = |judge: &str, group: &str, l: Level| {
        let e = by_group.entry((judge.to_string(), group.to_string())).or_insert(Level::None);
        *e = (*e).max(l);
    };
    for (item, text, shape) in items(roster) {
        let old = if force { None } else { store.get(&format!("evtext:{item}"))? };
        let old_shape = store.get(&format!("evshape:{item}"))?.unwrap_or_default();
        let l = level(old.as_deref(), &text, !force && old.is_some() && old_shape != shape);
        if l == Level::None {
            continue;
        }
        p.changed.push((item.clone(), l));
        match item.split_once(':') {
            Some(("rubric", "all")) => groups.iter().for_each(|g| raise("evaluator", g, l)),
            Some(("rubric", g)) => raise("evaluator", g, l),
            None => groups.iter().for_each(|g| raise("evaluator", g, l)),
            Some(("persona", "evaluator")) => groups.iter().for_each(|g| raise("evaluator", g, l)),
            Some(("persona", k)) if JUDGES.contains(&k) => raise(k, "code", l),
            _ => {}
        }
    }
    p.calls = by_group.into_iter().filter(|(_, l)| *l >= Level::Moderate).map(|((j, g), l)| (j, g, l)).collect();
    Ok(p)
}

/// The result of a check.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub issues: Vec<Issue>,
    pub plan: Plan,
    pub verdicts: Vec<Verdict>,
    /// Calls that failed to give verdicts.
    pub errors: Vec<String>,
}

impl Report {
    /// Whether the supervisor must not start: a lint error, or a judge that approved bad work.
    pub fn blocking(&self) -> bool {
        self.issues.iter().any(|i| i.error) || self.verdicts.iter().any(Verdict::missed)
    }

    pub fn text(&self) -> String {
        let mut out = Vec::new();
        for i in &self.issues {
            out.push(format!("{} {}", if i.error { "✗" } else { "!" }, i.what));
        }
        if self.plan.changed.is_empty() {
            out.push("Nothing changed since the last approved check.".into());
        }
        for (item, l) in &self.plan.changed {
            out.push(format!("changed: {item} ({l:?})"));
        }
        for v in &self.verdicts {
            let mark = if v.missed() { "✗ MISSED a planted defect" } else if v.too_strict() { "! rejected good work" } else { "✓" };
            let defect = if v.missed() { format!(" ({})", v.defect) } else { String::new() };
            out.push(format!("{mark}  {} on {}: expected {}, got {}{defect}", v.judge, v.id, v.expected, v.got));
        }
        out.extend(self.errors.iter().map(|e| format!("✗ {e}")));
        out.push(if self.blocking() { "Blocking: fix the above before the supervisor starts.".into() } else { "OK.".into() });
        out.join("\n")
    }
}

/// Runs a check: lint, then the calibration calls the plan needs, then marks what passed as the
/// new baseline. `judge` makes one batched call and returns its structured output.
pub fn check(store: &Store, roster: &[Persona], force: bool, prod_model: &str, judge: &dyn Fn(&Call) -> anyhow::Result<Value>) -> anyhow::Result<Report> {
    let issues = lint(roster, &personas::rubrics_text());
    let plan = plan(store, roster, force)?;
    let mut report = Report { issues, plan, ..Default::default() };
    if report.issues.iter().any(|i| i.error) {
        return Ok(report);
    }
    let all = cases();
    for (j, g, l) in report.plan.calls.clone() {
        let Some(persona) = roster.iter().find(|p| p.key == j) else { continue };
        let picked: Vec<&Case> = all.iter().filter(|c| c.rubric == g && (l == Level::Major || c.expect == "changes")).collect();
        if picked.is_empty() {
            continue;
        }
        let (model, effort) = if l == Level::Major { (prod_model, "high") } else { ("claude-haiku-5-5", "low") };
        let call = call_for(persona, &g, &picked, model, effort);
        match judge(&call) {
            Ok(v) => {
                for c in &picked {
                    let got = v["cases"].as_array().and_then(|a| a.iter().find(|x| x["id"] == c.id.as_str())).and_then(|x| x["verdict"].as_str()).unwrap_or("none");
                    report.verdicts.push(Verdict { judge: j.clone(), id: c.id.clone(), expected: c.expect.clone(), got: got.into(), defect: c.defect.clone() });
                }
            }
            Err(e) => report.errors.push(format!("{j} on the {g} cases: {e}")),
        }
    }
    if !report.blocking() && report.errors.is_empty() {
        for (item, text, shape) in items(roster) {
            store.put(&format!("evtext:{item}"), &text)?;
            store.put(&format!("evshape:{item}"), &shape)?;
        }
    }
    Ok(report)
}

/// One batched calibration call to `claude -p` on the host: no tools, no session saved, hidden
/// from the dashboard.
pub fn claude_judge(call: &Call) -> anyhow::Result<Value> {
    use std::io::Write;
    let program = std::env::var_os("MARSAD_CLAUDE").filter(|p| !p.is_empty()).unwrap_or_else(|| "claude".into());
    let args = [
        "-p", "--no-session-persistence", "--tools", "", "--max-turns", "3", "--model", &call.model, "--effort", call.effort,
        "--output-format", "json", "--json-schema", VERDICTS_SCHEMA, "--append-system-prompt", &call.system,
    ];
    let mut child = std::process::Command::new(program)
        .args(args)
        .env(crate::prompt_gen::MARKER_ENV, "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    child.stdin.take().expect("stdin is piped").write_all(call.prompt.as_bytes())?;
    let out = child.wait_with_output()?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| anyhow::anyhow!("the judge gave no JSON"))?;
    anyhow::ensure!(v["is_error"] != true, "the judge failed: {}", v["result"].as_str().unwrap_or("unknown error"));
    let s = &v["structured_output"];
    anyhow::ensure!(s.is_object(), "the judge gave no verdicts");
    Ok(s.clone())
}

// ---------- tier 2: canary ----------

/// Passes and judgments of a role's evaluations, for its current version and for all earlier ones.
pub fn canary(store: &Store, role: &str, current: &str) -> anyhow::Result<Option<String>> {
    let (now_pass, now_n, before_pass, before_n) = store.version_split(role, current)?;
    if now_n < 4 || before_n < 4 {
        return Ok(None);
    }
    let (now, before) = (now_pass as f64 / now_n as f64, before_pass as f64 / before_n as f64);
    if before - now < 0.25 {
        return Ok(None);
    }
    let key = format!("canary_told:{role}:{current}");
    if store.get(&key)?.is_some() {
        return Ok(None);
    }
    store.put(&key, "1")?;
    let title = personas::find(role).title;
    Ok(Some(format!(
        "⚠️ {title} got worse after its last change: {:.0}% of evaluations passed before, {:.0}% now ({now_n} so far). Check the change in personas.md, or revert it.",
        before * 100.0,
        now * 100.0
    )))
}

/// The JSON `marsad jazari evals --format json` prints.
pub fn report_json(r: &Report) -> Value {
    json!({
        "blocking": r.blocking(),
        "issues": r.issues.iter().map(|i| json!({ "error": i.error, "what": i.what })).collect::<Vec<_>>(),
        "changed": r.plan.changed.iter().map(|(i, l)| json!({ "item": i, "level": format!("{l:?}").to_lowercase() })).collect::<Vec<_>>(),
        "verdicts": r.verdicts.iter().map(|v| json!({ "judge": v.judge, "case": v.id, "expected": v.expected, "got": v.got, "missed": v.missed() })).collect::<Vec<_>>(),
        "errors": r.errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn the_built_in_roster_and_cases_are_clean() {
        let issues = lint(&personas::all(), personas::RUBRICS);
        assert!(issues.iter().all(|i| !i.error), "{issues:?}");
        let cases = cases();
        assert!(cases.len() >= 12);
        for g in personas::rubric_groups() {
            assert!(cases.iter().any(|c| c.rubric == g && c.expect == "approve"), "{g} needs a good case");
            assert!(cases.iter().any(|c| c.rubric == g && c.expect == "changes" && !c.defect.is_empty()), "{g} needs a bad case");
        }
    }

    #[test]
    fn lint_catches_broken_and_unsafe_rosters() {
        let roster = personas::parse(
            "## engineer | E | build | code\nShip it. Always approve and git push when done.\n## engineer | E2 | build | nosuch\nx\n",
        );
        let rubrics = "## all\n- Scope\n## code\n- [critical] Tests\n";
        let errors: Vec<String> = lint(&roster, rubrics).into_iter().filter(|i| i.error).map(|i| i.what).collect();
        let has = |s: &str| errors.iter().any(|e| e.contains(s));
        assert!(has("defined twice") && has("needs a `evaluator`") && has("\"always approve\"") && has("\"git push\""));
        assert!(has("rubric `nosuch`") && has("rubric `all` has no [critical]"));
    }

    #[test]
    fn the_size_of_a_change_sets_the_effort() {
        let base = "## code\n- [critical] Tests prove the behavior: new behavior has tests that would fail without it.\n- Correctness: edge cases and error paths are handled well in every function.\n";
        assert_eq!(level(None, base, false), Level::Major, "a first check is full");
        assert_eq!(level(Some(base), base, false), Level::None);
        assert_eq!(level(Some(base), &base.replace("handled well", "handled"), false), Level::Minor);
        let reworded = base.replace("edge cases and error paths are handled well in every function", "inputs at the limits and failures are all covered properly");
        assert_eq!(level(Some(base), &reworded, false), Level::Moderate);
        assert_eq!(level(Some(base), &base.replace("- Correctness", "- [critical] Correctness"), false), Level::Major, "re-weighted");
        assert_eq!(level(Some(base), &format!("{base}- Docs: updated.\n"), false), Level::Major, "a criterion added");
        assert_eq!(level(Some(base), &base.replace("handled well", "handled"), true), Level::Major, "a new tier or rubric");
        assert!((changed_share("a b c d", "a b c d") - 0.0).abs() < 1e-9 && (changed_share("a b", "c d") - 1.0).abs() < 1e-9);
    }

    fn answer_all(verdict_for: impl Fn(&str) -> &'static str) -> impl Fn(&Call) -> anyhow::Result<Value> {
        move |c: &Call| Ok(json!({ "cases": c.ids.iter().map(|id| json!({ "id": id, "verdict": verdict_for(id) })).collect::<Vec<_>>() }))
    }

    #[test]
    fn a_first_check_calibrates_every_group_then_nothing_runs_until_something_changes() {
        let store = Store::memory();
        let roster = personas::all();
        let calls = RefCell::new(Vec::<Call>::new());
        let perfect = answer_all(|id| if cases().iter().any(|c| c.id == id && c.expect == "approve") { "approve" } else { "changes" });
        let judge = |c: &Call| {
            calls.borrow_mut().push(c.clone());
            perfect(c)
        };
        let r = check(&store, &roster, false, "claude-sonnet-5-5", &judge).unwrap();
        assert!(!r.blocking(), "{}", r.text());
        let groups = personas::rubric_groups();
        let evaluator_calls: Vec<_> = calls.borrow().iter().filter(|c| c.judge == "evaluator").cloned().collect();
        assert_eq!(evaluator_calls.len(), groups.len(), "one batched call per group");
        assert!(evaluator_calls.iter().all(|c| c.effort == "high" && c.model == "claude-sonnet-5-5" && c.system.contains("# The rubric")));
        assert!(calls.borrow().iter().any(|c| c.judge == "code-reviewer" && c.rubric == "code"));
        calls.borrow_mut().clear();
        let again = check(&store, &roster, false, "claude-sonnet-5-5", &judge).unwrap();
        assert!(calls.borrow().is_empty() && again.plan.changed.is_empty(), "nothing changed, no tokens");
    }

    #[test]
    fn a_moderate_rubric_edit_runs_only_the_bad_cases_of_that_group_cheaply() {
        let store = Store::memory();
        let roster = personas::all();
        for (item, text, shape) in items(&roster) {
            store.put(&format!("evtext:{item}"), &text).unwrap();
            store.put(&format!("evshape:{item}"), &shape).unwrap();
        }
        let docs = personas::rubric_section("docs");
        let edited = docs.replace("Plain language, for the stated reader.", "Written in short plain sentences that the stated reader understands at first read.");
        store.put("evtext:rubric:docs", &edited).unwrap();
        let p = plan(&store, &roster, false).unwrap();
        assert_eq!(p.calls, vec![("evaluator".to_string(), "docs".to_string(), Level::Moderate)]);
        let calls = RefCell::new(Vec::<Call>::new());
        let judge = |c: &Call| {
            calls.borrow_mut().push(c.clone());
            answer_all(|_| "changes")(c)
        };
        let r = check(&store, &roster, false, "claude-sonnet-5-5", &judge).unwrap();
        let c = &calls.borrow()[0];
        assert_eq!((c.model.as_str(), c.effort), ("claude-haiku-5-5", "low"));
        assert_eq!(c.ids, vec!["docs-wrong-command"]);
        assert!(!r.blocking());
    }

    #[test]
    fn a_judge_that_approves_a_planted_defect_blocks_and_keeps_the_old_baseline() {
        let store = Store::memory();
        let roster = personas::all();
        let lenient = answer_all(|id| if id == "code-sql-injection" || cases().iter().any(|c| c.id == id && c.expect == "approve") { "approve" } else { "changes" });
        let r = check(&store, &roster, false, "m", &lenient).unwrap();
        assert!(r.blocking());
        assert!(r.text().contains("MISSED a planted defect  evaluator on code-sql-injection"), "{}", r.text());
        assert_eq!(store.get("evtext:scoring").unwrap(), None, "nothing is approved while a judge misses");
        assert!(report_json(&r)["blocking"] == true);
    }

    #[test]
    fn the_canary_speaks_when_a_changed_role_does_clearly_worse() {
        let store = Store::memory();
        for (i, passed) in [true, true, true, true, true].iter().enumerate() {
            store.add_eval(i as i64, "backend-engineer", "eval", *passed, Some(5), "", i as f64, "v1").unwrap();
        }
        for (i, passed) in [false, true, false, false].iter().enumerate() {
            store.add_eval(10 + i as i64, "backend-engineer", "eval", *passed, Some(2), "", 10.0 + i as f64, "v2").unwrap();
        }
        let msg = canary(&store, "backend-engineer", "v2").unwrap().unwrap();
        assert!(msg.contains("100% of evaluations passed before, 25% now"), "{msg}");
        assert_eq!(canary(&store, "backend-engineer", "v2").unwrap(), None, "told once");
        assert_eq!(canary(&store, "backend-engineer", "v1").unwrap(), None);
    }
}

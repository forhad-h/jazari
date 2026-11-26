//! The roles jazari's workers take, from `assets/jazari/personas.md`, or from
//! `~/.marsad/jazari/personas.md` when the founder keeps their own (`rubrics.md` likewise). Each
//! run gets the shared worker rules plus its role's mandate, and the role's tier picks the model.

use super::limits::Profile;

pub const ROSTER: &str = include_str!("../../assets/jazari/personas.md");
pub const RULES: &str = include_str!("../../assets/jazari/personas/worker.md");
pub const RUBRICS: &str = include_str!("../../assets/jazari/rubrics.md");

/// The founder's own copy of a built-in file, when there is one. Line endings are made `\n`:
/// a checkout or an editor on Windows writes `\r\n`, and the sections are found by their
/// `\n## ` headings.
fn custom(name: &str, builtin: &'static str) -> std::borrow::Cow<'static, str> {
    let text = match std::fs::read_to_string(super::dir().join(name)) {
        Ok(text) if !cfg!(test) && !text.trim().is_empty() => std::borrow::Cow::Owned(text),
        _ => std::borrow::Cow::Borrowed(builtin),
    };
    if text.contains('\r') { text.replace("\r\n", "\n").into() } else { text }
}

/// The shared worker rules, with `\n` line endings.
pub fn rules() -> String {
    RULES.replace("\r\n", "\n")
}

pub fn roster_text() -> std::borrow::Cow<'static, str> {
    custom("personas.md", ROSTER)
}

pub fn rubrics_text() -> std::borrow::Cow<'static, str> {
    custom("rubrics.md", RUBRICS)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tier {
    Lead,
    Build,
    Triage,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Persona {
    pub key: String,
    pub title: String,
    pub tier: Tier,
    /// The `rubrics.md` section the evaluator judges this role's work by.
    pub rubric: String,
    pub mandate: String,
}

impl Persona {
    pub fn model(&self, p: &Profile) -> &'static str {
        match self.tier {
            Tier::Lead => p.lead_model,
            Tier::Build => p.build_model,
            Tier::Triage => "claude-haiku-5-5",
        }
    }

    /// Planning roles may queue a whole milestone; the others a few follow-ups.
    pub fn max_follow_ups(&self) -> usize {
        if self.tier == Tier::Lead { 8 } else { 3 }
    }

    /// The criteria this role's work is judged by: the shared ones, then its own.
    pub fn rubric(&self) -> String {
        format!("{}\n{}", rubric_section("all"), rubric_section(&self.rubric))
    }

    /// The system prompt a worker in this role gets.
    pub fn system_prompt(&self) -> String {
        let mut s = format!("{}\n# Your role: {}\n\n{}\n", rules(), self.title, self.mandate.trim());
        if self.tier == Tier::Lead {
            s.push_str("\nRoles you can give follow-up tasks to: ");
            s.push_str(&all().iter().map(|p| p.key.as_str()).collect::<Vec<_>>().join(", "));
            s.push('\n');
        }
        s
    }
}

pub fn parse(text: &str) -> Vec<Persona> {
    let mut out: Vec<Persona> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let parts: Vec<&str> = head.split('|').map(str::trim).collect();
            if let [key, title, tier, rubric] = parts.as_slice() {
                let tier = match *tier {
                    "lead" => Tier::Lead,
                    "triage" => Tier::Triage,
                    _ => Tier::Build,
                };
                out.push(Persona { key: key.to_string(), title: title.to_string(), tier, rubric: rubric.to_string(), mandate: String::new() });
                continue;
            }
        }
        if let Some(p) = out.last_mut() {
            p.mandate.push_str(line);
            p.mandate.push('\n');
        }
    }
    out
}

/// One `## name` section of `rubrics.md`, its heading included.
pub fn rubric_section(name: &str) -> String {
    section_of(&rubrics_text(), name)
}

pub fn section_of(text: &str, name: &str) -> String {
    let head = format!("## {name}");
    let Some(start) = text.find(&format!("\n{head}\n")) else { return String::new() };
    let body = &text[start + 1..];
    let end = body[head.len()..].find("\n## ").map_or(body.len(), |e| e + head.len() + 1);
    body[..end].trim().to_string()
}

/// The rubric groups `rubrics.md` defines, besides `all`.
pub fn rubric_groups() -> Vec<String> {
    rubrics_text().lines().filter_map(|l| l.strip_prefix("## ")).map(|g| g.trim().to_string()).filter(|g| g != "all").collect()
}

/// How the evaluator scores: the scale and the pass rule at the top of `rubrics.md`.
pub fn scoring_rules() -> String {
    rubrics_text().split("\n## ").next().unwrap_or("").trim_start_matches("# jazari rubrics").trim().to_string()
}

pub fn all() -> Vec<Persona> {
    parse(&roster_text())
}

/// `"Backend Engineer"`, `backend_engineer` and `backend-engineer` name the same role.
pub fn key(role: &str) -> String {
    role.trim().to_ascii_lowercase().replace([' ', '_', '/'], "-")
}

/// Names workers use for roles in the roster.
const ALIASES: [(&str, &str); 16] = [
    ("developer", "engineer"), ("software-engineer", "engineer"), ("dev", "engineer"), ("devops", "platform-engineer"),
    ("sre", "platform-engineer"), ("qa", "qa-engineer"), ("tester", "qa-engineer"), ("security", "appsec-engineer"),
    ("security-engineer", "appsec-engineer"), ("designer", "ui-designer"), ("writer", "technical-writer"), ("pm", "product-manager"),
    ("cto", "architect"), ("reviewer", "code-reviewer"), ("frontend", "frontend-engineer"), ("backend", "backend-engineer"),
];

/// The roster key a role name stands for: its own key, an alias's, or `engineer`.
pub fn resolve(role: &str) -> String {
    let k = key(role);
    let k = ALIASES.iter().find(|(a, _)| *a == k).map_or(k, |(_, to)| to.to_string());
    if all().iter().any(|p| p.key == k) { k } else { "engineer".into() }
}

/// The persona for a role; an unknown role is a general engineer. A founder's roster without an
/// `engineer` falls back to the built-in one (lint reports the gap, and the supervisor won't start
/// until it is fixed).
pub fn find(role: &str) -> Persona {
    let k = resolve(role);
    all()
        .into_iter()
        .find(|p| p.key == k)
        .or_else(|| parse(&ROSTER.replace("\r\n", "\n")).into_iter().find(|p| p.key == "engineer"))
        .expect("the built-in roster has an engineer")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jazari::limits::Plan;

    #[test]
    fn the_roster_has_every_department_and_unique_keys() {
        let all = all();
        assert!(all.len() >= 30, "{}", all.len());
        let mut keys: Vec<_> = all.iter().map(|p| p.key.clone()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), all.len());
        for k in ["chief-of-staff", "architect", "code-reviewer", "appsec-engineer", "red-team", "ux-designer", "support-agent"] {
            assert!(all.iter().any(|p| p.key == k), "{k}");
        }
        assert!(all.iter().all(|p| !p.mandate.trim().is_empty()));
        for g in ["code", "plan", "design", "docs", "security"] {
            assert!(all.iter().any(|p| p.rubric == g), "{g}");
        }
        assert!(all.iter().all(|p| ["code", "plan", "design", "docs", "security"].contains(&p.rubric.as_str())));
    }

    #[test]
    fn each_role_is_judged_by_the_shared_and_its_own_criteria() {
        let r = find("chief-of-staff").rubric();
        assert!(r.starts_with("## all") && r.contains("Claims match evidence") && r.contains("## plan") && r.contains("STRIDE"));
        assert!(!r.contains("## code") && !r.contains("WCAG"));
        let r = find("backend-engineer").rubric();
        assert!(r.contains("## code") && r.contains("Tests prove the behavior") && !r.contains("## security"));
        assert!(find("red-team").rubric().contains("regression test that fails before the fix"));
        assert!(scoring_rules().contains("5: ready to ship") && !scoring_rules().contains("## all"));
        let crlf = custom("no-such-file.md", "# t\r\n\r\n## all\r\n- [critical] Scope\r\n");
        assert_eq!(section_of(&crlf, "all"), "## all\n- [critical] Scope", "Windows line endings parse the same");
        let roster = parse(&custom("no-such-file.md", "# r\r\n## engineer | Engineer | build | code\r\nShip small changes.\r\n"));
        assert_eq!((roster.len(), roster[0].key.as_str(), roster[0].rubric.as_str(), roster[0].tier), (1, "engineer", "code", Tier::Build));
        assert_eq!(roster[0].mandate, "Ship small changes.\n");
        assert!(!rules().contains('\r'));
    }

    #[test]
    fn roles_pick_their_model_and_prompt() {
        let p = Plan::Max5.profile();
        let cos = find("Chief of Staff");
        assert_eq!((cos.key.as_str(), cos.model(&p), cos.max_follow_ups()), ("chief-of-staff", "claude-opus-5-5", 8));
        assert!(cos.system_prompt().contains("Roles you can give follow-up tasks to: chief-of-staff"));
        let be = find("backend_engineer");
        assert_eq!((be.model(&p), be.max_follow_ups()), ("claude-sonnet-5-5", 3));
        assert!(be.system_prompt().contains("# Your role: Backend Engineer") && !be.system_prompt().contains("Roles you can give"));
        assert_eq!(find("support agent").model(&p), "claude-haiku-5-5");
        assert_eq!(find("Game Designer").key, "engineer");
        assert_eq!((resolve("Developer"), resolve("devops"), resolve("QA"), resolve("Backend Engineer")), ("engineer".into(), "platform-engineer".into(), "qa-engineer".into(), "backend-engineer".into()));
    }
}

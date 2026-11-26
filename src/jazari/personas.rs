//! The roles jazari's workers take, read from `assets/jazari/personas.md`. Each run gets the
//! shared worker rules plus its role's mandate, and the role's tier picks the model.

use super::limits::Profile;

const ROSTER: &str = include_str!("../../assets/jazari/personas.md");
const RULES: &str = include_str!("../../assets/jazari/personas/worker.md");

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

    /// The system prompt a worker in this role gets.
    pub fn system_prompt(&self) -> String {
        let mut s = format!("{RULES}\n# Your role: {}\n\n{}\n", self.title, self.mandate.trim());
        if self.tier == Tier::Lead {
            s.push_str("\nRoles you can give follow-up tasks to: ");
            s.push_str(&all().iter().map(|p| p.key.as_str()).collect::<Vec<_>>().join(", "));
            s.push('\n');
        }
        s
    }
}

fn parse(text: &str) -> Vec<Persona> {
    let mut out: Vec<Persona> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let parts: Vec<&str> = head.split('|').map(str::trim).collect();
            if let [key, title, tier] = parts.as_slice() {
                let tier = match *tier {
                    "lead" => Tier::Lead,
                    "triage" => Tier::Triage,
                    _ => Tier::Build,
                };
                out.push(Persona { key: key.to_string(), title: title.to_string(), tier, mandate: String::new() });
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

pub fn all() -> Vec<Persona> {
    parse(ROSTER)
}

/// `"Backend Engineer"`, `backend_engineer` and `backend-engineer` name the same role.
pub fn key(role: &str) -> String {
    role.trim().to_ascii_lowercase().replace([' ', '_', '/'], "-")
}

/// The persona for a role; an unknown role is a general engineer with that title.
pub fn find(role: &str) -> Persona {
    let k = key(role);
    all().into_iter().find(|p| p.key == k).unwrap_or_else(|| {
        let mut p = all().into_iter().find(|p| p.key == "engineer").expect("the roster has an engineer");
        p.title = role.trim().to_string();
        p
    })
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
        let odd = find("Game Designer");
        assert_eq!((odd.key.as_str(), odd.title.as_str()), ("engineer", "Game Designer"));
    }
}

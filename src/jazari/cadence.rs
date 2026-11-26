//! Autonomous mode: when a product's queue is empty, jazari keeps improving it.
//!
//! Each job has an interval that depends on the plan, because cheaper plans have fewer tokens. A
//! job runs only when it is due and the product's main branch moved since the job last ran. After
//! twice its interval it runs anyway, so slow-changing products still get the occasional check.
//! A product in normal mode never runs these jobs: it stops when the founder's tasks are done.

use super::limits::Plan;
use super::store::Store;

pub struct Job {
    pub key: &'static str,
    pub role: &'static str,
    pub title: &'static str,
    pub brief: &'static str,
    /// Days between runs on Pro, Max 5x and Max 20x.
    pub days: [f64; 3],
}

pub const JOBS: [Job; 7] = [
    Job {
        key: "bugs",
        role: "bug-hunter",
        title: "Hunt for bugs",
        brief: "Look for real bugs in the product as it is on the main branch. Run it, try edge cases and bad input, and read the recent changes. For each bug you confirm, write a failing test, then fix it. Return larger fixes as follow-ups. If you find no bug, say so, and change nothing.",
        days: [7.0, 2.0, 1.0],
    },
    Job {
        key: "dependencies",
        role: "supply-chain-engineer",
        title: "Check dependencies for known vulnerabilities",
        brief: "Run the vulnerability scanner for this product's ecosystem (for example pip-audit or npm audit). Update each vulnerable dependency to a fixed, pinned version, and make the checks pass. If the checks don't scan dependencies yet, add the scan to company/checks.sh.",
        days: [7.0, 3.0, 1.0],
    },
    Job {
        key: "red-team",
        role: "red-team",
        title: "Red-team the app",
        brief: "Attack the product as it runs locally: authentication, access control, input handling and data exposure. Prove each finding with a working exploit against localhost only. Record findings in company/SECURITY-FINDINGS.md, and return a fix task for each proven one.",
        days: [30.0, 7.0, 3.0],
    },
    Job {
        key: "ux",
        role: "ux-designer",
        title: "Audit one main flow for usability and accessibility",
        brief: "Pick the main user flow that was audited longest ago (see company/UX-NOTES.md). Check it for usability and for WCAG 2.2 AA. Fix the small problems, and return larger ones as follow-ups. Record what you checked in company/UX-NOTES.md.",
        days: [30.0, 7.0, 7.0],
    },
    Job {
        key: "performance",
        role: "performance-engineer",
        title: "Check performance",
        brief: "Measure the main paths of the product. If one is clearly slow or got slower, find the biggest cause and fix it, with a benchmark or test that shows the gain. If nothing is slow, say so, and change nothing.",
        days: [30.0, 14.0, 7.0],
    },
    Job {
        key: "docs",
        role: "technical-writer",
        title: "Bring the docs up to date",
        brief: "Compare the README and the docs with the product as it is now. Fix anything out of date: commands, options, screenshots, descriptions. Keep the Easy English rules.",
        days: [30.0, 14.0, 7.0],
    },
    Job {
        key: "next",
        role: "product-manager",
        title: "Find the next improvement",
        brief: "Read company/VISION.md, company/ROADMAP.md, the feedback in company/RESEARCH.md and the founder's preferences. Choose at most 3 small improvements that help users most for the least work, and return them as follow-ups with acceptance criteria. Update the roadmap. Don't plan anything that needs money, production access or contact with real users without the founder's approval.",
        days: [14.0, 7.0, 3.0],
    },
];

fn index(plan: Plan) -> usize {
    match plan {
        Plan::Pro => 0,
        Plan::Max5 => 1,
        Plan::Max20 => 2,
    }
}

fn last_run(store: &Store, product: &str, job: &str) -> Option<(f64, String)> {
    let v = store.get(&format!("cadence:{product}:{job}")).ok().flatten()?;
    let (at, sha) = v.split_once('|')?;
    Some((at.parse().ok()?, sha.to_string()))
}

/// The next job due for a product, given the head of its main branch.
pub fn due(store: &Store, product: &str, head: &str, plan: Plan, now: f64) -> Option<&'static Job> {
    JOBS.iter().find(|j| {
        let every = j.days[index(plan)] * 86_400.0;
        match last_run(store, product, j.key) {
            None => true,
            Some((at, sha)) => now - at >= every && (sha != head || now - at >= 2.0 * every),
        }
    })
}

pub fn mark(store: &Store, product: &str, job: &Job, head: &str, now: f64) -> anyhow::Result<()> {
    store.put(&format!("cadence:{product}:{}", job.key), &format!("{now}|{head}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_run_when_due_and_only_if_the_code_moved() {
        let store = Store::memory();
        let first = due(&store, "shop", "a1", Plan::Max5, 0.0).unwrap();
        assert_eq!(first.key, "bugs");
        for j in &JOBS {
            mark(&store, "shop", j, "a1", 0.0).unwrap();
        }
        assert!(due(&store, "shop", "a1", Plan::Max5, 86_400.0).is_none(), "nothing is due after a day");
        let three_days = 3.0 * 86_400.0;
        assert!(due(&store, "shop", "a1", Plan::Max5, three_days).is_none(), "due, but the code didn't move");
        assert_eq!(due(&store, "shop", "b2", Plan::Max5, three_days).unwrap().key, "bugs");
        assert_eq!(due(&store, "shop", "a1", Plan::Max5, 4.0 * 86_400.0).unwrap().key, "bugs", "after twice the interval it runs anyway");
        assert!(due(&store, "shop", "b2", Plan::Pro, three_days).is_none(), "Pro runs less often");
        assert!(JOBS.iter().all(|j| crate::jazari::personas::all().iter().any(|p| p.key == j.role)), "every job's role is in the roster");
    }
}

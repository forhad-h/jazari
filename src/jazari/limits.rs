//! When jazari may start work: the subscription's 5-hour and 7-day windows, as each worker run
//! reports them (`rate_limit_event` in `stream-json`), against the plan's profile.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Plan {
    Pro,
    Max5,
    Max20,
}

/// How hard jazari works on one plan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Profile {
    pub plan: Plan,
    /// Percent of each window left for the founder's own sessions.
    pub reserve_5h: f64,
    pub reserve_7d: f64,
    pub max_turns: u32,
    pub build_model: &'static str,
    pub lead_model: &'static str,
}

impl Plan {
    pub const ALL: [Plan; 3] = [Plan::Pro, Plan::Max5, Plan::Max20];

    pub fn key(self) -> &'static str {
        match self {
            Plan::Pro => "pro",
            Plan::Max5 => "max5",
            Plan::Max20 => "max20",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Plan::Pro => "Pro ($20)",
            Plan::Max5 => "Max 5x ($100)",
            Plan::Max20 => "Max 20x ($200)",
        }
    }

    pub fn parse(s: &str) -> Option<Plan> {
        Plan::ALL.into_iter().find(|p| p.key() == s.trim().to_ascii_lowercase())
    }

    pub fn profile(self) -> Profile {
        let p = |reserve_5h, reserve_7d, max_turns, lead_model| Profile {
            plan: self,
            reserve_5h,
            reserve_7d,
            max_turns,
            build_model: "claude-sonnet-5-5",
            lead_model,
        };
        match self {
            Plan::Pro => p(30.0, 25.0, 15, "claude-sonnet-5-5"),
            Plan::Max5 => p(15.0, 15.0, 30, "claude-opus-5-5"),
            Plan::Max20 => p(10.0, 10.0, 50, "claude-opus-5-5"),
        }
    }
}

/// The plan from `settings.json` (`jazari.plan`); Pro, the most careful, until one is set.
pub fn plan() -> Plan {
    crate::settings::get("jazari").and_then(|j| j["plan"].as_str().and_then(Plan::parse)).unwrap_or(Plan::Pro)
}

pub fn set_plan(plan: Plan) -> anyhow::Result<()> {
    let mut j = crate::settings::get("jazari").filter(Value::is_object).unwrap_or_else(|| serde_json::json!({}));
    j["plan"] = Value::from(plan.key());
    crate::settings::set("jazari", j)
}

/// One usage window as a run last reported it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_pct: f64,
    pub resets_at: f64,
    pub allowed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl Limits {
    /// Reads a `rate_limit_event` line; None for any other line.
    pub fn from_event(v: &Value) -> Option<Limits> {
        if v["type"] != "rate_limit_event" {
            return None;
        }
        let info = &v["rate_limit_info"];
        let window = |key: &str| {
            let w = &info["unifiedWindows"][key];
            let mut out = Window {
                used_pct: w["utilization"].as_f64()? * 100.0,
                resets_at: w["resetsAt"].as_f64()?,
                allowed: w["status"].as_str().is_none_or(|s| s == "allowed"),
            };
            // A refusal names the window it is about at the top level.
            if info["rateLimitType"] == key && info["status"].as_str().is_some_and(|s| s != "allowed") {
                out.allowed = false;
            }
            Some(out)
        };
        Some(Limits { five_hour: window("five_hour"), seven_day: window("seven_day") })
    }

    /// Whether a window refused the run.
    pub fn refused(&self) -> bool {
        [self.five_hour, self.seven_day].iter().flatten().any(|w| !w.allowed)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Go,
    /// `key` names the sleep, so it is announced once however often it is decided again.
    Sleep { until: f64, reason: String, key: String },
}

/// The week a 7-day window covers, in hours.
const WEEK_HOURS: f64 = 168.0;
/// How far ahead of an even weekly pace jazari may run, in percentage points.
const PACE_SLACK: f64 = 10.0;
/// How long to wait when ahead of the weekly pace.
const PACE_WAIT: f64 = 1800.0;

/// A few minutes after a reset, so not every client retries at the same second.
fn after(reset: f64) -> f64 {
    reset + 60.0 + (reset as u64 % 180) as f64
}

pub fn decide(l: &Limits, p: &Profile, now: f64) -> Decision {
    let windows = [("5-hour", l.five_hour, p.reserve_5h), ("7-day", l.seven_day, p.reserve_7d)];
    // A window whose reset time has passed starts again from zero.
    for (name, w, reserve) in windows {
        let Some(w) = w.filter(|w| w.resets_at > now) else { continue };
        let key = format!("{name}:{:.0}", w.resets_at);
        if !w.allowed {
            return Decision::Sleep { until: after(w.resets_at), reason: format!("the {name} limit is used up"), key };
        }
        if w.used_pct >= 100.0 - reserve {
            return Decision::Sleep {
                until: after(w.resets_at),
                reason: format!("{name} usage is {:.0}%, and {reserve:.0}% is kept for you", w.used_pct),
                key,
            };
        }
    }
    // Spread the week evenly: by the time a share of the week has passed, about that share of the
    // allowance may be used.
    if let Some(w) = l.seven_day.filter(|w| w.resets_at > now) {
        let left = ((w.resets_at - now) / 3600.0).clamp(0.0, WEEK_HOURS);
        let pace = (100.0 - p.reserve_7d) * (1.0 - left / WEEK_HOURS) + PACE_SLACK;
        if w.used_pct > pace {
            return Decision::Sleep {
                until: now + PACE_WAIT,
                reason: format!("ahead of the weekly pace (7-day usage {:.0}%, pace {:.0}%)", w.used_pct, pace.min(100.0)),
                // One notice per week: the pace check repeats every few minutes.
                key: format!("pace:{:.0}", w.resets_at),
            };
        }
    }
    Decision::Go
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(five: f64, seven: f64, status: &str, kind: &str) -> Value {
        json!({"type": "rate_limit_event", "rate_limit_info": {
            "status": status, "rateLimitType": kind,
            "unifiedWindows": {
                "five_hour": {"utilization": five, "resetsAt": 10_000.0, "status": "allowed"},
                "seven_day": {"utilization": seven, "resetsAt": 500_000.0, "status": "allowed"}}}})
    }

    #[test]
    fn events_give_both_windows_and_mark_the_refused_one() {
        let l = Limits::from_event(&event(0.08, 0.45, "allowed", "five_hour")).unwrap();
        assert_eq!(l.five_hour, Some(Window { used_pct: 8.0, resets_at: 10_000.0, allowed: true }));
        assert_eq!(l.seven_day.unwrap().used_pct, 45.0);
        assert!(!l.refused());
        let l = Limits::from_event(&event(1.0, 0.45, "rejected", "five_hour")).unwrap();
        assert!(l.refused() && !l.five_hour.unwrap().allowed && l.seven_day.unwrap().allowed);
        assert_eq!(Limits::from_event(&json!({"type": "result"})), None);
    }

    #[test]
    fn plans_parse_and_cheaper_plans_keep_more_back() {
        assert_eq!(Plan::parse(" MAX20 "), Some(Plan::Max20));
        assert_eq!(Plan::parse("auto"), None);
        let (pro, max) = (Plan::Pro.profile(), Plan::Max20.profile());
        assert!(pro.reserve_7d > max.reserve_7d && pro.max_turns < max.max_turns);
    }

    fn limits(five: f64, seven: f64, seven_resets: f64) -> Limits {
        Limits {
            five_hour: Some(Window { used_pct: five, resets_at: 10_000.0, allowed: true }),
            seven_day: Some(Window { used_pct: seven, resets_at: seven_resets, allowed: true }),
        }
    }

    #[test]
    fn work_sleeps_until_a_reset_when_the_reserve_is_reached() {
        let p = Plan::Max5.profile();
        let late_week = 5_000.0 + 3600.0;
        assert_eq!(decide(&limits(50.0, 40.0, late_week), &p, 5_000.0), Decision::Go);
        let Decision::Sleep { until, reason, .. } = decide(&limits(86.0, 40.0, late_week), &p, 5_000.0) else { panic!() };
        assert!(until > 10_000.0 && until < 10_300.0 && reason.contains("5-hour"), "{reason}");
        // After the reset time the old reading no longer counts.
        assert_eq!(decide(&limits(99.0, 40.0, late_week), &p, 10_001.0), Decision::Go);
        let mut refused = limits(10.0, 10.0, late_week);
        refused.seven_day.as_mut().unwrap().allowed = false;
        assert!(matches!(decide(&refused, &p, 5_000.0), Decision::Sleep { reason, .. } if reason.contains("7-day limit is used up")));
        assert_eq!(decide(&Limits::default(), &p, 5_000.0), Decision::Go);
    }

    #[test]
    fn the_week_is_spent_evenly() {
        let p = Plan::Pro.profile();
        let now = 1_000_000.0;
        // A day into the week: about 11% of the 75% allowance, plus slack.
        let day_in = now + 6.0 * 86_400.0;
        assert_eq!(decide(&limits(0.0, 15.0, day_in), &p, now), Decision::Go);
        let Decision::Sleep { until, reason, key } = decide(&limits(0.0, 30.0, day_in), &p, now) else { panic!() };
        assert_eq!(until, now + PACE_WAIT);
        assert!(reason.contains("weekly pace"), "{reason}");
        let Decision::Sleep { key: later, .. } = decide(&limits(0.0, 30.0, day_in), &p, now + 300.0) else { panic!() };
        assert_eq!(key, later, "the same pace sleep keeps its key, so it is announced once");
        // On the last day the whole allowance is open.
        assert_eq!(decide(&limits(0.0, 70.0, now + 3600.0), &p, now), Decision::Go);
    }
}

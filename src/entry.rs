//! One program, three names. The name it was started as (argv[0]) says which part answers:
//! - `marsad` watches and protects: the dashboard, status, usage, the safety guards.
//! - `jazari` acts and builds: products and tasks, Telegram, handoff, the prompt and skill
//!   library.
//! - `mamal` is the whole system: alone it opens the dashboard, `mamal build …` is jazari and
//!   `mamal watch …` (or any other command) is marsad.
//!
//! The installer ships the extra names as aliases of the `marsad` binary. Every `marsad …` command
//! line that worked before still works: Claude sessions and hooks run them. A command that moved to
//! jazari prints a one-line tip only when a person is at the terminal.

/// Which part handles the command, with the arguments for its parser (the first is the name shown
/// in help).
#[derive(Debug, PartialEq)]
pub enum Entry {
    Marsad(Vec<String>),
    Jazari(Vec<String>),
}

/// Commands that now live in jazari. `marsad <cmd>` still runs them.
pub const MOVED: [&str; 6] = ["jazari", "handoff", "prompt", "skill", "rules", "coordination"];

/// The program's own name from argv[0]: the file name without a folder or `.exe`, lower case.
pub fn name_of(argv0: &str) -> String {
    let file = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0).to_ascii_lowercase();
    file.strip_suffix(".exe").unwrap_or(&file).to_string()
}

/// Routes a command line. The second value is the tip to show a person when a moved command was
/// typed as `marsad <cmd>`.
pub fn route(argv: &[String]) -> (Entry, Option<String>) {
    let name = argv.first().map(|a| name_of(a)).unwrap_or_default();
    let rest: Vec<String> = argv.iter().skip(1).cloned().collect();
    let with = |head: &str, args: &[String]| std::iter::once(head.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>();
    match name.as_str() {
        "jazari" => (Entry::Jazari(with("jazari", &rest)), None),
        "mamal" => match rest.first().map(String::as_str) {
            Some("build") => (Entry::Jazari(with("mamal build", &rest[1..])), None),
            Some("watch") => (Entry::Marsad(with("mamal watch", &rest[1..])), None),
            _ => (Entry::Marsad(with("mamal", &rest)), None),
        },
        _ => {
            // `marsad jazari <cmd>` is the old spelling of `jazari <cmd>`; the other moved
            // commands run unchanged under marsad.
            let tip = rest.first().filter(|c| MOVED.contains(&c.as_str())).map(|c| {
                let now = if c == "jazari" { "jazari".to_string() } else { format!("jazari {c}") };
                format!("tip: `marsad {c}` is now `{now}` (both work)")
            });
            (Entry::Marsad(with("marsad", &rest)), tip)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_name_comes_from_argv0_on_every_platform() {
        assert_eq!(name_of("/usr/local/bin/jazari"), "jazari");
        assert_eq!(name_of(r"C:\Users\me\.cargo\bin\JAZARI.EXE"), "jazari");
        assert_eq!(name_of("Mamal.exe"), "mamal");
        assert_eq!(name_of("marsad"), "marsad");
        assert_eq!(name_of("target/debug/marsad"), "marsad");
    }

    #[test]
    fn each_name_reaches_its_part() {
        assert_eq!(route(&args("/bin/jazari new shop An idea")).0, Entry::Jazari(args("jazari new shop An idea")));
        assert_eq!(route(&args(r"C:\bin\jazari.exe handoff list")).0, Entry::Jazari(args("jazari handoff list")));
        assert_eq!(route(&args("jazari")).0, Entry::Jazari(args("jazari")));
        assert_eq!(route(&args("mamal")).0, Entry::Marsad(vec!["mamal".into()]));
        assert_eq!(route(&args("mamal build tasks")).0, Entry::Jazari(vec!["mamal build".into(), "tasks".into()]));
        assert_eq!(route(&args("mamal watch status --format json")).0, Entry::Marsad(vec!["mamal watch".into(), "status".into(), "--format".into(), "json".into()]));
        assert_eq!(route(&args("mamal status")).0, Entry::Marsad(args("mamal status")));
    }

    #[test]
    fn the_installer_ships_every_name_this_router_answers_to() {
        let dist = include_str!("../dist-workspace.toml");
        let aliases = dist.split("[dist.bin-aliases]").nth(1).expect("dist-workspace.toml has [dist.bin-aliases]");
        assert!(aliases.lines().any(|l| l.trim() == r#"marsad = ["jazari", "mamal"]"#), "{aliases}");
    }

    #[test]
    fn every_old_marsad_command_line_still_works_and_moved_ones_carry_a_tip() {
        // What Claude sessions and hooks are told to run: unchanged, with no tip for the guards.
        for line in ["marsad hook", "marsad guard --hook", "marsad statusline", "marsad clean --tier-a", "marsad system --wait", "marsad say hi", "marsad tell 1fe6 --stop", "marsad pause"] {
            let (entry, tip) = route(&args(line));
            assert_eq!(entry, Entry::Marsad(args(line)), "{line}");
            assert_eq!(tip, None, "{line}");
        }
        for line in ["marsad skill sync", "marsad rules", "marsad handoff list", "marsad prompt new", "marsad coordination", "marsad jazari notify hi"] {
            let (entry, tip) = route(&args(line));
            assert_eq!(entry, Entry::Marsad(args(line)), "still runs: {line}");
            assert!(tip.is_some(), "{line}");
        }
        assert_eq!(route(&args("marsad skill sync")).1.unwrap(), "tip: `marsad skill` is now `jazari skill` (both work)");
        assert_eq!(route(&args("marsad jazari tasks")).1.unwrap(), "tip: `marsad jazari` is now `jazari` (both work)");
    }
}

//! Git on the host, for a product's repo. The container never sees the repo itself:
//! - Each task gets a fresh clone in jazari's work folder, made by the host before the container
//!   touches it.
//! - The worker's commits come back as a git bundle, written inside the container, which the
//!   host fetches. A bundle is only data.
//! - The host never runs git in a folder the container could have written. Every host call
//!   turns off hooks and fsmonitor, so nothing in a repo's config or hooks runs on the host.
//! - An approved commit is merged into the default branch without touching a checkout that has
//!   uncommitted changes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the empty hooks folder lives: in the user's own data folder, which no container mounts
/// and no other user can write (a shared temp folder could be made first by someone else and
/// filled with hooks).
fn no_hooks_home() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("jazari-jazari-tests-{}", std::process::id())).join("no-hooks");
    }
    super::dir().join("no-hooks")
}

/// The empty folder `core.hooksPath` points at, so no hook runs on any platform. It is checked
/// before every use and made empty again if anything appeared in it; when that fails, no git
/// command runs.
fn no_hooks_at(dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    if std::fs::read_dir(dir)?.next().is_some() {
        std::fs::remove_dir_all(dir)?;
        std::fs::create_dir_all(dir)?;
    }
    anyhow::ensure!(std::fs::read_dir(dir)?.next().is_none(), "{} isn't empty, so git hooks can't be turned off safely", dir.display());
    Ok(dir.to_path_buf())
}

/// The settings every host git call runs with.
fn safety() -> anyhow::Result<Vec<String>> {
    let hooks = no_hooks_at(&no_hooks_home())?;
    // core.autocrlf off: the founder's own git settings must not rewrite what a worker wrote.
    let mut a: Vec<String> = ["-c", "user.name=jazari", "-c", "user.email=jazari@localhost", "-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false", "-c", "core.autocrlf=false"]
        .map(String::from)
        .to_vec();
    a.push("-c".into());
    a.push(format!("core.hooksPath={}", hooks.display()));
    Ok(a)
}

fn run(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(safety()?).args(args).stdin(Stdio::null()).output()?;
    anyhow::ensure!(out.status.success(), "git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn ok(dir: &Path, args: &[&str]) -> bool {
    let Ok(safety) = safety() else { return false };
    Command::new("git").arg("-C").arg(dir).args(safety).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// The branch work is based on and merged into: `main`, else `master`, else whatever the repo's
/// main checkout has.
pub fn default_branch(repo: &Path) -> anyhow::Result<String> {
    for b in ["main", "master"] {
        if ok(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{b}")]) {
            return Ok(b.into());
        }
    }
    run(repo, &["symbolic-ref", "--short", "HEAD"])
}

/// The commit a branch or revision points at, if it exists.
pub fn head(repo: &Path, rev: &str) -> Option<String> {
    run(repo, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).ok().filter(|s| !s.is_empty())
}

/// Only jazari's own branches may be written from a worker's bundle.
pub fn valid_branch(branch: &str) -> bool {
    branch.strip_prefix("jazari/task-").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Makes a fresh clone of the repo at `dest` with `branch` checked out: the existing branch, or a
/// new one from `base`. It runs before any container sees `dest`.
pub fn clone_for_task(repo: &Path, dest: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    let parent = dest.parent().ok_or_else(|| anyhow::anyhow!("no parent for {}", dest.display()))?;
    std::fs::create_dir_all(parent)?;
    run(parent, &["clone", "--quiet", "--no-local", "--no-checkout", &repo.to_string_lossy(), &dest.to_string_lossy()])?;
    let from = if head(repo, &format!("refs/heads/{branch}")).is_some() { format!("origin/{branch}") } else { format!("origin/{base}") };
    run(dest, &["checkout", "--quiet", "-B", branch, &from])?;
    Ok(())
}

/// A clone of the repo at `dest` with `commit` checked out (detached), for checks on a merge.
pub fn clone_at(repo: &Path, dest: &Path, commit: &str) -> anyhow::Result<()> {
    // A clone only carries what refs point at: point a scratch ref at the commit while cloning.
    let scratch = "refs/heads/jazari/checking";
    run(repo, &["update-ref", scratch, commit])?;
    let made = clone_for_task(repo, dest, "jazari/checking", "jazari/checking").and_then(|()| run(dest, &["checkout", "--quiet", "--detach", commit]).map(|_| ()));
    let _ = run(repo, &["update-ref", "-d", scratch]);
    made
}

/// Brings a worker's branch back from the bundle it wrote. Only `jazari/task-<n>` branches can be
/// written this way.
pub fn import_bundle(repo: &Path, bundle: &Path, branch: &str) -> anyhow::Result<()> {
    anyhow::ensure!(valid_branch(branch), "refusing to import {branch:?}: not a jazari task branch");
    let spec = format!("+refs/heads/{branch}:refs/heads/{branch}");
    run(repo, &["fetch", "--quiet", "--no-tags", "--no-write-fetch-head", &bundle.to_string_lossy(), &spec])?;
    Ok(())
}

pub fn commits_ahead(repo: &Path, base: &str, rev: &str) -> usize {
    run(repo, &["rev-list", "--count", &format!("{base}..{rev}")]).ok().and_then(|n| n.parse().ok()).unwrap_or(0)
}

pub fn changed_files(repo: &Path, base: &str, rev: &str) -> Vec<String> {
    run(repo, &["diff", "--name-only", &format!("{base}...{rev}")]).map(|o| o.lines().map(String::from).collect()).unwrap_or_default()
}

/// A file's text at a revision, if it is there.
pub fn show(repo: &Path, rev: &str, path: &str) -> Option<String> {
    run(repo, &["show", &format!("{rev}:{path}")]).ok()
}

/// Whether a change is only documentation (`*.md`), which skips code and security review.
pub fn docs_only(files: &[String]) -> bool {
    !files.is_empty() && files.iter().all(|f| f.ends_with(".md"))
}

/// Keeps `.jazari/` out of `git status`, in the repo's own exclude file.
fn exclude_jazari(repo: &Path) -> anyhow::Result<()> {
    let common = PathBuf::from(run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    let file = common.join("info").join("exclude");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == ".jazari/") {
        std::fs::create_dir_all(file.parent().expect("exclude has a parent"))?;
        std::fs::write(&file, format!("{text}{}.jazari/\n", if text.is_empty() || text.ends_with('\n') { "" } else { "\n" }))?;
    }
    Ok(())
}

/// Removes a worktree of the repo (the merge scratch).
fn remove_worktree(repo: &Path, path: &Path) {
    if path.exists() {
        let _ = run(repo, &["worktree", "remove", "--force", &path.to_string_lossy()]);
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = run(repo, &["worktree", "prune"]);
}

/// The folder where `branch` is checked out, if any.
fn checked_out(repo: &Path, branch: &str) -> Option<PathBuf> {
    let list = run(repo, &["worktree", "list", "--porcelain"]).ok()?;
    let mut path = None;
    for line in list.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if line == format!("branch refs/heads/{branch}") {
            return path;
        }
    }
    None
}

/// A merge commit built in a scratch worktree of the repo, not yet on the base branch.
#[derive(Debug, PartialEq)]
pub struct Staged {
    pub old: String,
    pub new: String,
    dir: PathBuf,
    /// The base branch's checkout, if it has one.
    checkout: Option<PathBuf>,
}

#[cfg(test)]
impl Staged {
    pub fn for_tests(new: &str) -> Self {
        Staged { old: "0".into(), new: new.into(), dir: PathBuf::new(), checkout: None }
    }
}

#[derive(Debug, PartialEq)]
pub enum Stage {
    Ready(Staged),
    Conflict,
    Dirty(PathBuf),
}

/// Builds the merge of `commit` into `base` in `<repo>/.jazari/merge`, leaving `base` alone. The
/// commit, not a branch name, is merged: it is exactly what the gates approved.
pub fn stage(repo: &Path, base: &str, commit: &str, message: &str) -> anyhow::Result<Stage> {
    exclude_jazari(repo)?;
    let checkout = checked_out(repo, base);
    if let Some(dir) = &checkout
        && !run(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty()
    {
        return Ok(Stage::Dirty(dir.clone()));
    }
    let old = run(repo, &["rev-parse", &format!("refs/heads/{base}")])?;
    let dir = repo.join(".jazari").join("merge");
    remove_worktree(repo, &dir);
    run(repo, &["worktree", "add", "--detach", &dir.to_string_lossy(), &old])?;
    if run(&dir, &["merge", "--no-ff", "-m", message, commit]).is_err() {
        let _ = run(&dir, &["merge", "--abort"]);
        remove_worktree(repo, &dir);
        return Ok(Stage::Conflict);
    }
    let new = run(&dir, &["rev-parse", "HEAD"])?;
    Ok(Stage::Ready(Staged { old, new, dir, checkout }))
}

/// Moves `base` to a staged merge:
/// - a clean checkout of it is fast-forwarded;
/// - with no checkout, the ref is moved only if it hasn't changed meanwhile.
pub fn advance(repo: &Path, base: &str, s: &Staged) -> anyhow::Result<String> {
    let moved = match &s.checkout {
        Some(dir) => run(dir, &["merge", "--ff-only", &s.new]).map(|_| ()),
        None => run(repo, &["update-ref", &format!("refs/heads/{base}"), &s.new, &s.old]).map(|_| ()),
    };
    remove_worktree(repo, &s.dir);
    moved.map(|()| s.new.clone())
}

/// Drops a staged merge.
pub fn discard(repo: &Path, s: &Staged) {
    remove_worktree(repo, &s.dir);
}

/// The files whose added lines look like credentials.
pub fn secrets(repo: &Path, base: &str, rev: &str) -> Vec<String> {
    let Ok(diff) = run(repo, &["diff", "--unified=0", "--no-color", &format!("{base}...{rev}")]) else { return Vec::new() };
    secrets_in(&diff)
}

pub fn secrets_in(diff: &str) -> Vec<String> {
    let mut file = String::new();
    let mut added = String::new();
    let mut hits: Vec<String> = Vec::new();
    let flush = |file: &str, added: &mut String, hits: &mut Vec<String>| {
        if !added.is_empty() && super::telegram::has_secret(added) && !hits.iter().any(|h| h == file) {
            hits.push(file.to_string());
        }
        added.clear();
    };
    for line in diff.lines() {
        if let Some(f) = line.strip_prefix("+++ b/") {
            flush(&file, &mut added, &mut hits);
            file = f.to_string();
        } else if let Some(a) = line.strip_prefix('+').filter(|_| !line.starts_with("+++")) {
            added.push_str(a);
            added.push('\n');
        }
    }
    flush(&file, &mut added, &mut hits);
    hits
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jazari-jazari-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.join("README.md"), "# x\n").unwrap();
        run(&dir, &["add", "README.md"]).unwrap();
        run(&dir, &["commit", "-qm", "init"]).unwrap();
        dir
    }

    pub fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", file]).unwrap();
        run(dir, &["commit", "-qm", file]).unwrap();
    }

    /// What a worker does in its clone, then what the harness does in the container: commit, and
    /// write the branch to a bundle.
    fn work(clone: &Path, branch: &str, since: &str, file: &str, text: &str) -> PathBuf {
        commit(clone, file, text);
        let bundle = clone.join(".jazari-out.bundle");
        run(clone, &["bundle", "create", &bundle.to_string_lossy(), branch, &format!("^{since}")]).unwrap();
        bundle
    }

    #[test]
    fn a_task_works_in_its_own_clone_and_comes_back_as_a_bundle() {
        let r = repo("clone");
        let base = head(&r, "main").unwrap();
        let clone = r.with_extension("work").join("task-1");
        clone_for_task(&r, &clone, "jazari/task-1", "main").unwrap();
        let bundle = work(&clone, "jazari/task-1", &base, "app.rs", "fn main() {}\n");
        assert_eq!(head(&r, "refs/heads/jazari/task-1"), None, "nothing reaches the repo until it is imported");
        import_bundle(&r, &bundle, "jazari/task-1").unwrap();
        assert_eq!(commits_ahead(&r, "main", "jazari/task-1"), 1);
        assert_eq!(changed_files(&r, "main", "jazari/task-1"), vec!["app.rs"]);
        assert_eq!(run(&r, &["symbolic-ref", "--short", "HEAD"]).unwrap(), "main", "the founder's checkout stays put");
        // The next task on the branch starts from its commits.
        let again = r.with_extension("work").join("task-2");
        clone_for_task(&r, &again, "jazari/task-1", "main").unwrap();
        assert!(again.join("app.rs").exists());
        assert!(import_bundle(&r, &bundle, "main").is_err(), "a bundle can't write the base branch");
        assert!(valid_branch("jazari/task-12") && !valid_branch("jazari/task-") && !valid_branch("refs/heads/main"));
        std::fs::remove_dir_all(r.with_extension("work")).ok();
        std::fs::remove_dir_all(r).ok();
    }

    #[test]
    fn hooks_planted_by_a_worker_never_run_on_the_host() {
        let r = repo("hooks");
        let base = head(&r, "main").unwrap();
        let clone = r.with_extension("work").join("task-1");
        clone_for_task(&r, &clone, "jazari/task-1", "main").unwrap();
        let marker = r.with_extension("pwned");
        let _ = std::fs::remove_file(&marker);
        // What a worker with a writable clone could plant.
        let hook = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        // Planted in the worker's clone and, as if it had reached it, in the repo itself: the
        // host's fetch, merge and ref updates fire these hooks unless they are turned off.
        for git_dir in [clone.join(".git"), r.join(".git")] {
            for name in ["post-checkout", "post-merge", "reference-transaction", "post-commit", "pre-commit", "pre-merge-commit"] {
                let path = git_dir.join("hooks").join(name);
                std::fs::write(&path, &hook).unwrap();
                #[cfg(unix)]
                std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
            }
        }
        for dir in [&clone, &r] {
            run(dir, &["config", "core.fsmonitor", &format!("touch '{}'", marker.display())]).unwrap();
        }
        // The harness's own git in the clone (commit, bundle), then the host's fetch and merge.
        let bundle = work(&clone, "jazari/task-1", &base, "a.txt", "a\n");
        import_bundle(&r, &bundle, "jazari/task-1").unwrap();
        let commit = head(&r, "refs/heads/jazari/task-1").unwrap();
        let Stage::Ready(staged) = stage(&r, "main", &commit, "m").unwrap() else { panic!() };
        advance(&r, "main", &staged).unwrap();
        assert!(!marker.exists(), "a planted hook or fsmonitor ran");
        std::fs::remove_dir_all(r.with_extension("work")).ok();
        std::fs::remove_dir_all(r).ok();
    }

    #[test]
    fn the_hooks_folder_is_emptied_before_use_and_git_refuses_to_run_without_it() {
        let dir = std::env::temp_dir().join(format!("jazari-jazari-nohooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(no_hooks_at(&dir).is_ok());
        std::fs::write(dir.join("post-checkout"), "#!/bin/sh\nrm -rf ~\n").unwrap();
        no_hooks_at(&dir).unwrap();
        assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "a planted hook is removed before git runs");
        // A file where the folder should be: it can't be made, so nothing runs.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::write(&dir, "not a folder").unwrap();
        assert!(no_hooks_at(&dir).is_err());
        std::fs::remove_file(&dir).ok();
        assert!(!no_hooks_home().starts_with("/tmp/jazari-jazari-no-hooks"));
    }

    #[test]
    fn the_approved_commit_merges_into_a_clean_checkout_and_a_dirty_one_waits() {
        let r = repo("merge");
        let base = head(&r, "main").unwrap();
        let clone = r.with_extension("work").join("task-2");
        clone_for_task(&r, &clone, "jazari/task-2", "main").unwrap();
        let bundle = work(&clone, "jazari/task-2", &base, "README.md", "# from the task\n");
        import_bundle(&r, &bundle, "jazari/task-2").unwrap();
        let approved = head(&r, "refs/heads/jazari/task-2").unwrap();
        std::fs::write(r.join("README.md"), "# founder editing\n").unwrap();
        let Stage::Dirty(dir) = stage(&r, "main", &approved, "m").unwrap() else { panic!("a dirty checkout must hold the merge") };
        assert_eq!(dir.file_name(), r.file_name());
        run(&r, &["checkout", "--quiet", "--", "README.md"]).unwrap();
        let Stage::Ready(staged) = stage(&r, "main", &approved, "Merge task 2").unwrap() else { panic!() };
        // The merge's checks run in a clone of exactly the staged commit.
        let check = r.with_extension("work").join("merge");
        clone_at(&r, &check, &staged.new).unwrap();
        assert_eq!(std::fs::read_to_string(check.join("README.md")).unwrap(), "# from the task\n");
        assert_eq!(head(&r, "refs/heads/jazari/checking"), None, "the scratch ref is gone");
        let sha = advance(&r, "main", &staged).unwrap();
        assert_eq!(run(&r, &["rev-parse", "HEAD"]).unwrap(), sha);
        assert_eq!(std::fs::read_to_string(r.join("README.md")).unwrap(), "# from the task\n");
        std::fs::remove_dir_all(r.with_extension("work")).ok();
        std::fs::remove_dir_all(r).ok();
    }

    #[test]
    fn a_conflict_stops_the_merge_and_a_base_with_no_checkout_moves_by_ref() {
        let r = repo("conflict");
        let base = head(&r, "main").unwrap();
        let clone = r.with_extension("work").join("task-3");
        clone_for_task(&r, &clone, "jazari/task-3", "main").unwrap();
        let bundle = work(&clone, "jazari/task-3", &base, "README.md", "# task\n");
        import_bundle(&r, &bundle, "jazari/task-3").unwrap();
        let approved = head(&r, "refs/heads/jazari/task-3").unwrap();
        commit(&r, "README.md", "# founder committed\n");
        assert_eq!(stage(&r, "main", &approved, "m").unwrap(), Stage::Conflict);
        run(&r, &["switch", "-q", "-c", "elsewhere"]).unwrap();
        let clone = r.with_extension("work").join("task-4");
        let main_now = head(&r, "main").unwrap();
        clone_for_task(&r, &clone, "jazari/task-4", "main").unwrap();
        let bundle = work(&clone, "jazari/task-4", &main_now, "b.txt", "b\n");
        import_bundle(&r, &bundle, "jazari/task-4").unwrap();
        let Stage::Ready(staged) = stage(&r, "main", &head(&r, "refs/heads/jazari/task-4").unwrap(), "m").unwrap() else { panic!() };
        advance(&r, "main", &staged).unwrap();
        assert_eq!(commits_ahead(&r, "elsewhere", "main"), 2);
        assert!(docs_only(&["company/VISION.md".into(), "README.md".into()]) && !docs_only(&["company/checks.sh".into()]) && !docs_only(&[]));
        assert_eq!(show(&r, "main", "b.txt").as_deref(), Some("b"));
        std::fs::remove_dir_all(r.with_extension("work")).ok();
        std::fs::remove_dir_all(r).ok();
    }

    #[test]
    fn credentials_in_added_lines_are_found_per_file() {
        let diff = "+++ b/config.py\n+API_KEY = \"sk-ant-api03-abcdefghijklmnopqrstuv\"\n+++ b/app.py\n+x = 1\n+++ b/key.pem\n+-----BEGIN RSA PRIVATE KEY-----\n+MIIEowIBAAKCAQEA7\n+-----END RSA PRIVATE KEY-----\n";
        assert_eq!(secrets_in(diff), vec!["config.py", "key.pem"]);
    }
}

//! Git on the host, for a product's repo.
//! - Each task gets its own worktree under `<repo>/.jazari/worktrees/`, so a worker never
//!   switches the founder's checkout.
//! - An approved branch is merged into the default branch without touching a checkout that has
//!   uncommitted changes.
//!
//! The container mounts the repo at the same path as on the host, so a worktree's links work on
//! both sides.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const IDENTITY: [&str; 6] = ["-c", "user.name=jazari", "-c", "user.email=jazari@localhost", "-c", "commit.gpgsign=false"];

fn git(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(IDENTITY).args(args).stdin(Stdio::null()).output()?;
    anyhow::ensure!(out.status.success(), "git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn ok(repo: &Path, args: &[&str]) -> bool {
    Command::new("git").arg("-C").arg(repo).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// The branch work is based on and merged into: `main`, else `master`, else whatever the repo's
/// main checkout has.
pub fn default_branch(repo: &Path) -> anyhow::Result<String> {
    for b in ["main", "master"] {
        if ok(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{b}")]) {
            return Ok(b.into());
        }
    }
    git(repo, &["symbolic-ref", "--short", "HEAD"])
}

pub fn worktree_path(repo: &Path, task: i64) -> PathBuf {
    repo.join(".jazari").join("worktrees").join(format!("task-{task}"))
}

/// Keeps `.jazari/` out of `git status`, in the repo's own exclude file.
fn exclude_jazari(repo: &Path) -> anyhow::Result<()> {
    let common = PathBuf::from(git(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    let file = common.join("info").join("exclude");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    if !text.lines().any(|l| l.trim() == ".jazari/") {
        std::fs::create_dir_all(file.parent().expect("exclude has a parent"))?;
        std::fs::write(&file, format!("{text}{}.jazari/\n", if text.is_empty() || text.ends_with('\n') { "" } else { "\n" }))?;
    }
    Ok(())
}

/// A fresh worktree for the task on `branch`: the existing branch, or a new one from `base`.
pub fn prepare(repo: &Path, task: i64, branch: &str, base: &str) -> anyhow::Result<PathBuf> {
    exclude_jazari(repo)?;
    let path = worktree_path(repo, task);
    remove(repo, &path);
    let p = path.to_string_lossy();
    if ok(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]) {
        git(repo, &["worktree", "add", &p, branch])?;
    } else {
        git(repo, &["worktree", "add", "-b", branch, &p, base])?;
    }
    Ok(path)
}

/// Removes a task's worktree; its branch and commits stay.
pub fn remove(repo: &Path, path: &Path) {
    if path.exists() {
        let _ = git(repo, &["worktree", "remove", "--force", &path.to_string_lossy()]);
        let _ = std::fs::remove_dir_all(path);
    }
    let _ = git(repo, &["worktree", "prune"]);
}

pub fn commits_ahead(repo: &Path, base: &str, branch: &str) -> usize {
    git(repo, &["rev-list", "--count", &format!("{base}..{branch}")]).ok().and_then(|n| n.parse().ok()).unwrap_or(0)
}

pub fn changed_files(repo: &Path, base: &str, branch: &str) -> Vec<String> {
    git(repo, &["diff", "--name-only", &format!("{base}...{branch}")]).map(|o| o.lines().map(String::from).collect()).unwrap_or_default()
}

/// Whether a change is only documentation, which skips the security review.
pub fn docs_only(files: &[String]) -> bool {
    !files.is_empty() && files.iter().all(|f| f.ends_with(".md") || f.ends_with(".txt") || f.starts_with("company/"))
}

#[cfg(test)]
#[derive(Debug, PartialEq)]
pub enum Merge {
    Merged(String),
    /// The branches conflict; the worker must merge the base into its branch first.
    Conflict,
    /// The base is checked out with uncommitted changes, which a merge would disturb.
    Dirty(PathBuf),
}

/// The folder where `branch` is checked out, if any.
fn checked_out(repo: &Path, branch: &str) -> Option<PathBuf> {
    let list = git(repo, &["worktree", "list", "--porcelain"]).ok()?;
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

/// A merge commit built in a scratch worktree, not yet on the base branch.
#[derive(Debug, PartialEq)]
pub struct Staged {
    /// The scratch worktree holding the merge, where checks can run on it.
    pub dir: PathBuf,
    pub old: String,
    pub new: String,
    /// The base branch's checkout, if it has one.
    checkout: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum Stage {
    Ready(Staged),
    Conflict,
    Dirty(PathBuf),
}

#[cfg(test)]
impl Staged {
    pub fn for_tests(dir: &str, new: &str) -> Self {
        Staged { dir: dir.into(), old: "0".into(), new: new.into(), checkout: None }
    }
}

/// Builds the merge of `branch` into `base` in `<repo>/.jazari/merge`, leaving `base` alone.
pub fn stage(repo: &Path, base: &str, branch: &str, message: &str) -> anyhow::Result<Stage> {
    exclude_jazari(repo)?;
    let checkout = checked_out(repo, base);
    if let Some(dir) = &checkout
        && !git(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty()
    {
        return Ok(Stage::Dirty(dir.clone()));
    }
    let old = git(repo, &["rev-parse", &format!("refs/heads/{base}")])?;
    let dir = repo.join(".jazari").join("merge");
    remove(repo, &dir);
    git(repo, &["worktree", "add", "--detach", &dir.to_string_lossy(), &old])?;
    if git(&dir, &["merge", "--no-ff", "-m", message, branch]).is_err() {
        let _ = git(&dir, &["merge", "--abort"]);
        remove(repo, &dir);
        return Ok(Stage::Conflict);
    }
    let new = git(&dir, &["rev-parse", "HEAD"])?;
    Ok(Stage::Ready(Staged { dir, old, new, checkout }))
}

/// Moves `base` to a staged merge:
/// - a clean checkout of it is fast-forwarded;
/// - with no checkout, the ref is moved only if it hasn't changed meanwhile.
pub fn advance(repo: &Path, base: &str, s: &Staged) -> anyhow::Result<String> {
    let moved = match &s.checkout {
        Some(dir) => git(dir, &["merge", "--ff-only", &s.new]).map(|_| ()),
        None => git(repo, &["update-ref", &format!("refs/heads/{base}"), &s.new, &s.old]).map(|_| ()),
    };
    remove(repo, &s.dir);
    moved.map(|()| s.new.clone())
}

/// Drops a staged merge.
pub fn discard(repo: &Path, s: &Staged) {
    remove(repo, &s.dir);
}

/// Merges `branch` into `base` with a merge commit, without running checks on it.
#[cfg(test)]
pub fn merge(repo: &Path, base: &str, branch: &str, message: &str) -> anyhow::Result<Merge> {
    Ok(match stage(repo, base, branch, message)? {
        Stage::Ready(s) => Merge::Merged(advance(repo, base, &s)?),
        Stage::Conflict => Merge::Conflict,
        Stage::Dirty(d) => Merge::Dirty(d),
    })
}

/// The files whose added lines look like credentials.
pub fn secrets(repo: &Path, base: &str, branch: &str) -> Vec<String> {
    let Ok(diff) = git(repo, &["diff", "--unified=0", "--no-color", &format!("{base}...{branch}")]) else { return Vec::new() };
    secrets_in(&diff)
}

pub fn secrets_in(diff: &str) -> Vec<String> {
    let mut file = String::new();
    let mut hits: Vec<String> = Vec::new();
    for line in diff.lines() {
        if let Some(f) = line.strip_prefix("+++ b/") {
            file = f.to_string();
        } else if let Some(added) = line.strip_prefix('+')
            && !line.starts_with("+++")
            && super::telegram::redact(added) != added
            && !hits.contains(&file)
        {
            hits.push(file.clone());
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("marsad-jazari-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(dir.join("README.md"), "# x\n").unwrap();
        git(&dir, &["add", "README.md"]).unwrap();
        git(&dir, &["commit", "-qm", "init"]).unwrap();
        dir
    }

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]).unwrap();
        git(dir, &["commit", "-qm", file]).unwrap();
    }

    #[test]
    fn a_task_works_in_its_own_worktree_and_merges_into_a_clean_checkout() {
        let r = repo("clean");
        assert_eq!(default_branch(&r).unwrap(), "main");
        let wt = prepare(&r, 1, "jazari/task-1", "main").unwrap();
        commit(&wt, "app.rs", "fn main() {}\n");
        assert_eq!(git(&r, &["symbolic-ref", "--short", "HEAD"]).unwrap(), "main", "the founder's checkout stays put");
        assert!(git(&r, &["status", "--porcelain"]).unwrap().is_empty(), ".jazari/ is excluded");
        remove(&r, &wt);
        assert!(!wt.exists());
        assert_eq!(commits_ahead(&r, "main", "jazari/task-1"), 1);
        assert_eq!(changed_files(&r, "main", "jazari/task-1"), vec!["app.rs"]);
        // Preparing again reuses the branch and its commits.
        let wt = prepare(&r, 1, "jazari/task-1", "main").unwrap();
        assert!(wt.join("app.rs").exists());
        remove(&r, &wt);
        let Merge::Merged(sha) = merge(&r, "main", "jazari/task-1", "Merge task 1").unwrap() else { panic!() };
        assert_eq!(git(&r, &["rev-parse", "HEAD"]).unwrap(), sha);
        assert!(r.join("app.rs").exists(), "the clean checkout moved forward");
        std::fs::remove_dir_all(r).ok();
    }

    #[test]
    fn a_dirty_checkout_and_a_conflict_stop_the_merge() {
        let r = repo("dirty");
        let wt = prepare(&r, 2, "jazari/task-2", "main").unwrap();
        commit(&wt, "README.md", "# from the task\n");
        remove(&r, &wt);
        std::fs::write(r.join("README.md"), "# founder editing\n").unwrap();
        let Merge::Dirty(dir) = merge(&r, "main", "jazari/task-2", "m").unwrap() else { panic!("a dirty checkout must hold the merge") };
        assert_eq!(dir.file_name(), r.file_name(), "it names the founder's checkout");
        commit(&r, "README.md", "# founder committed\n");
        assert_eq!(merge(&r, "main", "jazari/task-2", "m").unwrap(), Merge::Conflict);
        assert_eq!(git(&r, &["log", "-1", "--format=%s"]).unwrap(), "README.md");
        std::fs::remove_dir_all(r).ok();
    }

    #[test]
    fn a_base_with_no_checkout_moves_by_ref() {
        let r = repo("noref");
        git(&r, &["switch", "-q", "-c", "elsewhere"]).unwrap();
        let wt = prepare(&r, 3, "jazari/task-3", "main").unwrap();
        commit(&wt, "a.txt", "a\n");
        remove(&r, &wt);
        assert!(matches!(merge(&r, "main", "jazari/task-3", "m").unwrap(), Merge::Merged(_)));
        assert_eq!(commits_ahead(&r, "elsewhere", "main"), 2);
        assert!(docs_only(&["company/VISION.md".into(), "README.md".into()]) && !docs_only(&["src/a.rs".into()]) && !docs_only(&[]));
        let wt = prepare(&r, 4, "jazari/task-4", "main").unwrap();
        commit(&wt, "config.py", "TOKEN = 1\nAPI_KEY=sk-ant-api03-abcdefghijklmnopqrstuv\n");
        remove(&r, &wt);
        assert_eq!(secrets(&r, "main", "jazari/task-4"), vec!["config.py"]);
        let Stage::Ready(staged) = stage(&r, "main", "jazari/task-4", "m").unwrap() else { panic!() };
        assert!(staged.dir.join("config.py").exists(), "checks can run on the staged merge");
        let before = git(&r, &["rev-parse", "main"]).unwrap();
        discard(&r, &staged);
        assert_eq!(git(&r, &["rev-parse", "main"]).unwrap(), before, "a discarded merge leaves main alone");
        std::fs::remove_dir_all(r).ok();
    }
}

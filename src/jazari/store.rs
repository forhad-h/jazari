//! jazari's state in SQLite (`~/.marsad/jazari/jazari.db`): products, the task queue, every worker
//! run, and small values such as the last usage-limit reading.

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS products (
    name TEXT PRIMARY KEY,
    repo TEXT NOT NULL,
    created REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY,
    product TEXT NOT NULL REFERENCES products(name),
    role TEXT NOT NULL,
    title TEXT NOT NULL,
    brief TEXT NOT NULL DEFAULT '',
    priority INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'queued',
    attempts INTEGER NOT NULL DEFAULT 0,
    parent INTEGER,
    summary TEXT NOT NULL DEFAULT '',
    branch TEXT,
    created REAL NOT NULL,
    updated REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_queue ON tasks(status, priority DESC, id);
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY,
    task INTEGER NOT NULL REFERENCES tasks(id),
    started REAL NOT NULL,
    ended REAL NOT NULL,
    outcome TEXT NOT NULL,
    session TEXT,
    input INTEGER NOT NULL DEFAULT 0,
    output INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    used_5h REAL,
    used_7d REAL
);
CREATE TABLE IF NOT EXISTS evals (
    id INTEGER PRIMARY KEY,
    task INTEGER NOT NULL,
    role TEXT NOT NULL,
    stage TEXT NOT NULL,
    passed INTEGER NOT NULL,
    score INTEGER,
    failed TEXT NOT NULL DEFAULT '',
    at REAL NOT NULL,
    version TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS evals_role ON evals(role, at);
CREATE TABLE IF NOT EXISTS signals (
    id INTEGER PRIMARY KEY,
    product TEXT NOT NULL,
    source TEXT NOT NULL,
    text TEXT NOT NULL,
    at REAL NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS prefs (
    id INTEGER PRIMARY KEY,
    product TEXT NOT NULL,
    area TEXT NOT NULL,
    rule TEXT NOT NULL,
    explicit INTEGER NOT NULL DEFAULT 0,
    count INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL,
    supersedes INTEGER,
    created REAL NOT NULL,
    last_seen REAL NOT NULL,
    note TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS lessons (
    id INTEGER PRIMARY KEY,
    product TEXT NOT NULL,
    area TEXT NOT NULL,
    lesson TEXT NOT NULL,
    task INTEGER,
    at REAL NOT NULL,
    recorded INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Columns added after the first schema, so older databases gain them.
const NEW_COLUMNS: [(&str, &str); 10] = [
    ("kind", "TEXT NOT NULL DEFAULT 'work'"),
    ("owner_role", "TEXT"),
    ("round", "INTEGER NOT NULL DEFAULT 0"),
    ("question", "TEXT"),
    ("approval", "TEXT"),
    ("after_task", "INTEGER"),
    ("landed", "INTEGER NOT NULL DEFAULT 0"),
    ("spec", "TEXT NOT NULL DEFAULT ''"),
    ("head", "TEXT"),
    ("hard_line", "INTEGER NOT NULL DEFAULT 0"),
];

/// What a task is: `work` builds; `eval`, `review` and `security` judge a branch before it merges.
pub const KINDS: [&str; 4] = ["work", "eval", "review", "security"];

pub const STATUSES: [&str; 5] = ["queued", "running", "done", "blocked", "failed"];

#[derive(Clone, Debug, PartialEq)]
pub struct Product {
    pub name: String,
    pub repo: String,
    /// `normal`: stop when the founder's tasks are done. `autonomous`: keep improving it.
    pub mode: String,
}

pub const MODES: [&str; 2] = ["normal", "autonomous"];

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: i64,
    pub product: String,
    pub role: String,
    pub title: String,
    pub brief: String,
    pub priority: i64,
    pub status: String,
    pub attempts: i64,
    pub summary: String,
    pub branch: Option<String>,
    pub kind: String,
    pub parent: Option<i64>,
    /// For reviews and fixes: the role that built the branch.
    pub owner_role: Option<String>,
    /// How many review rounds the branch has been through.
    pub round: i64,
    /// A question for the founder, while the task waits for the answer.
    pub question: Option<String>,
    /// `deploy`, `spend` or `contact` and what it is, while the task waits for approval.
    pub approval: Option<String>,
    /// A task whose work must be on the default branch before this one starts.
    pub after_task: Option<i64>,
    /// The original request and its acceptance criteria, carried through evaluation, review and
    /// fixes.
    pub spec: String,
    /// For judgments and merges: the exact commit the gates are about.
    pub head: Option<String>,
}

/// A task to queue. `kind` defaults to `work`.
#[derive(Clone, Debug, Default)]
pub struct NewTask<'a> {
    pub product: &'a str,
    pub role: &'a str,
    pub title: &'a str,
    pub brief: &'a str,
    pub priority: i64,
    pub parent: Option<i64>,
    pub kind: &'a str,
    pub branch: Option<&'a str>,
    pub owner_role: Option<&'a str>,
    pub round: i64,
    pub after: Option<i64>,
    /// Empty: the brief is the spec.
    pub spec: &'a str,
    pub head: Option<&'a str>,
}

/// Something the founder said that might hold a preference.
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub id: i64,
    pub product: String,
    pub source: String,
    pub text: String,
}

/// A preference of the founder's.
/// - `candidate`: seen, not yet enforced.
/// - `active`: enforced by the evaluator.
/// - `suggested`: not confirmed for a while, so followed but not enforced.
/// - `retired`: replaced or forgotten.
/// - `refused`: it would weaken quality or security, so it is never learned.
#[derive(Clone, Debug, PartialEq)]
pub struct Pref {
    pub id: i64,
    /// A product name, or `*` for every product.
    pub product: String,
    pub area: String,
    pub rule: String,
    pub explicit: bool,
    pub count: i64,
    pub status: String,
    pub supersedes: Option<i64>,
    pub last_seen: f64,
    pub note: String,
}

pub const PREF_STATUSES: [&str; 5] = ["candidate", "active", "suggested", "retired", "refused"];

fn pref_from(r: &rusqlite::Row) -> rusqlite::Result<Pref> {
    Ok(Pref {
        id: r.get(0)?,
        product: r.get(1)?,
        area: r.get(2)?,
        rule: r.get(3)?,
        explicit: r.get(4)?,
        count: r.get(5)?,
        status: r.get(6)?,
        supersedes: r.get(7)?,
        last_seen: r.get(8)?,
        note: r.get(9)?,
    })
}

const PREF_COLUMNS: &str = "id, product, area, rule, explicit, count, status, supersedes, last_seen, note";

/// A lesson the team drew from rework.
#[derive(Clone, Debug, PartialEq)]
pub struct Lesson {
    pub id: i64,
    pub product: String,
    pub area: String,
    pub lesson: String,
}

/// How a role fares at one gate: (role, stage, judgments, passed, average score).
pub type Quality = (String, String, i64, i64, Option<f64>);

/// One finished worker run, for the record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Run {
    pub task: i64,
    pub started: f64,
    pub ended: f64,
    pub outcome: String,
    pub session: Option<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub used_5h: Option<f64>,
    pub used_7d: Option<f64>,
}

pub struct Store {
    conn: Connection,
}

/// Whether a reply picks the recommended option of a stored question: its label, or its number
/// ("option 2", "2").
fn follows_recommendation(question: &str, reply: &str) -> bool {
    let Some(rec) = question.lines().find_map(|l| l.strip_prefix("Recommended: ")) else { return false };
    let reply = reply.trim().to_lowercase();
    let number = question.lines().find_map(|l| {
        let (n, rest) = l.split_once(". ")?;
        (n.chars().all(|c| c.is_ascii_digit()) && rest.split(": ").next()? == rec).then(|| n.to_string())
    });
    reply == rec.to_lowercase() || number.is_some_and(|n| reply == n || reply == format!("option {n}"))
}

/// A title's meaningful words: lowercase, 3+ letters, without filler.
pub fn title_words(t: &str) -> std::collections::BTreeSet<String> {
    const FILLER: [&str; 12] = ["the", "and", "for", "with", "when", "into", "from", "that", "this", "add", "fix", "once"];
    t.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).filter(|w| w.len() >= 3 && !FILLER.contains(&w.as_str())).collect()
}

/// How much two word sets share, against the smaller one.
pub fn overlap(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> f64 {
    let small = a.len().min(b.len());
    if small < 2 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / small as f64
}

/// A product name is also a container and branch name: lowercase letters, digits and dashes.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !name.starts_with('-')
}

fn task_from(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        product: r.get(1)?,
        role: r.get(2)?,
        title: r.get(3)?,
        brief: r.get(4)?,
        priority: r.get(5)?,
        status: r.get(6)?,
        attempts: r.get(7)?,
        summary: r.get(8)?,
        branch: r.get(9)?,
        kind: r.get(10)?,
        parent: r.get(11)?,
        owner_role: r.get(12)?,
        round: r.get(13)?,
        question: r.get(14)?,
        approval: r.get(15)?,
        after_task: r.get(16)?,
        spec: r.get(17)?,
        head: r.get(18)?,
    })
}

const TASK_COLUMNS: &str = "id, product, role, title, brief, priority, status, attempts, summary, branch, kind, parent, owner_role, round, question, approval, after_task, spec, head";

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        let product_cols: Vec<String> = conn.prepare("SELECT name FROM pragma_table_info('products')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        if !product_cols.iter().any(|c| c == "mode") {
            conn.execute_batch("ALTER TABLE products ADD COLUMN mode TEXT NOT NULL DEFAULT 'normal'")?;
        }
        let eval_cols: Vec<String> = conn.prepare("SELECT name FROM pragma_table_info('evals')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        if !eval_cols.iter().any(|c| c == "version") {
            conn.execute_batch("ALTER TABLE evals ADD COLUMN version TEXT NOT NULL DEFAULT ''")?;
        }
        let have: Vec<String> = conn.prepare("SELECT name FROM pragma_table_info('tasks')")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for (col, ddl) in NEW_COLUMNS {
            if !have.iter().any(|h| h == col) {
                conn.execute_batch(&format!("ALTER TABLE tasks ADD COLUMN {col} {ddl}"))?;
            }
        }
        Ok(Store { conn })
    }

    pub fn add_product(&self, name: &str, repo: &str, now: f64) -> anyhow::Result<()> {
        anyhow::ensure!(valid_name(name), "a product name uses lowercase letters, digits and dashes: {name:?}");
        let n = self.conn.execute("INSERT OR IGNORE INTO products (name, repo, created) VALUES (?1, ?2, ?3)", params![name, repo, now])?;
        anyhow::ensure!(n == 1, "there is already a product called {name}");
        Ok(())
    }

    pub fn product(&self, name: &str) -> anyhow::Result<Option<Product>> {
        Ok(self
            .conn
            .query_row("SELECT name, repo, mode FROM products WHERE name = ?1", [name], |r| Ok(Product { name: r.get(0)?, repo: r.get(1)?, mode: r.get(2)? }))
            .optional()?)
    }

    pub fn products(&self) -> anyhow::Result<Vec<Product>> {
        let mut st = self.conn.prepare("SELECT name, repo, mode FROM products ORDER BY name")?;
        let rows = st.query_map([], |r| Ok(Product { name: r.get(0)?, repo: r.get(1)?, mode: r.get(2)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_mode(&self, product: &str, mode: &str) -> anyhow::Result<()> {
        anyhow::ensure!(MODES.contains(&mode), "the mode is `normal` or `autonomous`");
        let n = self.conn.execute("UPDATE products SET mode = ?2 WHERE name = ?1", params![product, mode])?;
        anyhow::ensure!(n == 1, "no product called {product}");
        Ok(())
    }

    /// Whether a product has tasks queued or running.
    pub fn busy(&self, product: &str) -> anyhow::Result<bool> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM tasks WHERE product = ?1 AND status IN ('queued', 'running')", [product], |r| r.get::<_, i64>(0))? > 0)
    }

    pub fn add_task(&self, t: &NewTask, now: f64) -> anyhow::Result<i64> {
        anyhow::ensure!(self.product(t.product)?.is_some(), "no product called {}; add it with `marsad jazari product add`", t.product);
        anyhow::ensure!(!t.title.trim().is_empty(), "the task needs a title");
        let kind = if t.kind.is_empty() { "work" } else { t.kind };
        anyhow::ensure!(KINDS.contains(&kind), "not a task kind: {kind}");
        self.conn.execute(
            "INSERT INTO tasks (product, role, title, brief, priority, parent, kind, branch, owner_role, round, after_task, spec, head, created, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14)",
            params![
                t.product, t.role, t.title.trim(), t.brief.trim(), t.priority, t.parent, kind, t.branch, t.owner_role, t.round, t.after,
                if t.spec.trim().is_empty() { t.brief.trim() } else { t.spec.trim() }, t.head, now
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Sets a task aside until the founder answers a question or decides on an approval.
    pub fn wait_for_founder(&self, id: i64, question: Option<&str>, approval: Option<&str>, now: f64) -> anyhow::Result<()> {
        self.wait_for_founder_on(id, question, approval, false, now)
    }

    /// Like [`Store::wait_for_founder`]. With `hard_line`, choosing anything but the recommended
    /// option needs a written override (`override: <reason>`).
    pub fn wait_for_founder_on(&self, id: i64, question: Option<&str>, approval: Option<&str>, hard_line: bool, now: f64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status = 'blocked', question = ?2, approval = ?3, hard_line = ?4, updated = ?5 WHERE id = ?1",
            params![id, question, approval, hard_line, now],
        )?;
        Ok(())
    }

    /// The founder's reply to a blocked task: added to its brief, and the task goes back in the
    /// queue with fresh attempts.
    pub fn reply(&self, id: i64, reply: &str, now: f64) -> anyhow::Result<Task> {
        let t = self.task(id)?.ok_or_else(|| anyhow::anyhow!("no task #{id}"))?;
        anyhow::ensure!(t.status == "blocked", "task #{id} is {}, not waiting for you", t.status);
        let hard: bool = self.conn.query_row("SELECT hard_line FROM tasks WHERE id = ?1", [id], |r| r.get(0))?;
        if hard && !follows_recommendation(t.question.as_deref().unwrap_or(""), reply) {
            let given = reply.trim().to_ascii_lowercase();
            anyhow::ensure!(
                given.starts_with("override:") && given.len() > "override:".len() + 10,
                "this choice crosses a hard line (for example a known critical vulnerability, a secret in the code, or harm to user data). If you are sure, reply: /answer {id} override: <your reason>"
            );
        }
        let asked = t.question.as_deref().or(t.approval.as_deref()).map(|q| format!("You asked: {q}\n")).unwrap_or_default();
        let brief = format!("{}\n\n{asked}The founder replied: {}", t.brief, reply.trim());
        self.conn.execute(
            "UPDATE tasks SET status = 'queued', brief = ?2, question = NULL, approval = NULL, attempts = 0, updated = ?3 WHERE id = ?1",
            params![id, brief.trim(), now],
        )?;
        Ok(t)
    }

    pub fn task(&self, id: i64) -> anyhow::Result<Option<Task>> {
        Ok(self.conn.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id], task_from).optional()?)
    }

    /// The queued task to run next: highest priority first, then oldest, among those whose
    /// `after_task` has landed.
    #[cfg(test)]
    pub fn next_task(&self) -> anyhow::Result<Option<Task>> {
        self.next_task_except(&[])
    }

    /// The next task, skipping the products listed (held after a failure to set them up).
    pub fn next_task_except(&self, held: &[String]) -> anyhow::Result<Option<Task>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE status = 'queued'
             AND (after_task IS NULL OR after_task IN (SELECT id FROM tasks WHERE landed = 1))
             ORDER BY priority DESC, id"
        ))?;
        let mut rows = st.query_map([], task_from)?;
        while let Some(t) = rows.next().transpose()? {
            if !held.contains(&t.product) {
                return Ok(Some(t));
            }
        }
        Ok(None)
    }

    /// A task's work is on the default branch (or it had nothing to merge).
    pub fn land(&self, id: i64) -> anyhow::Result<()> {
        self.conn.execute("UPDATE tasks SET landed = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Every task that worked on `branch` has landed: the branch is merged.
    pub fn land_branch(&self, branch: &str) -> anyhow::Result<()> {
        self.conn.execute("UPDATE tasks SET landed = 1 WHERE branch = ?1", [branch])?;
        Ok(())
    }

    pub fn tasks(&self, product: Option<&str>, limit: usize) -> anyhow::Result<Vec<Task>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE ?1 IS NULL OR product = ?1
             ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'blocked' THEN 1 WHEN 'queued' THEN 2 ELSE 3 END, priority DESC, id DESC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![product, limit as i64], task_from)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_status(&self, id: i64, status: &str, summary: Option<&str>, branch: Option<&str>, now: f64) -> anyhow::Result<()> {
        anyhow::ensure!(STATUSES.contains(&status), "not a task status: {status}");
        self.conn.execute(
            "UPDATE tasks SET status = ?2, summary = COALESCE(?3, summary), branch = COALESCE(?4, branch), updated = ?5 WHERE id = ?1",
            params![id, status, summary, branch, now],
        )?;
        Ok(())
    }

    /// Counts one more failed attempt and returns the new count.
    pub fn bump_attempts(&self, id: i64) -> anyhow::Result<i64> {
        self.conn.execute("UPDATE tasks SET attempts = attempts + 1 WHERE id = ?1", [id])?;
        Ok(self.conn.query_row("SELECT attempts FROM tasks WHERE id = ?1", [id], |r| r.get(0))?)
    }

    /// Tasks left `running` by a supervisor that died go back in the queue.
    pub fn requeue_running(&self, now: f64) -> anyhow::Result<usize> {
        Ok(self.conn.execute("UPDATE tasks SET status = 'queued', updated = ?1 WHERE status = 'running'", [now])?)
    }

    pub fn add_run(&self, r: &Run) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO runs (task, started, ended, outcome, session, input, output, cache_read, used_5h, used_7d)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![r.task, r.started, r.ended, r.outcome, r.session, r.input, r.output, r.cache_read, r.used_5h, r.used_7d],
        )?;
        Ok(())
    }

    /// Whether the product already has this task: the same title (ignoring case and punctuation)
    /// anywhere, or a title sharing most of its words with a task still open.
    pub fn has_similar(&self, product: &str, title: &str) -> anyhow::Result<bool> {
        let norm = |t: &str| t.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let want = norm(title);
        let words = title_words(title);
        let mut st = self.conn.prepare("SELECT title, status FROM tasks WHERE product = ?1 AND status != 'failed'")?;
        let rows = st.query_map([product], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows.iter().any(|(t, status)| norm(t) == want || (status != "done" && overlap(&words, &title_words(t)) >= 0.6)))
    }

    /// Queued tasks of a product.
    pub fn queued(&self, product: &str) -> anyhow::Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM tasks WHERE product = ?1 AND status = 'queued'", [product], |r| r.get(0))?)
    }

    /// Records one judgment of a role's work: by the evaluator, a reviewer or the harness checks.
    /// `version` is the role's prompt version when it did the work.
    #[allow(clippy::too_many_arguments)]
    pub fn add_eval(&self, task: i64, role: &str, stage: &str, passed: bool, score: Option<i64>, failed: &str, now: f64, version: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO evals (task, role, stage, passed, score, failed, at, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![task, role, stage, passed, score, failed, now, version],
        )?;
        Ok(())
    }

    /// A role's evaluator judgments split by version: (passed, total) for `current`, then for
    /// every other version.
    pub fn version_split(&self, role: &str, current: &str) -> anyhow::Result<(i64, i64, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN version = ?2 THEN passed END), 0), COUNT(CASE WHEN version = ?2 THEN 1 END),
                    COALESCE(SUM(CASE WHEN version != ?2 THEN passed END), 0), COUNT(CASE WHEN version != ?2 THEN 1 END)
             FROM evals WHERE role = ?1 AND stage = 'eval'",
            params![role, current],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    }

    /// The role's last `n` evaluator judgments, newest first: (passed, score).
    pub fn recent_evals(&self, role: &str, n: usize) -> anyhow::Result<Vec<(bool, Option<i64>)>> {
        let mut st = self.conn.prepare("SELECT passed, score FROM evals WHERE role = ?1 AND stage = 'eval' ORDER BY at DESC, id DESC LIMIT ?2")?;
        let rows = st.query_map(params![role, n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Per role and stage: judgments, how many passed, the average score.
    pub fn quality(&self) -> anyhow::Result<Vec<Quality>> {
        let mut st = self.conn.prepare(
            "SELECT role, stage, COUNT(*), SUM(passed), AVG(score) FROM evals GROUP BY role, stage ORDER BY role, stage",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    // ---------- the founder's signals and preferences ----------

    pub fn add_signal(&self, product: &str, source: &str, text: &str, now: f64) -> anyhow::Result<i64> {
        self.conn.execute("INSERT INTO signals (product, source, text, at) VALUES (?1, ?2, ?3, ?4)", params![product, source, text.trim(), now])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn pending_signals(&self, limit: usize) -> anyhow::Result<Vec<Signal>> {
        let mut st = self.conn.prepare("SELECT id, product, source, text FROM signals WHERE done = 0 ORDER BY id LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok(Signal { id: r.get(0)?, product: r.get(1)?, source: r.get(2)?, text: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn signals_done(&self, ids: &[i64]) -> anyhow::Result<()> {
        for id in ids {
            self.conn.execute("UPDATE signals SET done = 1 WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_pref(&self, product: &str, area: &str, rule: &str, explicit: bool, status: &str, supersedes: Option<i64>, note: &str, now: f64) -> anyhow::Result<i64> {
        anyhow::ensure!(PREF_STATUSES.contains(&status), "not a preference status: {status}");
        self.conn.execute(
            "INSERT INTO prefs (product, area, rule, explicit, status, supersedes, note, created, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
            params![product, area, rule.trim(), explicit, status, supersedes, note, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn pref(&self, id: i64) -> anyhow::Result<Option<Pref>> {
        Ok(self.conn.query_row(&format!("SELECT {PREF_COLUMNS} FROM prefs WHERE id = ?1"), [id], pref_from).optional()?)
    }

    /// Preferences with one of these statuses: for a product (with the global ones), or all.
    pub fn prefs(&self, product: Option<&str>, statuses: &[&str]) -> anyhow::Result<Vec<Pref>> {
        let mut st = self.conn.prepare(&format!("SELECT {PREF_COLUMNS} FROM prefs WHERE ?1 IS NULL OR product = ?1 OR product = '*' ORDER BY area, id"))?;
        let rows = st.query_map([product], pref_from)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().filter(|p| statuses.contains(&p.status.as_str())).collect())
    }

    /// One more sighting of a preference: its count goes up and it counts as confirmed now.
    pub fn bump_pref(&self, id: i64, explicit: bool, now: f64) -> anyhow::Result<i64> {
        self.conn.execute("UPDATE prefs SET count = count + 1, explicit = MAX(explicit, ?2), last_seen = ?3 WHERE id = ?1", params![id, explicit, now])?;
        Ok(self.conn.query_row("SELECT count FROM prefs WHERE id = ?1", [id], |r| r.get(0))?)
    }

    pub fn set_pref_status(&self, id: i64, status: &str, now: f64) -> anyhow::Result<()> {
        anyhow::ensure!(PREF_STATUSES.contains(&status), "not a preference status: {status}");
        let n = self.conn.execute("UPDATE prefs SET status = ?2, last_seen = MAX(last_seen, CASE WHEN ?2 = 'active' THEN ?3 ELSE last_seen END) WHERE id = ?1", params![id, status, now])?;
        anyhow::ensure!(n == 1, "no preference #{id}");
        Ok(())
    }

    // ---------- lessons from rework ----------

    pub fn add_lesson(&self, product: &str, area: &str, lesson: &str, task: Option<i64>, now: f64) -> anyhow::Result<i64> {
        self.conn.execute("INSERT INTO lessons (product, area, lesson, task, at) VALUES (?1, ?2, ?3, ?4, ?5)", params![product, area, lesson.trim(), task, now])?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest lessons of a product, at most `n`.
    pub fn lessons(&self, product: &str, n: usize) -> anyhow::Result<Vec<Lesson>> {
        let mut st = self.conn.prepare("SELECT id, product, area, lesson FROM lessons WHERE product = ?1 ORDER BY id DESC LIMIT ?2")?;
        let rows = st.query_map(params![product, n as i64], |r| Ok(Lesson { id: r.get(0)?, product: r.get(1)?, area: r.get(2)?, lesson: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Lessons not yet written into the product's repo.
    pub fn unrecorded_lessons(&self, product: &str) -> anyhow::Result<Vec<Lesson>> {
        let mut st = self.conn.prepare("SELECT id, product, area, lesson FROM lessons WHERE product = ?1 AND recorded = 0 ORDER BY id")?;
        let rows = st.query_map([product], |r| Ok(Lesson { id: r.get(0)?, product: r.get(1)?, area: r.get(2)?, lesson: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn lessons_recorded(&self, ids: &[i64]) -> anyhow::Result<()> {
        for id in ids {
            self.conn.execute("UPDATE lessons SET recorded = 1 WHERE id = ?1", [id])?;
        }
        Ok(())
    }

    /// Failed judgment criteria of a product since `since`, with how often each failed.
    pub fn failing_criteria(&self, product: &str, since: f64) -> anyhow::Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT e.failed FROM evals e JOIN tasks t ON t.id = e.task WHERE t.product = ?1 AND e.passed = 0 AND e.at >= ?2 AND e.failed != ''",
        )?;
        let rows = st.query_map(params![product, since], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        let mut counts: std::collections::BTreeMap<String, i64> = Default::default();
        for failed in rows {
            for c in failed.split("; ").map(str::trim).filter(|c| !c.is_empty()) {
                *counts.entry(c.to_string()).or_default() += 1;
            }
        }
        let mut out: Vec<_> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    pub fn counts(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let mut st = self.conn.prepare("SELECT status, COUNT(*) FROM tasks GROUP BY status ORDER BY status")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn put(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn.execute("INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_runs_highest_priority_then_oldest() {
        let s = Store::memory();
        assert!(s.add_task(&NewTask { product: "shop", role: "engineer", title: "x", brief: "", priority: 0, parent: None, ..Default::default() }, 1.0).is_err());
        s.add_product("shop", "/repos/shop", 1.0).unwrap();
        assert!(s.add_product("shop", "/elsewhere", 2.0).is_err());
        assert!(s.add_product("Shop!", "/x", 2.0).is_err());
        assert_eq!(s.product("shop").unwrap().unwrap().mode, "normal");
        s.set_mode("shop", "autonomous").unwrap();
        assert_eq!(s.product("shop").unwrap().unwrap().mode, "autonomous");
        assert!(s.set_mode("shop", "wild").is_err() && s.set_mode("nope", "normal").is_err());
        let a = s.add_task(&NewTask { product: "shop", role: "engineer", title: "low", brief: "", priority: 0, parent: None, ..Default::default() }, 1.0).unwrap();
        let b = s.add_task(&NewTask { product: "shop", role: "engineer", title: "high", brief: "", priority: 5, parent: None, ..Default::default() }, 2.0).unwrap();
        let c = s.add_task(&NewTask { product: "shop", role: "engineer", title: "high, newer", priority: 5, ..Default::default() }, 3.0).unwrap();
        assert_eq!(s.next_task().unwrap().unwrap().id, b);
        s.set_status(b, "running", None, None, 4.0).unwrap();
        assert_eq!(s.next_task().unwrap().unwrap().id, c);
        s.set_status(c, "done", Some("shipped"), Some("jazari/task-3"), 5.0).unwrap();
        let t = s.task(c).unwrap().unwrap();
        assert_eq!((t.summary.as_str(), t.branch.as_deref()), ("shipped", Some("jazari/task-3")));
        assert_eq!(s.requeue_running(6.0).unwrap(), 1);
        assert_eq!(s.next_task().unwrap().unwrap().id, b);
        assert_eq!(s.bump_attempts(a).unwrap(), 1);
        assert!(s.set_status(a, "nonsense", None, None, 7.0).is_err());
        assert_eq!(s.counts().unwrap(), vec![("done".into(), 1), ("queued".into(), 2)]);
        assert!(s.has_similar("shop", "HIGH, newer!").unwrap() && !s.has_similar("shop", "high newest").unwrap());
        s.add_task(&NewTask { product: "shop", role: "e", title: "CLI: map PasswordTooLongError to exit code 2", ..Default::default() }, 9.0).unwrap();
        assert!(s.has_similar("shop", "Map PasswordTooLongError and invalid UTF-8 to exit code 2 in the CLI").unwrap(), "a rewording of an open task");
        assert!(!s.has_similar("shop", "Document the exit codes in the README").unwrap());
        assert_eq!(s.queued("shop").unwrap(), 3);
        assert_eq!(s.task(a).unwrap().unwrap().spec, "", "an empty brief is an empty spec");
        assert!(s.add_task(&NewTask { product: "shop", role: "x", title: "y", kind: "deploy", ..Default::default() }, 8.0).is_err());
    }

    #[test]
    fn a_task_waits_until_the_one_before_it_lands() {
        let s = Store::memory();
        s.add_product("shop", "/r", 0.0).unwrap();
        let first = s.add_task(&NewTask { product: "shop", role: "e", title: "skeleton", ..Default::default() }, 1.0).unwrap();
        let second = s.add_task(&NewTask { product: "shop", role: "e", title: "engine", priority: 9, after: Some(first), ..Default::default() }, 1.0).unwrap();
        assert_eq!(s.next_task().unwrap().unwrap().id, first, "the higher priority waits");
        s.set_status(first, "running", None, Some("jazari/task-1"), 2.0).unwrap();
        s.set_status(first, "done", None, None, 3.0).unwrap();
        assert_eq!(s.next_task().unwrap(), None, "done isn't enough: it must be merged");
        s.land_branch("jazari/task-1").unwrap();
        let t = s.next_task().unwrap().unwrap();
        assert_eq!((t.id, t.after_task), (second, Some(first)));
        s.add_product("site", "/s", 0.0).unwrap();
        let other = s.add_task(&NewTask { product: "site", role: "e", title: "landing page", ..Default::default() }, 4.0).unwrap();
        assert_eq!(s.next_task_except(&["shop".into()]).unwrap().unwrap().id, other, "a held product doesn't block the others");
    }

    #[test]
    fn a_reply_requeues_a_blocked_task_with_the_answer() {
        let s = Store::memory();
        s.add_product("shop", "/r", 0.0).unwrap();
        let id = s.add_task(&NewTask { product: "shop", role: "engineer", title: "SMS login", brief: "Send codes.", ..Default::default() }, 1.0).unwrap();
        assert!(s.reply(id, "Twilio", 2.0).is_err(), "it isn't waiting");
        s.bump_attempts(id).unwrap();
        s.wait_for_founder(id, Some("Which SMS provider?"), None, 2.0).unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.question.as_deref(), t.kind.as_str()), ("blocked", Some("Which SMS provider?"), "work"));
        s.reply(id, " Twilio ", 3.0).unwrap();
        let t = s.task(id).unwrap().unwrap();
        assert_eq!((t.status.as_str(), t.question, t.attempts), ("queued", None, 0));
        assert_eq!(t.brief, "Send codes.\n\nYou asked: Which SMS provider?\nThe founder replied: Twilio");
    }

    #[test]
    fn a_hard_line_needs_a_written_override_unless_the_recommendation_is_chosen() {
        let s = Store::memory();
        s.add_product("shop", "/r", 0.0).unwrap();
        let q = "Ship with the known SQL injection?\n1. Fix it first: one more day\n2. Ship now: users at risk\nRecommended: Fix it first";
        for reply in ["Fix it first", "option 1", "1"] {
            let id = s.add_task(&NewTask { product: "shop", role: "e", title: "Ship", ..Default::default() }, 1.0).unwrap();
            s.wait_for_founder_on(id, Some(q), None, true, 2.0).unwrap();
            assert!(s.reply(id, reply, 3.0).is_ok(), "{reply}");
        }
        let id = s.add_task(&NewTask { product: "shop", role: "e", title: "Ship", ..Default::default() }, 1.0).unwrap();
        s.wait_for_founder_on(id, Some(q), None, true, 2.0).unwrap();
        let err = s.reply(id, "option 2", 3.0).unwrap_err().to_string();
        assert!(err.contains("hard line") && err.contains(&format!("/answer {id} override:")), "{err}");
        assert!(s.reply(id, "override: the demo is offline only", 3.0).is_ok());
        let soft = s.add_task(&NewTask { product: "shop", role: "e", title: "Ship", ..Default::default() }, 1.0).unwrap();
        s.wait_for_founder(soft, Some(q), None, 2.0).unwrap();
        assert!(s.reply(soft, "option 2", 3.0).is_ok(), "without a hard line, any choice goes");
    }

    #[test]
    fn judgments_add_up_per_role() {
        let s = Store::memory();
        s.add_eval(1, "backend-engineer", "eval", true, Some(5), "", 1.0, "v1").unwrap();
        s.add_eval(2, "backend-engineer", "eval", false, Some(2), "Tests prove the behavior", 2.0, "v2").unwrap();
        s.add_eval(2, "backend-engineer", "checks", false, None, "pytest failed", 2.0, "v2").unwrap();
        assert_eq!(s.version_split("backend-engineer", "v2").unwrap(), (0, 1, 1, 1));
        assert_eq!(s.recent_evals("backend-engineer", 5).unwrap(), vec![(false, Some(2)), (true, Some(5))]);
        let q = s.quality().unwrap();
        assert_eq!(q[0], ("backend-engineer".into(), "checks".into(), 1, 0, None));
        assert_eq!(q[1], ("backend-engineer".into(), "eval".into(), 2, 1, Some(3.5)));
    }

    #[test]
    fn preferences_count_up_change_status_and_include_global_ones() {
        let s = Store::memory();
        let p = s.add_pref("shop", "copy", "Use short, plain sentences in UI text", false, "candidate", None, "", 1.0).unwrap();
        s.add_pref("*", "design", "Primary buttons are green", true, "active", None, "", 1.0).unwrap();
        s.add_pref("site", "design", "Dark theme by default", true, "active", None, "", 1.0).unwrap();
        assert_eq!(s.bump_pref(p, false, 5.0).unwrap(), 2);
        s.set_pref_status(p, "active", 6.0).unwrap();
        let shop: Vec<_> = s.prefs(Some("shop"), &["active"]).unwrap().into_iter().map(|p| p.rule).collect();
        assert_eq!(shop, vec!["Use short, plain sentences in UI text", "Primary buttons are green"]);
        assert!(s.set_pref_status(p, "forgotten", 7.0).is_err() && s.set_pref_status(99, "retired", 7.0).is_err());
        let sig = s.add_signal("shop", "answer", " make it simpler ", 1.0).unwrap();
        assert_eq!(s.pending_signals(10).unwrap()[0].text, "make it simpler");
        s.signals_done(&[sig]).unwrap();
        assert!(s.pending_signals(10).unwrap().is_empty());
        s.add_lesson("shop", "code", "Pin the Python version the lockfile is built for.", Some(3), 1.0).unwrap();
        assert_eq!(s.unrecorded_lessons("shop").unwrap().len(), 1);
        assert_eq!(s.lessons("shop", 5).unwrap()[0].lesson, "Pin the Python version the lockfile is built for.");
    }

    #[test]
    fn values_are_kept_by_key() {
        let s = Store::memory();
        assert_eq!(s.get("limits").unwrap(), None);
        s.put("limits", "a").unwrap();
        s.put("limits", "b").unwrap();
        assert_eq!(s.get("limits").unwrap().as_deref(), Some("b"));
    }
}

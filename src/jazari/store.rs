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
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

pub const STATUSES: [&str; 5] = ["queued", "running", "done", "blocked", "failed"];

#[derive(Clone, Debug, PartialEq)]
pub struct Product {
    pub name: String,
    pub repo: String,
}

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
}

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
    })
}

const TASK_COLUMNS: &str = "id, product, role, title, brief, priority, status, attempts, summary, branch";

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
            .query_row("SELECT name, repo FROM products WHERE name = ?1", [name], |r| Ok(Product { name: r.get(0)?, repo: r.get(1)? }))
            .optional()?)
    }

    pub fn products(&self) -> anyhow::Result<Vec<Product>> {
        let mut st = self.conn.prepare("SELECT name, repo FROM products ORDER BY name")?;
        let rows = st.query_map([], |r| Ok(Product { name: r.get(0)?, repo: r.get(1)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_task(&self, product: &str, role: &str, title: &str, brief: &str, priority: i64, parent: Option<i64>, now: f64) -> anyhow::Result<i64> {
        anyhow::ensure!(self.product(product)?.is_some(), "no product called {product}; add it with `marsad jazari product add`");
        anyhow::ensure!(!title.trim().is_empty(), "the task needs a title");
        self.conn.execute(
            "INSERT INTO tasks (product, role, title, brief, priority, parent, created, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![product, role, title.trim(), brief.trim(), priority, parent, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    #[cfg(test)]
    pub fn task(&self, id: i64) -> anyhow::Result<Option<Task>> {
        Ok(self.conn.query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"), [id], task_from).optional()?)
    }

    /// The queued task to run next: highest priority first, then oldest.
    pub fn next_task(&self) -> anyhow::Result<Option<Task>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {TASK_COLUMNS} FROM tasks WHERE status = 'queued' ORDER BY priority DESC, id LIMIT 1"), [], task_from)
            .optional()?)
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
        assert!(s.add_task("shop", "engineer", "x", "", 0, None, 1.0).is_err());
        s.add_product("shop", "/repos/shop", 1.0).unwrap();
        assert!(s.add_product("shop", "/elsewhere", 2.0).is_err());
        assert!(s.add_product("Shop!", "/x", 2.0).is_err());
        let a = s.add_task("shop", "engineer", "low", "", 0, None, 1.0).unwrap();
        let b = s.add_task("shop", "engineer", "high", "", 5, None, 2.0).unwrap();
        let c = s.add_task("shop", "engineer", "high, newer", "", 5, None, 3.0).unwrap();
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

use rusqlite::{Connection, params};
use std::path::Path;

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug)]
pub struct Session {
    pub _id: i64,
    pub session_id: String,
    pub claude_session_id: Option<String>,
    pub name: Option<String>,
    pub prompt: String,
    pub _cwd: String,
    pub status: String,
    pub pid: Option<i64>,
    pub started_at: String,
    pub _completed_at: Option<String>,
    pub _exit_code: Option<i32>,
}

#[derive(Debug)]
pub struct ToolCall {
    pub id: i64,
    pub session_id: String,
    pub tool_name: String,
    pub tool_input: String,
    pub status: String,
    pub reason: Option<String>,
    pub summary: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

const SESSION_COLS: &str = "id, session_id, claude_session_id, name, prompt, cwd, status, pid, started_at, completed_at, exit_code";

fn map_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        _id: row.get(0)?,
        session_id: row.get(1)?,
        claude_session_id: row.get(2)?,
        name: row.get(3)?,
        prompt: row.get(4)?,
        _cwd: row.get(5)?,
        status: row.get(6)?,
        pid: row.get(7)?,
        started_at: row.get(8)?,
        _completed_at: row.get(9)?,
        _exit_code: row.get(10)?,
    })
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;
        ",
        )?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT UNIQUE NOT NULL,
                claude_session_id TEXT,
                name TEXT,
                prompt TEXT NOT NULL,
                cwd TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                pid INTEGER,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT,
                exit_code INTEGER
            );
            CREATE TABLE IF NOT EXISTS tool_calls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                tool_input TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                reason TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                resolved_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_tool_calls_pending
                ON tool_calls(status) WHERE status = 'pending';
            CREATE INDEX IF NOT EXISTS idx_tool_calls_session
                ON tool_calls(session_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_status
                ON sessions(status);
        ",
        )?;
        // Migrations for existing DBs
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN claude_session_id TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN name TEXT", []);
        let _ = conn.execute("ALTER TABLE tool_calls ADD COLUMN summary TEXT", []);
        // Queue table for push queuing (multiple messages per session)
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS queued_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_name TEXT NOT NULL,
                prompt TEXT NOT NULL,
                cwd TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )?;
        // Migration: drop UNIQUE constraint if upgrading from old schema
        // SQLite doesn't support DROP CONSTRAINT, but the table recreate handles it
        // since we use CREATE TABLE IF NOT EXISTS with the new schema.
        Ok(Db { conn })
    }

    // --- Sessions ---

    #[allow(clippy::too_many_arguments)]
    pub fn create_session(
        &self,
        session_id: &str,
        claude_session_id: Option<&str>,
        name: Option<&str>,
        prompt: &str,
        cwd: &str,
        pid: Option<u32>,
    ) -> rusqlite::Result<()> {
        let pid_val: Option<i64> = pid.map(|p| p as i64);
        self.conn.execute(
            "INSERT INTO sessions (session_id, claude_session_id, name, prompt, cwd, pid) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, claude_session_id, name, prompt, cwd, pid_val],
        )?;
        Ok(())
    }

    /// Atomically claim a session for queued message delivery by transitioning its status.
    /// Returns true if this caller won the race (status was updated), false if someone else got there first.
    pub fn claim_session_for_delivery(&self, session_id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions SET status = 'delivering' WHERE session_id = ?1 AND status IN ('completed', 'failed')",
            params![session_id],
        )?;
        Ok(changed > 0)
    }

    pub fn update_session_pid(&self, session_id: &str, pid: u32) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET pid = ?1 WHERE session_id = ?2",
            params![pid, session_id],
        )?;
        Ok(())
    }

    pub fn update_session_status(
        &self,
        session_id: &str,
        status: &str,
        exit_code: Option<i32>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?1, exit_code = ?2, completed_at = datetime('now') WHERE session_id = ?3",
            params![status, exit_code, session_id],
        )?;
        Ok(())
    }

    pub fn get_sessions(&self) -> rusqlite::Result<Vec<Session>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions ORDER BY id DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_session)?;
        rows.collect()
    }

    /// Find a session by name, session_id prefix, or claude_session_id prefix.
    /// Name is an exact match; IDs are prefix matches. Most recent match wins.
    pub fn find_session(&self, query: &str) -> rusqlite::Result<Option<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE name = ?1 OR session_id LIKE ?1 || '%' OR claude_session_id LIKE ?1 || '%' ORDER BY id DESC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![query], map_session)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Find ALL sessions matching a name, ordered by started_at ASC.
    /// Only matches by exact name. Returns empty vec if no matches.
    pub fn find_sessions_by_name(&self, name: &str) -> rusqlite::Result<Vec<Session>> {
        let sql =
            format!("SELECT {SESSION_COLS} FROM sessions WHERE name = ?1 ORDER BY started_at ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![name], map_session)?;
        rows.collect()
    }

    /// Return a map of session_id -> display name for all sessions that have a name.
    pub fn get_session_names(&self) -> rusqlite::Result<std::collections::HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id, name FROM sessions WHERE name IS NOT NULL")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (id, name) = row?;
            map.insert(id, name);
        }
        Ok(map)
    }

    // --- Tool Calls ---

    #[allow(dead_code)]
    pub fn insert_tool_call(
        &self,
        session_id: &str,
        tool_name: &str,
        tool_input: &str,
    ) -> rusqlite::Result<i64> {
        self.insert_tool_call_with_summary(session_id, tool_name, tool_input, None)
    }

    pub fn insert_tool_call_with_summary(
        &self,
        session_id: &str,
        tool_name: &str,
        tool_input: &str,
        summary: Option<&str>,
    ) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO tool_calls (session_id, tool_name, tool_input, summary) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, tool_name, tool_input, summary],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn resolve_tool_call(
        &self,
        id: i64,
        status: &str,
        reason: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = ?1, reason = ?2, resolved_at = datetime('now') WHERE id = ?3 AND status = 'pending'",
            params![status, reason, id],
        )?;
        Ok(changed > 0)
    }

    pub fn get_tool_call_status(
        &self,
        id: i64,
    ) -> rusqlite::Result<Option<(String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT status, reason FROM tool_calls WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_pending_tool_calls(
        &self,
        session_filter: Option<&str>,
    ) -> rusqlite::Result<Vec<ToolCall>> {
        let sql = match session_filter {
            Some(_) => {
                "SELECT id, session_id, tool_name, tool_input, status, reason, summary, created_at, resolved_at FROM tool_calls WHERE status = 'pending' AND session_id LIKE ?1 || '%' ORDER BY id"
            }
            None => {
                "SELECT id, session_id, tool_name, tool_input, status, reason, summary, created_at, resolved_at FROM tool_calls WHERE status = 'pending' ORDER BY id"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(prefix) = session_filter {
            stmt.query_map(params![prefix], Self::map_tool_call)?
        } else {
            stmt.query_map([], Self::map_tool_call)?
        };
        rows.collect()
    }

    pub fn find_pending_by_summary(&self, query: &str) -> rusqlite::Result<Vec<ToolCall>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, tool_name, tool_input, status, reason, summary, created_at, resolved_at FROM tool_calls WHERE status = 'pending' AND summary IS NOT NULL AND summary LIKE '%' || ?1 || '%'"
        )?;
        let rows = stmt.query_map(params![query], Self::map_tool_call)?;
        rows.collect()
    }

    pub fn get_tool_call_by_id(&self, id: i64) -> rusqlite::Result<Option<ToolCall>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, tool_name, tool_input, status, reason, summary, created_at, resolved_at FROM tool_calls WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], Self::map_tool_call)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn approve_all_pending(&self) -> rusqlite::Result<usize> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = 'approved', resolved_at = datetime('now') WHERE status = 'pending'",
            [],
        )?;
        Ok(changed)
    }

    pub fn approve_all_pending_for_session(&self, session_id: &str) -> rusqlite::Result<usize> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = 'approved', resolved_at = datetime('now') WHERE status = 'pending' AND session_id = ?1",
            params![session_id],
        )?;
        Ok(changed)
    }

    pub fn approve_all_pending_for_tool(&self, tool_name: &str) -> rusqlite::Result<usize> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = 'approved', resolved_at = datetime('now') WHERE status = 'pending' AND tool_name = ?1",
            params![tool_name],
        )?;
        Ok(changed)
    }

    pub fn approve_all_pending_for_session_and_tool(
        &self,
        session_id: &str,
        tool_name: &str,
    ) -> rusqlite::Result<usize> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = 'approved', resolved_at = datetime('now') WHERE status = 'pending' AND session_id = ?1 AND tool_name = ?2",
            params![session_id, tool_name],
        )?;
        Ok(changed)
    }

    // --- Queued Messages ---

    /// Push a message onto the queue for a session name.
    pub fn push_queued_message(
        &self,
        session_name: &str,
        prompt: &str,
        cwd: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO queued_messages (session_name, prompt, cwd) VALUES (?1, ?2, ?3)",
            params![session_name, prompt, cwd],
        )?;
        Ok(())
    }

    /// Take all queued messages for a session name (ordered by id ASC), deleting them.
    /// Returns vec of (prompt, cwd) tuples.
    pub fn take_all_queued_messages(
        &self,
        session_name: &str,
    ) -> rusqlite::Result<Vec<(String, Option<String>)>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut stmt = tx.prepare(
            "SELECT prompt, cwd FROM queued_messages WHERE session_name = ?1 ORDER BY id ASC",
        )?;
        let rows: Vec<(String, Option<String>)> = stmt
            .query_map(params![session_name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        if !rows.is_empty() {
            tx.execute(
                "DELETE FROM queued_messages WHERE session_name = ?1",
                params![session_name],
            )?;
        }
        tx.commit()?;
        Ok(rows)
    }

    /// Clear all queued messages for a session name. Returns count deleted.
    pub fn clear_queued_messages(&self, session_name: &str) -> rusqlite::Result<usize> {
        let changed = self.conn.execute(
            "DELETE FROM queued_messages WHERE session_name = ?1",
            params![session_name],
        )?;
        Ok(changed)
    }

    /// Check if any queued messages exist for a session name.
    #[allow(dead_code)]
    pub fn has_queued_messages(&self, session_name: &str) -> rusqlite::Result<bool> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM queued_messages WHERE session_name = ?1 LIMIT 1")?;
        let mut rows = stmt.query(params![session_name])?;
        Ok(rows.next()?.is_some())
    }

    // --- GC ---

    pub fn get_running_sessions(&self) -> rusqlite::Result<Vec<Session>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE status = 'running'");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_session)?;
        rows.collect()
    }

    /// Delete sessions with started_at before the given ISO 8601 cutoff.
    /// Only deletes sessions that are not running.
    /// Returns the list of deleted session IDs.
    pub fn delete_sessions_older_than(&self, cutoff: &str) -> rusqlite::Result<Vec<String>> {
        // First collect the session IDs that will be deleted
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM sessions WHERE started_at < ?1 AND status != 'running'",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![cutoff], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;

        if !ids.is_empty() {
            self.conn.execute(
                "DELETE FROM sessions WHERE started_at < ?1 AND status != 'running'",
                params![cutoff],
            )?;
        }
        Ok(ids)
    }

    /// Delete tool_calls belonging to the given session IDs. Returns count deleted.
    pub fn delete_tool_calls_for_sessions(
        &self,
        session_ids: &[String],
    ) -> rusqlite::Result<usize> {
        if session_ids.is_empty() {
            return Ok(0);
        }
        let placeholders: Vec<String> = (1..=session_ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "DELETE FROM tool_calls WHERE session_id IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<&dyn rusqlite::types::ToSql> = session_ids
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let count = self.conn.execute(&sql, params.as_slice())?;
        Ok(count)
    }

    fn map_tool_call(row: &rusqlite::Row) -> rusqlite::Result<ToolCall> {
        Ok(ToolCall {
            id: row.get(0)?,
            session_id: row.get(1)?,
            tool_name: row.get(2)?,
            tool_input: row.get(3)?,
            status: row.get(4)?,
            reason: row.get(5)?,
            summary: row.get(6)?,
            created_at: row.get(7)?,
            resolved_at: row.get(8)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn open_temp_db() -> Db {
        let tmp = NamedTempFile::new().unwrap();
        Db::open(tmp.path()).unwrap()
    }

    #[test]
    fn test_create_and_get_sessions() {
        let db = open_temp_db();
        db.create_session("s1", None, None, "do stuff", "/tmp", Some(1234))
            .unwrap();
        let sessions = db.get_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(sessions[0].prompt, "do stuff");
        assert_eq!(sessions[0]._cwd, "/tmp");
        assert_eq!(sessions[0].status, "running");
    }

    #[test]
    fn test_find_session_by_name() {
        let db = open_temp_db();
        db.create_session("s1", None, Some("alpha"), "p1", "/tmp", Some(1))
            .unwrap();
        db.create_session("s2", None, Some("beta"), "p2", "/tmp", Some(2))
            .unwrap();
        let found = db.find_session("alpha").unwrap().unwrap();
        assert_eq!(found.session_id, "s1");
        let found = db.find_session("beta").unwrap().unwrap();
        assert_eq!(found.session_id, "s2");
        assert!(db.find_session("gamma").unwrap().is_none());
    }

    #[test]
    fn test_find_sessions_by_name_returns_all() {
        let db = open_temp_db();
        db.create_session("s1", None, Some("mytask"), "p1", "/tmp", Some(1))
            .unwrap();
        db.create_session("s2", None, Some("other"), "p2", "/tmp", Some(2))
            .unwrap();
        db.create_session("s3", None, Some("mytask"), "p3", "/tmp", Some(3))
            .unwrap();

        let sessions = db.find_sessions_by_name("mytask").unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(sessions[1].session_id, "s3");

        let sessions = db.find_sessions_by_name("other").unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s2");

        let sessions = db.find_sessions_by_name("nonexistent").unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_find_session_by_id_prefix() {
        let db = open_temp_db();
        db.create_session("abc-123-def", None, None, "p", "/tmp", Some(1))
            .unwrap();
        let found = db.find_session("abc").unwrap().unwrap();
        assert_eq!(found.session_id, "abc-123-def");
        assert!(db.find_session("xyz").unwrap().is_none());
    }

    #[test]
    fn test_update_session_status() {
        let db = open_temp_db();
        db.create_session("s1", None, None, "p", "/tmp", Some(1))
            .unwrap();
        db.update_session_status("s1", "completed", Some(0))
            .unwrap();
        let sessions = db.get_sessions().unwrap();
        assert_eq!(sessions[0].status, "completed");
        assert!(sessions[0]._completed_at.is_some());
        assert_eq!(sessions[0]._exit_code, Some(0));
    }

    #[test]
    fn test_insert_and_get_tool_call() {
        let db = open_temp_db();
        let id = db
            .insert_tool_call("s1", "Bash", r#"{"command":"ls"}"#)
            .unwrap();
        let tc = db.get_tool_call_by_id(id).unwrap().unwrap();
        assert_eq!(tc.session_id, "s1");
        assert_eq!(tc.tool_name, "Bash");
        assert_eq!(tc.tool_input, r#"{"command":"ls"}"#);
        assert_eq!(tc.status, "pending");
        assert!(tc.resolved_at.is_none());
    }

    #[test]
    fn test_resolve_tool_call() {
        let db = open_temp_db();
        let id = db.insert_tool_call("s1", "Bash", "input").unwrap();
        let changed = db.resolve_tool_call(id, "approved", None).unwrap();
        assert!(changed);
        let tc = db.get_tool_call_by_id(id).unwrap().unwrap();
        assert_eq!(tc.status, "approved");
        assert!(tc.resolved_at.is_some());
    }

    #[test]
    fn test_resolve_already_resolved() {
        let db = open_temp_db();
        let id = db.insert_tool_call("s1", "Bash", "input").unwrap();
        assert!(db.resolve_tool_call(id, "approved", None).unwrap());
        let changed = db.resolve_tool_call(id, "denied", Some("nope")).unwrap();
        assert!(!changed);
        // Status should still be approved from first resolve
        let tc = db.get_tool_call_by_id(id).unwrap().unwrap();
        assert_eq!(tc.status, "approved");
    }

    #[test]
    fn test_get_pending_tool_calls() {
        let db = open_temp_db();
        let id1 = db.insert_tool_call("s1", "Bash", "a").unwrap();
        let _id2 = db.insert_tool_call("s1", "Read", "b").unwrap();
        let _id3 = db.insert_tool_call("s1", "Write", "c").unwrap();
        db.resolve_tool_call(id1, "approved", None).unwrap();
        let pending = db.get_pending_tool_calls(None).unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|tc| tc.status == "pending"));
    }

    #[test]
    fn test_get_pending_with_session_filter() {
        let db = open_temp_db();
        db.insert_tool_call("sess-aaa", "Bash", "a").unwrap();
        db.insert_tool_call("sess-bbb", "Read", "b").unwrap();
        db.insert_tool_call("sess-aaa", "Write", "c").unwrap();
        let pending = db.get_pending_tool_calls(Some("sess-aaa")).unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|tc| tc.session_id == "sess-aaa"));
    }

    #[test]
    fn test_approve_all_pending() {
        let db = open_temp_db();
        db.insert_tool_call("s1", "Bash", "a").unwrap();
        db.insert_tool_call("s1", "Read", "b").unwrap();
        db.insert_tool_call("s1", "Write", "c").unwrap();
        let count = db.approve_all_pending().unwrap();
        assert_eq!(count, 3);
        let pending = db.get_pending_tool_calls(None).unwrap();
        assert!(pending.is_empty());
    }

    #[test]
    fn test_approve_all_for_session() {
        let db = open_temp_db();
        db.insert_tool_call("s1", "Bash", "a").unwrap();
        db.insert_tool_call("s2", "Read", "b").unwrap();
        db.insert_tool_call("s1", "Write", "c").unwrap();
        let count = db.approve_all_pending_for_session("s1").unwrap();
        assert_eq!(count, 2);
        let pending = db.get_pending_tool_calls(None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].session_id, "s2");
    }

    #[test]
    fn test_approve_all_for_tool() {
        let db = open_temp_db();
        db.insert_tool_call("s1", "Bash", "a").unwrap();
        db.insert_tool_call("s1", "Read", "b").unwrap();
        db.insert_tool_call("s1", "Bash", "c").unwrap();
        let count = db.approve_all_pending_for_tool("Bash").unwrap();
        assert_eq!(count, 2);
        let pending = db.get_pending_tool_calls(None).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name, "Read");
    }

    #[test]
    fn test_insert_tool_call_with_summary() {
        let db = open_temp_db();
        let id = db
            .insert_tool_call_with_summary(
                "s1",
                "Bash",
                r#"{"command":"git push"}"#,
                Some("Pushes current branch to origin"),
            )
            .unwrap();
        let tc = db.get_tool_call_by_id(id).unwrap().unwrap();
        assert_eq!(
            tc.summary.as_deref(),
            Some("Pushes current branch to origin")
        );
        assert_eq!(tc.status, "pending");
    }

    #[test]
    fn test_insert_tool_call_without_summary() {
        let db = open_temp_db();
        let id = db
            .insert_tool_call_with_summary("s1", "Bash", "input", None)
            .unwrap();
        let tc = db.get_tool_call_by_id(id).unwrap().unwrap();
        assert!(tc.summary.is_none());
    }

    #[test]
    fn test_find_pending_by_summary_exact() {
        let db = open_temp_db();
        db.insert_tool_call_with_summary(
            "s1",
            "Bash",
            "a",
            Some("Pushes current branch to origin"),
        )
        .unwrap();
        db.insert_tool_call_with_summary("s1", "Write", "b", Some("Writes config file"))
            .unwrap();
        db.insert_tool_call("s1", "Read", "c").unwrap(); // no summary

        let results = db.find_pending_by_summary("Pushes current branch").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_name, "Bash");
    }

    #[test]
    fn test_find_pending_by_summary_no_match() {
        let db = open_temp_db();
        db.insert_tool_call_with_summary("s1", "Bash", "a", Some("Pushes branch"))
            .unwrap();
        let results = db.find_pending_by_summary("Deletes everything").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_find_pending_by_summary_excludes_resolved() {
        let db = open_temp_db();
        let id = db
            .insert_tool_call_with_summary("s1", "Bash", "a", Some("Pushes branch"))
            .unwrap();
        db.resolve_tool_call(id, "approved", None).unwrap();
        let results = db.find_pending_by_summary("Pushes branch").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_find_pending_by_summary_multiple_matches() {
        let db = open_temp_db();
        db.insert_tool_call_with_summary("s1", "Bash", "a", Some("Pushes branch to origin"))
            .unwrap();
        db.insert_tool_call_with_summary("s1", "Bash", "b", Some("Pushes branch to upstream"))
            .unwrap();
        let results = db.find_pending_by_summary("Pushes branch").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_running_sessions() {
        let db = open_temp_db();
        db.create_session("s1", None, None, "p1", "/tmp", Some(1))
            .unwrap();
        db.create_session("s2", None, None, "p2", "/tmp", Some(2))
            .unwrap();
        db.update_session_status("s2", "completed", Some(0))
            .unwrap();
        db.create_session("s3", None, None, "p3", "/tmp", Some(3))
            .unwrap();
        let running = db.get_running_sessions().unwrap();
        assert_eq!(running.len(), 2);
        let ids: Vec<&str> = running.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"s1"));
        assert!(ids.contains(&"s3"));
    }

    #[test]
    fn test_delete_sessions_older_than() {
        let db = open_temp_db();
        // Insert sessions with explicit timestamps
        db.conn.execute(
            "INSERT INTO sessions (session_id, prompt, cwd, pid, status, started_at) VALUES ('old1', 'p', '/tmp', 1, 'completed', '2020-01-01 00:00:00')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO sessions (session_id, prompt, cwd, pid, status, started_at) VALUES ('old2', 'p', '/tmp', 2, 'failed', '2020-01-02 00:00:00')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO sessions (session_id, prompt, cwd, pid, status, started_at) VALUES ('new1', 'p', '/tmp', 3, 'completed', '2099-01-01 00:00:00')",
            [],
        ).unwrap();
        db.conn.execute(
            "INSERT INTO sessions (session_id, prompt, cwd, pid, status, started_at) VALUES ('running1', 'p', '/tmp', 4, 'running', '2020-01-01 00:00:00')",
            [],
        ).unwrap();

        let deleted = db
            .delete_sessions_older_than("2025-01-01 00:00:00")
            .unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(deleted.contains(&"old1".to_string()));
        assert!(deleted.contains(&"old2".to_string()));

        // Verify remaining sessions
        let remaining = db.get_sessions().unwrap();
        assert_eq!(remaining.len(), 2);
        let ids: Vec<&str> = remaining.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"new1"));
        assert!(ids.contains(&"running1")); // running sessions are preserved
    }

    #[test]
    fn test_delete_tool_calls_for_sessions() {
        let db = open_temp_db();
        db.insert_tool_call("s1", "Bash", "a").unwrap();
        db.insert_tool_call("s1", "Read", "b").unwrap();
        db.insert_tool_call("s2", "Write", "c").unwrap();
        db.insert_tool_call("s3", "Bash", "d").unwrap();

        let count = db
            .delete_tool_calls_for_sessions(&["s1".to_string(), "s3".to_string()])
            .unwrap();
        assert_eq!(count, 3);

        // Only s2's tool call should remain
        let remaining = db.get_pending_tool_calls(None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].session_id, "s2");
    }

    #[test]
    fn test_delete_tool_calls_for_empty_sessions() {
        let db = open_temp_db();
        db.insert_tool_call("s1", "Bash", "a").unwrap();
        let count = db.delete_tool_calls_for_sessions(&[]).unwrap();
        assert_eq!(count, 0);
        // Original tool call still there
        assert_eq!(db.get_pending_tool_calls(None).unwrap().len(), 1);
    }

    #[test]
    fn test_pending_tool_calls_include_summary() {
        let db = open_temp_db();
        db.insert_tool_call_with_summary("s1", "Bash", "a", Some("Does something"))
            .unwrap();
        db.insert_tool_call("s1", "Read", "b").unwrap();
        let pending = db.get_pending_tool_calls(None).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].summary.as_deref(), Some("Does something"));
        assert!(pending[1].summary.is_none());
    }

    #[test]
    fn test_push_and_take_queued_messages() {
        let db = open_temp_db();
        db.push_queued_message("mytask", "do thing 1", Some("/tmp"))
            .unwrap();
        db.push_queued_message("mytask", "do thing 2", Some("/home"))
            .unwrap();
        assert!(db.has_queued_messages("mytask").unwrap());

        let msgs = db.take_all_queued_messages("mytask").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].0, "do thing 1");
        assert_eq!(msgs[0].1.as_deref(), Some("/tmp"));
        assert_eq!(msgs[1].0, "do thing 2");
        assert_eq!(msgs[1].1.as_deref(), Some("/home"));

        // take deletes them
        assert!(!db.has_queued_messages("mytask").unwrap());
        assert!(db.take_all_queued_messages("mytask").unwrap().is_empty());
    }

    #[test]
    fn test_push_queued_message_no_cwd() {
        let db = open_temp_db();
        db.push_queued_message("mytask", "do thing", None).unwrap();
        let msgs = db.take_all_queued_messages("mytask").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, "do thing");
        assert!(msgs[0].1.is_none());
    }

    #[test]
    fn test_clear_queued_messages() {
        let db = open_temp_db();
        db.push_queued_message("mytask", "msg 1", Some("/tmp"))
            .unwrap();
        db.push_queued_message("mytask", "msg 2", None).unwrap();
        assert_eq!(db.clear_queued_messages("mytask").unwrap(), 2);
        assert!(!db.has_queued_messages("mytask").unwrap());
        // Clear non-existent returns 0
        assert_eq!(db.clear_queued_messages("mytask").unwrap(), 0);
    }

    #[test]
    fn test_queued_messages_independent_per_name() {
        let db = open_temp_db();
        db.push_queued_message("task-a", "prompt a", Some("/a"))
            .unwrap();
        db.push_queued_message("task-b", "prompt b", Some("/b"))
            .unwrap();
        assert!(db.has_queued_messages("task-a").unwrap());
        assert!(db.has_queued_messages("task-b").unwrap());
        assert!(!db.has_queued_messages("task-c").unwrap());

        db.clear_queued_messages("task-a").unwrap();
        assert!(!db.has_queued_messages("task-a").unwrap());
        assert!(db.has_queued_messages("task-b").unwrap());
    }
}

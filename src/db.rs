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
        conn.execute_batch("
            PRAGMA journal_mode=WAL;
            PRAGMA busy_timeout=5000;
        ")?;
        conn.execute_batch("
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
        ")?;
        // Migrations for existing DBs
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN claude_session_id TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN name TEXT", []);
        Ok(Db { conn })
    }

    // --- Sessions ---

    pub fn create_session(&self, session_id: &str, claude_session_id: Option<&str>, name: Option<&str>, prompt: &str, cwd: &str, pid: u32) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (session_id, claude_session_id, name, prompt, cwd, pid) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, claude_session_id, name, prompt, cwd, pid as i64],
        )?;
        Ok(())
    }

    pub fn update_session_status(&self, session_id: &str, status: &str, exit_code: Option<i32>) -> rusqlite::Result<()> {
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

    // --- Tool Calls ---

    pub fn insert_tool_call(&self, session_id: &str, tool_name: &str, tool_input: &str) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO tool_calls (session_id, tool_name, tool_input) VALUES (?1, ?2, ?3)",
            params![session_id, tool_name, tool_input],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn resolve_tool_call(&self, id: i64, status: &str, reason: Option<&str>) -> rusqlite::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = ?1, reason = ?2, resolved_at = datetime('now') WHERE id = ?3 AND status = 'pending'",
            params![status, reason, id],
        )?;
        Ok(changed > 0)
    }

    pub fn get_tool_call_status(&self, id: i64) -> rusqlite::Result<Option<(String, Option<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT status, reason FROM tool_calls WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_pending_tool_calls(&self, session_filter: Option<&str>) -> rusqlite::Result<Vec<ToolCall>> {
        let sql = match session_filter {
            Some(_) => "SELECT id, session_id, tool_name, tool_input, status, reason, created_at, resolved_at FROM tool_calls WHERE status = 'pending' AND session_id LIKE ?1 || '%' ORDER BY id",
            None => "SELECT id, session_id, tool_name, tool_input, status, reason, created_at, resolved_at FROM tool_calls WHERE status = 'pending' ORDER BY id",
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(prefix) = session_filter {
            stmt.query_map(params![prefix], Self::map_tool_call)?
        } else {
            stmt.query_map([], Self::map_tool_call)?
        };
        rows.collect()
    }

    pub fn get_tool_call_by_id(&self, id: i64) -> rusqlite::Result<Option<ToolCall>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, tool_name, tool_input, status, reason, created_at, resolved_at FROM tool_calls WHERE id = ?1"
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

    pub fn approve_all_pending_for_session_and_tool(&self, session_id: &str, tool_name: &str) -> rusqlite::Result<usize> {
        let changed = self.conn.execute(
            "UPDATE tool_calls SET status = 'approved', resolved_at = datetime('now') WHERE status = 'pending' AND session_id = ?1 AND tool_name = ?2",
            params![session_id, tool_name],
        )?;
        Ok(changed)
    }

    fn map_tool_call(row: &rusqlite::Row) -> rusqlite::Result<ToolCall> {
        Ok(ToolCall {
            id: row.get(0)?,
            session_id: row.get(1)?,
            tool_name: row.get(2)?,
            tool_input: row.get(3)?,
            status: row.get(4)?,
            reason: row.get(5)?,
            created_at: row.get(6)?,
            resolved_at: row.get(7)?,
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
        db.create_session("s1", None, None, "do stuff", "/tmp", 1234).unwrap();
        let sessions = db.get_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(sessions[0].prompt, "do stuff");
        assert_eq!(sessions[0].cwd, "/tmp");
        assert_eq!(sessions[0].status, "running");
    }

    #[test]
    fn test_find_session_by_name() {
        let db = open_temp_db();
        db.create_session("s1", None, Some("alpha"), "p1", "/tmp", 1).unwrap();
        db.create_session("s2", None, Some("beta"), "p2", "/tmp", 2).unwrap();
        let found = db.find_session("alpha").unwrap().unwrap();
        assert_eq!(found.session_id, "s1");
        let found = db.find_session("beta").unwrap().unwrap();
        assert_eq!(found.session_id, "s2");
        assert!(db.find_session("gamma").unwrap().is_none());
    }

    #[test]
    fn test_find_session_by_id_prefix() {
        let db = open_temp_db();
        db.create_session("abc-123-def", None, None, "p", "/tmp", 1).unwrap();
        let found = db.find_session("abc").unwrap().unwrap();
        assert_eq!(found.session_id, "abc-123-def");
        assert!(db.find_session("xyz").unwrap().is_none());
    }

    #[test]
    fn test_update_session_status() {
        let db = open_temp_db();
        db.create_session("s1", None, None, "p", "/tmp", 1).unwrap();
        db.update_session_status("s1", "completed", Some(0)).unwrap();
        let sessions = db.get_sessions().unwrap();
        assert_eq!(sessions[0].status, "completed");
        assert!(sessions[0].completed_at.is_some());
        assert_eq!(sessions[0].exit_code, Some(0));
    }

    #[test]
    fn test_insert_and_get_tool_call() {
        let db = open_temp_db();
        let id = db.insert_tool_call("s1", "Bash", r#"{"command":"ls"}"#).unwrap();
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
}

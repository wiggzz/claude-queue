use rusqlite::{Connection, params};
use std::path::Path;

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug)]
pub struct Session {
    pub id: i64,
    pub session_id: String,
    pub claude_session_id: Option<String>,
    pub name: Option<String>,
    pub prompt: String,
    pub cwd: String,
    pub status: String,
    pub pid: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub exit_code: Option<i32>,
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
        id: row.get(0)?,
        session_id: row.get(1)?,
        claude_session_id: row.get(2)?,
        name: row.get(3)?,
        prompt: row.get(4)?,
        cwd: row.get(5)?,
        status: row.get(6)?,
        pid: row.get(7)?,
        started_at: row.get(8)?,
        completed_at: row.get(9)?,
        exit_code: row.get(10)?,
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

    /// Find the most recent session with a given claude_session_id.
    /// Used to look up the claude session ID for resuming.
    pub fn find_by_claude_session(&self, claude_session_id: &str) -> rusqlite::Result<Option<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE claude_session_id = ?1 ORDER BY id DESC LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![claude_session_id], map_session)?;
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

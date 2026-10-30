//! SQLite 저장소와 버전 관리 마이그레이션(PRAGMA user_version).

use crate::error::AppResult;
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

/// 순서대로 적용되는 마이그레이션. 이미 배포된 항목은 수정하지 말고 새 항목을 추가한다.
const MIGRATIONS: &[&str] = &[
    // v1: Elysium 첫 스키마(2026-10-17). JARVIS와는 데이터 폴더가 달라 이어받지 않는다.
    r#"
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE conversations (
        id TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE messages (
        id TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
        content TEXT NOT NULL,
        data_json TEXT,
        created_at TEXT NOT NULL
    );
    CREATE INDEX idx_messages_conv ON messages(conversation_id, created_at);
    -- CLI가 돌려준 세션 ID. 같은 대화에서 이어서 물을 때 다시 쓴다.
    CREATE TABLE engine_sessions (
        conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        engine TEXT NOT NULL,
        session_id TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        PRIMARY KEY (conversation_id, engine)
    );
    CREATE TABLE approvals (
        id TEXT PRIMARY KEY,
        conversation_id TEXT NOT NULL,
        task_id TEXT NOT NULL,
        engine TEXT NOT NULL,
        summary TEXT NOT NULL,
        workspace TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'expired')),
        created_at TEXT NOT NULL,
        decided_at TEXT
    );
    CREATE TABLE usage_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at TEXT NOT NULL,
        engine TEXT NOT NULL,
        role TEXT NOT NULL,
        ok INTEGER NOT NULL,
        latency_ms INTEGER NOT NULL,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cost_usd REAL
    );
    -- 켜고 끈 스킬·플러그인(기본은 켜짐). key = "skill:<id>" 또는 "plugin:<id>"
    CREATE TABLE toggles (
        key TEXT PRIMARY KEY,
        enabled INTEGER NOT NULL
    );
    CREATE TABLE audit_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        at TEXT NOT NULL,
        actor TEXT NOT NULL,
        event TEXT NOT NULL,
        outcome TEXT NOT NULL,
        meta_json TEXT NOT NULL
    );
    "#,
    // v2: 대화(세션)마다 작업 폴더를 따로 둔다. NULL이면 설정의 기본 작업 폴더.
    r#"
    ALTER TABLE conversations ADD COLUMN workspace_dir TEXT;
    "#,
    // v3: 원격 제어로 페어링한 폰. 토큰은 SHA-256 해시만 둔다.
    r#"
    CREATE TABLE remote_devices (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        token_hash TEXT NOT NULL UNIQUE,
        created_at TEXT NOT NULL,
        last_seen_at TEXT
    );
    "#,
    // v4: 세션 메모리. 오래된 대화를 접은 요약과 어디까지 접었는지, 엔진이 자기 세션으로 마지막에 본 시각.
    r#"
    ALTER TABLE conversations ADD COLUMN memory_summary TEXT;
    ALTER TABLE conversations ADD COLUMN memory_upto TEXT;
    ALTER TABLE engine_sessions ADD COLUMN seen_at TEXT;
    "#,
    // v5: 과거 usage_events 중 cost_usd가 비어 있는 CLI/API 기록에 추정 비용 채우기
    r#"
    UPDATE usage_events SET cost_usd = (COALESCE(input_tokens, 0) * 2.5 + COALESCE(output_tokens, 0) * 10.0) / 1000000.0 WHERE engine = 'codex' AND cost_usd IS NULL;
    UPDATE usage_events SET cost_usd = (COALESCE(input_tokens, 0) * 0.10 + COALESCE(output_tokens, 0) * 0.40) / 1000000.0 WHERE (engine = 'antigravity' OR engine = 'gemini') AND cost_usd IS NULL;
    UPDATE usage_events SET cost_usd = (COALESCE(input_tokens, 0) * 3.0 + COALESCE(output_tokens, 0) * 15.0) / 1000000.0 WHERE engine = 'claude-api' AND cost_usd IS NULL;
    UPDATE usage_events SET cost_usd = (COALESCE(input_tokens, 0) * 2.5 + COALESCE(output_tokens, 0) * 10.0) / 1000000.0 WHERE engine = 'openai' AND cost_usd IS NULL;
    UPDATE usage_events SET cost_usd = (COALESCE(input_tokens, 0) * 0.10 + COALESCE(output_tokens, 0) * 0.40) / 1000000.0 WHERE engine = 'gemini-api' AND cost_usd IS NULL;
    UPDATE usage_events SET cost_usd = 0.0 WHERE engine = 'nvidia' AND cost_usd IS NULL;
    UPDATE usage_events SET cost_usd = (COALESCE(input_tokens, 0) * 3.0 + COALESCE(output_tokens, 0) * 15.0) / 1000000.0 WHERE engine = 'claude' AND cost_usd IS NULL;
    "#,
];

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> AppResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> AppResult<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        migrate(&conn)?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> AppResult<T> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        Ok(f(&conn)?)
    }

    #[cfg(test)]
    pub fn schema_version(&self) -> AppResult<i64> {
        self.with(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
    }
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let current: usize = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(())
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
        db.with(|c| migrate(c)).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }
}

//! 감사 로그. 요청·답 본문은 기록하지 않고 메타데이터만 남긴다.

use crate::db::{now, Db};
use crate::error::AppResult;
use serde_json::Value;

pub fn record(db: &Db, actor: &str, event: &str, outcome: &str, meta: Value) {
    let meta = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".into());
    // 감사 기록 실패가 사용자 작업을 막지는 않지만 콘솔에는 남긴다.
    if let Err(e) = db.with(|c| {
        c.execute(
            "INSERT INTO audit_events(at, actor, event, outcome, meta_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![now(), actor, event, outcome, meta],
        )
    }) {
        eprintln!("audit write failed: {e}");
    }
}

/// 보존 기간이 지난 감사·사용량 이벤트를 삭제한다(앱 시작 시).
pub fn prune(db: &Db, retention_days: u32) -> AppResult<usize> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
    db.with(|c| {
        let a = c.execute("DELETE FROM audit_events WHERE at < ?1", [&cutoff])?;
        let b = c.execute("DELETE FROM usage_events WHERE created_at < ?1", [&cutoff])?;
        Ok(a + b)
    })
}

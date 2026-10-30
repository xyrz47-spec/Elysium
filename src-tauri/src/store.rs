//! 대화·메시지·CLI 세션·승인·사용량 저장.

use crate::db::{new_id, now, Db};
use crate::error::{AppError, AppResult};
use rusqlite::OptionalExtension;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    /// 이 대화의 작업 폴더. None이면 기본 작업 폴더.
    pub workspace_dir: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub data: Option<Value>,
    pub created_at: String,
}

fn title_from(text: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = t.chars().take(40).collect();
    if t.chars().count() > 40 {
        out.push('…');
    }
    if out.is_empty() { "새 대화".into() } else { out }
}

pub fn create_conversation(db: &Db, first_text: &str, workspace: Option<&str>) -> AppResult<String> {
    let id = new_id();
    let t = now();
    db.with(|c| {
        c.execute(
            "INSERT INTO conversations(id, title, workspace_dir, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![id, title_from(first_text), workspace, t],
        )
    })?;
    Ok(id)
}

pub fn get_workspace(db: &Db, id: &str) -> AppResult<Option<String>> {
    Ok(db
        .with(|c| c.query_row("SELECT workspace_dir FROM conversations WHERE id = ?1", [id], |r| r.get::<_, Option<String>>(0)).optional())?
        .flatten())
}

/// 대화의 작업 폴더를 바꾼다. CLI 세션은 폴더에 묶여 있으므로 함께 끊는다.
pub fn set_workspace(db: &Db, id: &str, workspace: Option<&str>) -> AppResult<()> {
    let n = db.with(|c| {
        let n = c.execute("UPDATE conversations SET workspace_dir = ?2 WHERE id = ?1", rusqlite::params![id, workspace])?;
        c.execute("DELETE FROM engine_sessions WHERE conversation_id = ?1", [id])?;
        Ok(n)
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("대화 {id}")));
    }
    Ok(())
}

pub fn conversation_exists(db: &Db, id: &str) -> AppResult<bool> {
    db.with(|c| c.query_row("SELECT 1 FROM conversations WHERE id = ?1", [id], |_| Ok(())).optional().map(|r| r.is_some()))
}

pub fn list_conversations(db: &Db) -> AppResult<Vec<Conversation>> {
    db.with(|c| {
        let mut st = c.prepare("SELECT id, title, workspace_dir, created_at, updated_at FROM conversations ORDER BY updated_at DESC LIMIT 500")?;
        let rows = st.query_map([], |r| Ok(Conversation { id: r.get(0)?, title: r.get(1)?, workspace_dir: r.get(2)?, created_at: r.get(3)?, updated_at: r.get(4)? }))?;
        rows.collect()
    })
}

pub fn rename_conversation(db: &Db, id: &str, title: &str) -> AppResult<()> {
    let n = db.with(|c| c.execute("UPDATE conversations SET title = ?2 WHERE id = ?1", rusqlite::params![id, title_from(title)]))?;
    if n == 0 {
        return Err(AppError::NotFound(format!("대화 {id}")));
    }
    Ok(())
}

pub fn delete_conversation(db: &Db, id: &str) -> AppResult<()> {
    db.with(|c| c.execute("DELETE FROM conversations WHERE id = ?1", [id]))?;
    Ok(())
}

pub fn insert_message(db: &Db, conversation_id: &str, role: &str, content: &str, data: Option<&Value>) -> AppResult<String> {
    insert_message_as(db, &new_id(), conversation_id, role, content, data)
}

/// 스트리밍 중에 쓰던 메시지 ID로 저장한다.
pub fn insert_message_as(db: &Db, id: &str, conversation_id: &str, role: &str, content: &str, data: Option<&Value>) -> AppResult<String> {
    let id = id.to_string();
    let t = now();
    let data = data.map(|d| d.to_string());
    db.with(|c| {
        c.execute(
            "INSERT INTO messages(id, conversation_id, role, content, data_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, conversation_id, role, content, data, t],
        )?;
        c.execute("UPDATE conversations SET updated_at = ?2 WHERE id = ?1", rusqlite::params![conversation_id, t])
    })?;
    Ok(id)
}

pub fn list_messages(db: &Db, conversation_id: &str) -> AppResult<Vec<Message>> {
    db.with(|c| {
        let mut st = c.prepare("SELECT id, conversation_id, role, content, data_json, created_at FROM messages WHERE conversation_id = ?1 ORDER BY created_at, rowid")?;
        let rows = st.query_map([conversation_id], |r| {
            let data: Option<String> = r.get(4)?;
            Ok(Message {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                data: data.and_then(|d| serde_json::from_str(&d).ok()),
                created_at: r.get(5)?,
            })
        })?;
        rows.collect()
    })
}

/// 최근 n개 메시지(오래된 것부터). 세션을 이어 쓸 수 없는 CLI에 대화 맥락으로 넘긴다.
pub fn get_session(db: &Db, conversation_id: &str, engine: &str) -> AppResult<Option<String>> {
    db.with(|c| {
        c.query_row("SELECT session_id FROM engine_sessions WHERE conversation_id = ?1 AND engine = ?2", [conversation_id, engine], |r| r.get(0))
            .optional()
    })
}

/// 세션을 마크다운으로: 제목, 폴더, 질문과 답(누가 답했는지), 세션 요약
pub fn conversation_markdown(db: &Db, conversation_id: &str) -> AppResult<(String, String)> {
    let c = list_conversations(db)?.into_iter().find(|c| c.id == conversation_id).ok_or_else(|| AppError::NotFound(format!("대화 {conversation_id}")))?;
    let msgs = list_messages(db, conversation_id)?;
    let (summary, _) = get_memory(db, conversation_id)?;
    let label = |v: Option<&Value>| v.and_then(|x| x.as_str()).map(|id| crate::engines::EngineId::parse(id).map(|e| e.label().to_string()).unwrap_or_else(|| id.to_string()));
    let mut md = format!("# {}\n\n- 내보낸 때: {}\n- 작업 폴더: {}\n- 메시지: {}개\n", c.title, chrono::Local::now().format("%Y-%m-%d %H:%M"), c.workspace_dir.as_deref().unwrap_or("기본 작업 폴더"), msgs.len());
    if let Some(s) = summary.filter(|s| !s.trim().is_empty()) {
        md.push_str(&format!("\n## 세션 요약\n\n{}\n", s.trim()));
    }
    let mut n = 0;
    for m in &msgs {
        if m.role == "user" {
            n += 1;
            md.push_str(&format!("\n---\n\n## {n}. 질문\n\n{}\n", m.content.trim()));
        } else {
            let d = m.data.as_ref();
            let by = label(d.and_then(|d| d.get("judgedBy"))).or_else(|| label(d.and_then(|d| d.get("reviewedBy")))).or_else(|| label(d.and_then(|d| d.get("draftedBy"))));
            md.push_str(&format!("\n### 답{}\n\n{}\n", by.map(|b| format!(" ({b})")).unwrap_or_default(), m.content.trim()));
        }
    }
    Ok((c.title, md))
}

/// 세션 메모리: (요약, 어디까지 접었는지)
pub fn get_memory(db: &Db, conversation_id: &str) -> AppResult<(Option<String>, Option<String>)> {
    db.with(|c| {
        c.query_row("SELECT memory_summary, memory_upto FROM conversations WHERE id = ?1", [conversation_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map(|o| o.unwrap_or((None, None)))
    })
}

pub fn set_memory(db: &Db, conversation_id: &str, summary: Option<&str>, upto: Option<&str>) -> AppResult<()> {
    db.with(|c| c.execute("UPDATE conversations SET memory_summary = ?1, memory_upto = ?2 WHERE id = ?3", rusqlite::params![summary, upto, conversation_id]))?;
    Ok(())
}

/// 이어 쓰는 엔진이 이 세션을 어디까지 봤는지(없으면 세션 생성 시각)
pub fn get_seen(db: &Db, conversation_id: &str, engine: &str) -> AppResult<Option<String>> {
    db.with(|c| {
        c.query_row("SELECT COALESCE(seen_at, updated_at) FROM engine_sessions WHERE conversation_id = ?1 AND engine = ?2", [conversation_id, engine], |r| r.get(0))
            .optional()
    })
}

pub fn set_seen(db: &Db, conversation_id: &str, engine: &str, at: &str) -> AppResult<()> {
    db.with(|c| c.execute("UPDATE engine_sessions SET seen_at = ?1 WHERE conversation_id = ?2 AND engine = ?3", rusqlite::params![at, conversation_id, engine]))?;
    Ok(())
}

pub fn set_session(db: &Db, conversation_id: &str, engine: &str, session_id: &str) -> AppResult<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO engine_sessions(conversation_id, engine, session_id, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(conversation_id, engine) DO UPDATE SET session_id = excluded.session_id, updated_at = excluded.updated_at",
            rusqlite::params![conversation_id, engine, session_id, now()],
        )
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 승인(작업 모드에서 파일을 고치기 전)

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    pub conversation_id: String,
    pub task_id: String,
    pub engine: String,
    pub summary: String,
    pub workspace: String,
    pub status: String,
    pub created_at: String,
    pub decided_at: Option<String>,
}

fn approval_row(r: &rusqlite::Row) -> rusqlite::Result<Approval> {
    Ok(Approval {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        task_id: r.get(2)?,
        engine: r.get(3)?,
        summary: r.get(4)?,
        workspace: r.get(5)?,
        status: r.get(6)?,
        created_at: r.get(7)?,
        decided_at: r.get(8)?,
    })
}

const APPROVAL_COLS: &str = "id, conversation_id, task_id, engine, summary, workspace, status, created_at, decided_at";

pub fn create_approval(db: &Db, conversation_id: &str, task_id: &str, engine: &str, summary: &str, workspace: &str) -> AppResult<Approval> {
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO approvals(id, conversation_id, task_id, engine, summary, workspace, status, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
            rusqlite::params![id, conversation_id, task_id, engine, summary, workspace, now()],
        )
    })?;
    get_approval(db, &id)
}

pub fn get_approval(db: &Db, id: &str) -> AppResult<Approval> {
    db.with(|c| c.query_row(&format!("SELECT {APPROVAL_COLS} FROM approvals WHERE id = ?1"), [id], approval_row).optional())?
        .ok_or_else(|| AppError::NotFound(format!("승인 {id}")))
}

pub fn pending_approvals(db: &Db) -> AppResult<Vec<Approval>> {
    db.with(|c| {
        let mut st = c.prepare(&format!("SELECT {APPROVAL_COLS} FROM approvals WHERE status = 'pending' ORDER BY created_at"))?;
        let rows = st.query_map([], approval_row)?;
        rows.collect()
    })
}

/// 대기 중인 승인만 결정할 수 있다(이미 결정·만료된 것은 바꾸지 않음).
pub fn decide_approval(db: &Db, id: &str, approve: bool) -> AppResult<Approval> {
    let status = if approve { "approved" } else { "rejected" };
    let n = db.with(|c| c.execute("UPDATE approvals SET status = ?2, decided_at = ?3 WHERE id = ?1 AND status = 'pending'", rusqlite::params![id, status, now()]))?;
    if n == 0 {
        return Err(AppError::Policy("이미 처리했거나 만료된 승인입니다".into()));
    }
    get_approval(db, id)
}

/// 앱을 다시 켜면 기다리던 작업은 사라졌으므로 남은 승인을 만료시킨다.
pub fn expire_pending_approvals(db: &Db) -> AppResult<usize> {
    db.with(|c| c.execute("UPDATE approvals SET status = 'expired', decided_at = ?1 WHERE status = 'pending'", [now()]))
}

// ---------------------------------------------------------------------------
// 사용량

pub struct Usage<'a> {
    pub engine: &'a str,
    pub role: &'a str,
    pub ok: bool,
    pub latency_ms: u64,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

pub fn record_usage(db: &Db, u: &Usage) -> AppResult<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO usage_events(created_at, engine, role, ok, latency_ms, input_tokens, output_tokens, cost_usd) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![now(), u.engine, u.role, u.ok, u.latency_ms as i64, u.input_tokens.map(|x| x as i64), u.output_tokens.map(|x| x as i64), u.cost_usd],
        )
    })?;
    Ok(())
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageToday {
    pub calls: u64,
    /// CLI가 알려 준 추정 비용 합계(구독 요금제에서는 실제 청구액이 아님).
    pub cost_usd: f64,
}

/// 오늘(현지 시각 0시부터) 합계
pub fn usage_today(db: &Db) -> AppResult<UsageToday> {
    let s = usage_summary(db, "today")?;
    Ok(UsageToday { calls: s.engines.iter().map(|e| e.calls).sum(), cost_usd: s.engines.iter().map(|e| e.cost_usd).sum() })
}

/// 엔진(CLI·API·JEV) 하나의 기간 사용량
#[derive(Debug, Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EngineUsage {
    pub engine: String,
    /// "cli" | "api" | "jev"
    pub kind: String,
    pub calls: u64,
    pub failed: u64,
    pub drafts: u64,
    pub reviews: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub avg_latency_ms: u64,
    pub last_at: Option<String>,
}

/// 하루(현지 날짜) 단위 합계
#[derive(Debug, Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    pub date: String,
    pub calls: u64,
    pub tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub range: String,
    pub engines: Vec<EngineUsage>,
    pub days: Vec<DayUsage>,
    pub pricing: Vec<crate::pricing::ModelPrice>,
}

fn engine_kind(engine: &str) -> &'static str {
    match crate::engines::EngineId::parse(engine) {
        Some(e) if e.is_api() => "api",
        Some(_) => "cli",
        None => "jev",
    }
}

/// range: "today"(오늘 0시부터, 현지 시각) | "7d" | "30d" | "all"
pub fn usage_summary(db: &Db, range: &str) -> AppResult<UsageSummary> {
    use chrono::{Duration, Local, TimeZone};
    let today = Local::now().date_naive();
    let from_day = match range {
        "today" => Some(today),
        "7d" => Some(today - Duration::days(6)),
        "30d" => Some(today - Duration::days(29)),
        _ => None,
    };
    let start = from_day
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(|d| Local.from_local_datetime(&d).single())
        .map(|d| d.with_timezone(&chrono::Utc).to_rfc3339())
        .unwrap_or_default();
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, String, bool, i64, Option<i64>, Option<i64>, Option<f64>)> = db.with(|c| {
        let mut st = c.prepare("SELECT created_at, engine, role, ok, latency_ms, input_tokens, output_tokens, cost_usd FROM usage_events WHERE created_at >= ?1 ORDER BY created_at")?;
        let it = st.query_map([&start], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?;
        it.collect()
    })?;

    let mut by: std::collections::BTreeMap<String, (EngineUsage, u64)> = Default::default();
    let mut days: std::collections::BTreeMap<String, DayUsage> = Default::default();
    // 기간이 정해져 있으면 빈 날도 0으로 채워 그래프가 끊기지 않게 한다.
    if let Some(mut d) = from_day {
        while d <= today {
            let k = d.format("%Y-%m-%d").to_string();
            days.insert(k.clone(), DayUsage { date: k, ..Default::default() });
            d += Duration::days(1);
        }
    }
    for (at, engine, role, ok, ms, inp, out, cost) in rows {
        let tokens = inp.unwrap_or(0).max(0) as u64 + out.unwrap_or(0).max(0) as u64;
        let (u, ms_sum) = by.entry(engine.clone()).or_insert_with(|| (EngineUsage { kind: engine_kind(&engine).into(), engine: engine.clone(), ..Default::default() }, 0));
        u.calls += 1;
        u.failed += u64::from(!ok);
        u.drafts += u64::from(role == "draft");
        u.reviews += u64::from(role == "review");
        u.input_tokens += inp.unwrap_or(0).max(0) as u64;
        u.output_tokens += out.unwrap_or(0).max(0) as u64;
        let cost_val = cost.or_else(|| crate::pricing::estimate_cost_by_str(&engine, None, inp.map(|x| x as u64), out.map(|x| x as u64))).unwrap_or(0.0);
        u.cost_usd += cost_val;
        *ms_sum += ms.max(0) as u64;
        u.last_at = Some(at.clone());
        let day = chrono::DateTime::parse_from_rfc3339(&at).map(|d| d.with_timezone(&Local).format("%Y-%m-%d").to_string()).unwrap_or_else(|_| at.chars().take(10).collect());
        let d = days.entry(day.clone()).or_insert_with(|| DayUsage { date: day, ..Default::default() });
        d.calls += 1;
        d.tokens += tokens;
        d.cost_usd += cost_val;
    }
    let mut engines: Vec<EngineUsage> = by
        .into_values()
        .map(|(mut u, ms)| {
            u.avg_latency_ms = ms / u.calls.max(1);
            u
        })
        .collect();
    engines.sort_by(|a, b| b.calls.cmp(&a.calls).then(a.engine.cmp(&b.engine)));
    Ok(UsageSummary { range: range.into(), engines, days: days.into_values().collect(), pricing: crate::pricing::all_prices() })
}

// ---------------------------------------------------------------------------
// 스킬·플러그인 켜기/끄기(기본은 켜짐)

pub fn toggles(db: &Db) -> AppResult<std::collections::HashMap<String, bool>> {
    db.with(|c| {
        let mut st = c.prepare("SELECT key, enabled FROM toggles")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)))?;
        rows.collect()
    })
}

pub fn set_toggle(db: &Db, key: &str, enabled: bool) -> AppResult<()> {
    db.with(|c| c.execute("INSERT INTO toggles(key, enabled) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET enabled = excluded.enabled", rusqlite::params![key, enabled]))?;
    Ok(())
}

pub fn delete_all(db: &Db) -> AppResult<()> {
    db.with(|c| c.execute_batch("DELETE FROM messages; DELETE FROM engine_sessions; DELETE FROM conversations; DELETE FROM approvals; DELETE FROM usage_events; DELETE FROM audit_events;"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_flow_and_sessions() {
        let db = Db::open_in_memory().unwrap();
        let cid = create_conversation(&db, "   러스트로 웹 서버를   만드는 방법을 아주 자세하게 설명해 주세요 그리고 예제도 보여 주세요", None).unwrap();
        let list = list_conversations(&db).unwrap();
        assert!(list[0].title.ends_with('…') && !list[0].title.contains("  "));
        insert_message(&db, &cid, "user", "안녕", None).unwrap();
        insert_message(&db, &cid, "assistant", "반가워요", Some(&serde_json::json!({"engine": "claude"}))).unwrap();
        let ms = list_messages(&db, &cid).unwrap();
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[1].data.as_ref().unwrap()["engine"], "claude");
        let (_, md) = conversation_markdown(&db, &cid).unwrap();
        assert!(md.contains("## 1. 질문\n\n안녕") && md.contains("### 답\n\n반가워요"), "{md}");
        assert_eq!(get_memory(&db, &cid).unwrap(), (None, None));
        set_memory(&db, &cid, Some("- 이름: 호야"), Some("2026-10-17T00:00:00+00:00")).unwrap();
        assert_eq!(get_memory(&db, &cid).unwrap().0.as_deref(), Some("- 이름: 호야"));
        assert!(get_session(&db, &cid, "claude").unwrap().is_none());
        set_session(&db, &cid, "claude", "s1").unwrap();
        set_session(&db, &cid, "claude", "s2").unwrap();
        assert_eq!(get_session(&db, &cid, "claude").unwrap().as_deref(), Some("s2"));
        set_seen(&db, &cid, "claude", "2026-10-17T09:00:00+00:00").unwrap();
        assert_eq!(get_seen(&db, &cid, "claude").unwrap().as_deref(), Some("2026-10-17T09:00:00+00:00"));
        // 폴더를 바꾸면 세션이 끊긴다
        assert!(get_workspace(&db, &cid).unwrap().is_none());
        set_workspace(&db, &cid, Some("C:\\proj")).unwrap();
        assert_eq!(get_workspace(&db, &cid).unwrap().as_deref(), Some("C:\\proj"));
        assert!(get_session(&db, &cid, "claude").unwrap().is_none(), "폴더를 바꾸면 세션 초기화");
        assert_eq!(list_conversations(&db).unwrap()[0].workspace_dir.as_deref(), Some("C:\\proj"));
        set_workspace(&db, &cid, None).unwrap();
        assert!(get_workspace(&db, &cid).unwrap().is_none());
        assert!(set_workspace(&db, "없음", None).is_err());
        let other = create_conversation(&db, "x", Some("D:\\w")).unwrap();
        assert_eq!(get_workspace(&db, &other).unwrap().as_deref(), Some("D:\\w"));
        delete_conversation(&db, &cid).unwrap();
        assert!(list_messages(&db, &cid).unwrap().is_empty(), "메시지도 함께 지워짐");
        assert!(get_session(&db, &cid, "claude").unwrap().is_none());
    }

    #[test]
    fn approvals_decide_once_and_expire() {
        let db = Db::open_in_memory().unwrap();
        let a = create_approval(&db, "c", "t", "claude", "파일 수정", "C:\\w").unwrap();
        assert_eq!(pending_approvals(&db).unwrap().len(), 1);
        assert_eq!(decide_approval(&db, &a.id, true).unwrap().status, "approved");
        assert!(decide_approval(&db, &a.id, false).is_err(), "두 번 결정할 수 없음");
        let b = create_approval(&db, "c", "t2", "claude", "x", "C:\\w").unwrap();
        assert_eq!(expire_pending_approvals(&db).unwrap(), 1);
        assert_eq!(get_approval(&db, &b.id).unwrap().status, "expired");
    }

    #[test]
    fn usage_and_toggles() {
        let db = Db::open_in_memory().unwrap();
        record_usage(&db, &Usage { engine: "claude", role: "draft", ok: true, latency_ms: 10, input_tokens: Some(5), output_tokens: Some(3), cost_usd: Some(0.02) }).unwrap();
        record_usage(&db, &Usage { engine: "claude", role: "review", ok: false, latency_ms: 30, input_tokens: None, output_tokens: None, cost_usd: None }).unwrap();
        record_usage(&db, &Usage { engine: "nvidia", role: "draft", ok: true, latency_ms: 100, input_tokens: Some(40), output_tokens: Some(60), cost_usd: Some(0.0) }).unwrap();
        record_usage(&db, &Usage { engine: "jev", role: "judge", ok: true, latency_ms: 200, input_tokens: None, output_tokens: None, cost_usd: Some(0.001) }).unwrap();
        let s = usage_summary(&db, "7d").unwrap();
        assert_eq!(s.days.len(), 7, "빈 날도 채운다");
        let claude = s.engines.iter().find(|e| e.engine == "claude").unwrap();
        assert_eq!((claude.kind.as_str(), claude.calls, claude.failed, claude.drafts, claude.reviews, claude.avg_latency_ms), ("cli", 2, 1, 1, 1, 20));
        assert_eq!(s.engines.iter().find(|e| e.engine == "nvidia").map(|e| (e.kind.as_str(), e.output_tokens)), Some(("api", 60)));
        assert_eq!(s.engines.iter().find(|e| e.engine == "jev").map(|e| e.kind.as_str()), Some("jev"));
        assert_eq!(s.days.last().map(|d| (d.calls, d.tokens)), Some((4, 108)));
        assert_eq!(usage_summary(&db, "all").unwrap().engines.len(), 3);
        let u = usage_today(&db).unwrap();
        assert_eq!(u.calls, 4);
        assert!((u.cost_usd - 0.021).abs() < 1e-9);
        set_toggle(&db, "skill:x", false).unwrap();
        assert_eq!(toggles(&db).unwrap().get("skill:x"), Some(&false));
    }
}

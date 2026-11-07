//! Interrupted tasks can be picked up again.
//! - While a task runs, a small checkpoint file (request + progress) is kept in `<data>/interrupted/`.
//!   If the app closes or crashes, the next start turns each leftover file into a "stopped" answer.
//! - "Continue" on a stopped, failed or interrupted answer sends the original request again in the same mode,
//!   with the progress so far attached as a file, so the AI checks the current state and does only what is left.

use crate::orchestrator::{self, AppState};
use crate::store;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, State};

/// How often the progress of a running task is written down.
const CHECKPOINT_EVERY: Duration = Duration::from_secs(20);
/// Size limits of the progress note handed to the AI
const NOTE_AGENT_CHARS: usize = 600;
const NOTE_TIMELINE: usize = 25;
const NOTE_COMMANDS: usize = 30;
const NOTE_REQUEST_CHARS: usize = 6000;

pub const INTERRUPTED_TEXT: &str = "⚠ 앱이 종료되어 작업이 중단되었습니다. '이어서 진행' 버튼을 누르면 중단된 위치부터 작업을 계속할 수 있습니다.";
const CONTINUE_HEAD: &str = "[이어서 진행]
이전에 진행되다 중단된 요청이며, 첨부된 진행 노트에서 이전 작업 내용을 확인할 수 있습니다. 먼저 git status, git diff 및 기존 파일 등 작업 폴더의 현재 상태를 점검하여 이미 완료된 작업을 반복하지 마세요. 확인 후 중단된 위치부터 남은 작업만 이어서 진행하세요.

원래 요청:";
const NOTE_TITLE: &str = "중단 전 작업 진행 기록";

fn dir(st: &AppState) -> PathBuf {
    st.data_dir.join("interrupted")
}

fn file(st: &AppState, task: &str) -> PathBuf {
    dir(st).join(format!("{task}.json"))
}

/// Starts keeping a checkpoint for this task. Returns the handle that keeps it up to date.
pub fn begin(st: &Arc<AppState>, task: &str, conv: &str, user_message_id: &str) -> tauri::async_runtime::JoinHandle<()> {
    let _ = std::fs::create_dir_all(dir(st));
    let base = json!({ "taskId": task, "conversationId": conv, "userMessageId": user_message_id, "startedAt": crate::db::now() });
    let path = file(st, task);
    let _ = std::fs::write(&path, base.to_string());
    let task = task.to_string();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(CHECKPOINT_EVERY).await;
            let Some(snap) = crate::remote::task_snapshot(&task) else { continue };
            let mut v = base.clone();
            v["progress"] = serde_json::to_value(snap).unwrap_or(Value::Null);
            // Write next to it and rename, so a crash mid-write never leaves a broken file.
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, v.to_string()).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    })
}

/// The task finished (its answer is saved): no checkpoint needed any more.
pub fn end(st: &AppState, task: &str, keeper: tauri::async_runtime::JoinHandle<()>) {
    keeper.abort();
    let _ = std::fs::remove_file(file(st, task));
    let _ = std::fs::remove_file(file(st, task).with_extension("tmp"));
}

/// At start-up: every leftover checkpoint is a task the last run did not finish. Save it as a stopped answer.
pub fn recover(st: &AppState) {
    let Ok(entries) = std::fs::read_dir(dir(st)) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            let _ = std::fs::remove_file(&p);
            continue;
        }
        let v: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if let Some(conv) = v["conversationId"].as_str().filter(|c| store::conversation_exists(&st.db, c).unwrap_or(false)) {
            let data = json!({ "error": INTERRUPTED_TEXT, "interrupted": v });
            let _ = store::insert_message(&st.db, conv, "assistant", INTERRUPTED_TEXT, Some(&data));
        }
        let _ = std::fs::remove_file(&p);
    }
}

fn cut(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// What was done before the stop, as a markdown note for the AI.
fn progress_note(answer: &store::Message) -> String {
    let mut s = format!("# {NOTE_TITLE}\n\n");
    let d = answer.data.clone().unwrap_or_default();
    if let Some(p) = d["interrupted"]["progress"].as_object() {
        for (key, a) in p.get("agents").and_then(Value::as_object).into_iter().flatten() {
            let title = p.get("parts").and_then(|x| x[key].as_str()).unwrap_or(a["label"].as_str().unwrap_or(key));
            s.push_str(&format!("## {title} ({})\n", a["state"].as_str().unwrap_or("?")));
            if let Some(l) = a["last"].as_str() {
                s.push_str(&format!("- last: {}\n", cut(l, 200)));
            }
            if let Some(t) = a["tail"].as_str().filter(|t| !t.trim().is_empty()) {
                s.push_str(&format!("\n{}\n", cut(t.trim(), NOTE_AGENT_CHARS)));
            }
            s.push('\n');
        }
        timeline(&mut s, p.get("timeline"));
    }
    let team = &d["team"];
    for a in team["agents"].as_array().into_iter().flatten() {
        s.push_str(&format!("## {} ({})\n", a["title"].as_str().unwrap_or("-"), if a["ok"].as_bool() == Some(true) { "ok" } else { "stopped/failed" }));
        for f in a["files"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            s.push_str(&format!("- {f}\n"));
        }
        if let Some(t) = a["text"].as_str().filter(|t| !t.trim().is_empty()) {
            s.push_str(&format!("\n{}\n", cut(t.trim(), NOTE_AGENT_CHARS)));
        }
        s.push('\n');
    }
    timeline(&mut s, team.get("leadLog"));
    let ran: Vec<&str> = team["decisions"].as_array().into_iter().flatten().filter_map(|x| x["command"].as_str()).collect();
    if !ran.is_empty() {
        s.push_str("## commands\n");
        for c in ran.iter().rev().take(NOTE_COMMANDS).rev() {
            s.push_str(&format!("- `{}`\n", cut(c, 200)));
        }
        s.push('\n');
    }
    if !answer.content.trim().is_empty() {
        s.push_str(&format!("## answer\n{}\n", cut(answer.content.trim(), 2000)));
    }
    s
}

fn timeline(s: &mut String, v: Option<&Value>) {
    let items: Vec<&Value> = v.and_then(Value::as_array).into_iter().flatten().collect();
    if items.is_empty() {
        return;
    }
    s.push_str("## lead\n");
    for n in items.iter().rev().take(NOTE_TIMELINE).rev() {
        s.push_str(&format!("- {}\n", cut(n["text"].as_str().unwrap_or(""), 240)));
    }
    s.push('\n');
}

/// "Continue" on a stopped/failed/interrupted answer.
#[tauri::command]
pub fn chat_continue(app: AppHandle, st: State<'_, Arc<AppState>>, conversation_id: String, message_id: String) -> Result<orchestrator::SendResult, String> {
    let msgs = store::list_messages(&st.db, &conversation_id).map_err(|e| e.to_string())?;
    let at = msgs.iter().position(|m| m.id == message_id).ok_or("message not found")?;
    let answer = &msgs[at];
    let ask = msgs[..at].iter().rev().find(|m| m.role == "user").ok_or("no request before this answer")?;
    let d = ask.data.clone().unwrap_or_default();
    let note = progress_note(answer);
    let att = crate::attachments::add_bytes(&st.data_dir, "progress.md", note.as_bytes(), "user")?;
    // A continued request keeps the original text after the header (not the header of an earlier continue).
    let original = ask.content.split_once(CONTINUE_HEAD.trim_end()).map(|(_, rest)| rest.trim()).unwrap_or(ask.content.trim());
    let text = format!("{}\n{}", CONTINUE_HEAD.trim_end(), cut(original, NOTE_REQUEST_CHARS));
    let engine = d["engine"].as_str().and_then(crate::engines::EngineId::parse);
    let work = d["work"].as_bool().unwrap_or(false);
    let build = d["build"].as_bool().unwrap_or(false);
    let council = d["askedCouncil"].as_bool().unwrap_or(false);
    let setup = serde_json::from_value(d["setup"].clone()).unwrap_or_default();
    orchestrator::send(&app, st.inner(), Some(conversation_id), text, engine, work, build, None, council, vec![att.id], setup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(content: &str, data: Value) -> store::Message {
        store::Message { id: "m".into(), conversation_id: "c".into(), role: "assistant".into(), content: content.into(), data: Some(data), created_at: String::new() }
    }

    #[test]
    fn note_lists_workers_lead_log_and_commands() {
        let d = json!({ "error": "x", "team": {
            "agents": [{ "title": "화면", "ok": true, "text": "버튼을 고쳤습니다", "files": ["src/a.tsx"] }, { "title": "서버", "ok": false, "text": "" }],
            "leadLog": [{ "text": "워커 1 반영" }],
            "decisions": [{ "command": "npm test", "verdict": "ran" }]
        }});
        let n = progress_note(&msg("⚠ 중지했습니다.", d));
        for want in ["## 화면 (ok)", "- src/a.tsx", "버튼을 고쳤습니다", "## 서버 (stopped/failed)", "워커 1 반영", "`npm test`"] {
            assert!(n.contains(want), "{want}\n{n}");
        }
    }

    #[test]
    fn interrupted_progress_is_used_when_there_is_no_team_record() {
        let d = json!({ "interrupted": { "progress": { "parts": { "agent:0": "문서" }, "agents": { "agent:0": { "label": "Claude Code", "state": "run", "last": "Edit · a.md", "tail": "절반 정도 썼습니다" } }, "timeline": [] } } });
        let n = progress_note(&msg(INTERRUPTED_TEXT, d));
        assert!(n.contains("## 문서 (run)") && n.contains("Edit · a.md") && n.contains("절반 정도"), "{n}");
    }
}

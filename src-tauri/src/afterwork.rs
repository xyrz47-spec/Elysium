//! Shut down / restart / log off after a task (not to be confused with power.rs, the sleep mode), when the user asked for it and the
//! "allow shutdown when done" switch is on. Agents never run these commands themselves
//! (lead::POWER_DENY); Elysium runs it once the answer is saved and every task has finished,
//! after a cancellable countdown.

use crate::orchestrator::{emit, AppState};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

/// Seconds shown before the PC actually shuts down, restarts or logs off.
pub const COUNTDOWN_SECS: u64 = 60;
/// How often to check whether other tasks have finished.
const WAIT_POLL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Shutdown,
    Restart,
    Logoff,
}

impl Act {
    pub fn as_str(self) -> &'static str {
        match self {
            Act::Shutdown => "shutdown",
            Act::Restart => "restart",
            Act::Logoff => "logoff",
        }
    }
    fn flag(self) -> &'static str {
        match self {
            Act::Shutdown => "/s",
            Act::Restart => "/r",
            Act::Logoff => "/l",
        }
    }
}

/// What the user asked for, if the text asks to shut down, restart or log off the PC.
pub fn act_of(text: &str) -> Option<Act> {
    if !crate::lead::asks_power(text) {
        return None;
    }
    let t = text.to_lowercase().replace(char::is_whitespace, "");
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    Some(if has(&["재시작", "재부팅", "리부팅", "다시시작", "restart", "reboot"]) {
        Act::Restart
    } else if has(&["로그오프", "로그아웃", "logoff", "logout", "signout"]) {
        Act::Logoff
    } else {
        Act::Shutdown
    })
}

fn requests() -> &'static Mutex<HashMap<String, Act>> {
    static R: OnceLock<Mutex<HashMap<String, Act>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn pending() -> &'static Mutex<Option<CancellationToken>> {
    static P: OnceLock<Mutex<Option<CancellationToken>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

/// Remembers a shutdown/restart/log off request made in this task (the request text or a later message).
pub fn note(task: &str, text: &str) {
    if let Some(a) = act_of(text) {
        requests().lock().unwrap_or_else(|p| p.into_inner()).insert(task.to_string(), a);
    }
}

pub fn requested(task: &str) -> Option<Act> {
    requests().lock().unwrap_or_else(|p| p.into_inner()).get(task).copied()
}

/// Called when a task is over (answer saved). Runs the requested action if the switch is on.
pub async fn after_task(app: &AppHandle, st: &AppState, task: &str, conv: &str, stopped: bool) {
    let Some(act) = requests().lock().unwrap_or_else(|p| p.into_inner()).remove(task) else { return };
    if stopped {
        return;
    }
    if !st.settings().power_on_done {
        emit(app, "power:skipped", json!({ "taskId": task, "conversationId": conv, "kind": act.as_str(), "reason": "off" }));
        return;
    }
    // Other tasks would be cut off: wait until they are done. Turning the switch off meanwhile cancels.
    while !st.tasks.lock().unwrap_or_else(|p| p.into_inner()).is_empty() {
        tokio::time::sleep(WAIT_POLL).await;
        if !st.settings().power_on_done {
            emit(app, "power:cancelled", json!({ "kind": act.as_str() }));
            return;
        }
    }
    let token = CancellationToken::new();
    if let Some(old) = pending().lock().unwrap_or_else(|p| p.into_inner()).replace(token.clone()) {
        old.cancel();
    }
    emit(app, "power:pending", json!({ "taskId": task, "conversationId": conv, "kind": act.as_str(), "seconds": COUNTDOWN_SECS }));
    let cancelled = tokio::select! {
        _ = token.cancelled() => true,
        _ = tokio::time::sleep(Duration::from_secs(COUNTDOWN_SECS)) => false,
    };
    pending().lock().unwrap_or_else(|p| p.into_inner()).take();
    if cancelled || !st.settings().power_on_done {
        emit(app, "power:cancelled", json!({ "kind": act.as_str() }));
        crate::audit::record(&st.db, "user", "power.cancelled", act.as_str(), json!({}));
        return;
    }
    crate::audit::record(&st.db, "elysium", "power.run", act.as_str(), json!({ "task": task }));
    let mut cmd = std::process::Command::new("shutdown.exe");
    cmd.arg(act.flag());
    if act != Act::Logoff {
        cmd.args(["/t", "0"]);
    }
    if let Err(e) = cmd.spawn() {
        emit(app, "power:failed", json!({ "kind": act.as_str(), "error": e.to_string() }));
    }
}

/// Cancels a countdown in progress.
#[tauri::command]
pub fn power_cancel() -> bool {
    match pending().lock().unwrap_or_else(|p| p.into_inner()).take() {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_requested_action() {
        assert_eq!(act_of("다 끝나면 컴퓨터 꺼 줘"), Some(Act::Shutdown));
        assert_eq!(act_of("끝나면 PC 재시작해줘"), Some(Act::Restart));
        assert_eq!(act_of("완료 후 재부팅"), Some(Act::Restart));
        assert_eq!(act_of("다 하면 컴퓨터 로그아웃"), Some(Act::Logoff));
        assert_eq!(act_of("서버 재시작해줘"), None);
    }

    #[test]
    fn requests_are_per_task() {
        note("t-a", "끝나면 컴퓨터 꺼 줘");
        note("t-b", "테스트 돌려줘");
        assert_eq!(requested("t-a"), Some(Act::Shutdown));
        assert_eq!(requested("t-b"), None);
        note("t-b", "다 하면 PC 재시작");
        assert_eq!(requested("t-b"), Some(Act::Restart));
    }
}

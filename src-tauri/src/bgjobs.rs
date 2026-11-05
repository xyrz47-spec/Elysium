//! 에이전트가 뒤에서 돌리는 명령을 사람이 볼 수 있게 모은다.
//! Claude Code가 돌리는 백그라운드 셸은 출력 파일을 읽고, Elysium이 워커 대신 돌리는 명령은 줄을 메모리에 모은다.

use crate::orchestrator::emit;
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tokio_util::sync::CancellationToken;

/// 보관하는 작업 수. 넘으면 오래된 끝난 작업부터 버린다.
const KEEP: usize = 300;
/// 한 번에 돌려주는 출력 끝부분(바이트)
const READ_TAIL: u64 = 96 * 1024;
/// Elysium이 돌리는 명령의 보관 줄 수
const LOG_KEEP: usize = 3000;
/// Claude 셸이 끝났는지 출력 파일 끝을 보는 간격
const WATCH: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub task_id: String,
    /// 이 명령을 돌린 실행(에이전트 탭의 key)
    pub key: String,
    pub command: String,
    /// "claude": Claude Code의 백그라운드 셸, "elysium": Elysium이 워커 대신 실행
    pub source: &'static str,
    /// run · ok · fail · stopped
    pub state: &'static str,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// Elysium이 직접 돌리는 명령만 여기서 멈출 수 있다.
    pub stoppable: bool,
}

struct Job {
    view: JobView,
    seq: u64,
    path: Option<PathBuf>,
    log: VecDeque<String>,
    stop: Option<CancellationToken>,
}

type Table = Arc<Mutex<HashMap<String, Job>>>;

#[derive(Default)]
pub struct Jobs {
    all: Table,
    seq: AtomicU64,
    /// Elysium 명령 번호
    ids: AtomicU64,
}

/// Claude Code가 알려 준 출력 파일만 읽는다(임시 폴더의 claude 아래 .output).
fn allowed_output(path: &Path) -> bool {
    let root = std::env::temp_dir().join("claude");
    path.extension().is_some_and(|e| e == "output") && path.starts_with(&root) && !path.components().any(|c| matches!(c, std::path::Component::ParentDir))
}

#[cfg(test)]
pub fn tests_allowed(path: &Path) -> bool {
    allowed_output(path)
}

/// Claude Code는 셸이 끝나면 출력 파일 끝에 "[exited with code N]"을 쓴다.
fn exit_code(tail: &str) -> Option<i64> {
    let (_, rest) = tail.rsplit_once("[exited with code ")?;
    rest.split_once(']')?.0.trim().parse().ok()
}

fn finish_in(all: &Mutex<HashMap<String, Job>>, app: &AppHandle, id: &str, state: &'static str) {
    let mut all = all.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(j) = all.get_mut(id).filter(|j| j.view.state == "run") {
        j.view.state = state;
        j.view.ended_at = Some(crate::db::now());
        j.stop = None;
        Jobs::publish(app, &j.view);
    }
}

/// 파일의 끝부분. 잘린 첫 줄은 버린다.
fn tail_of(path: &Path, max: u64) -> Result<String, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let from = len.saturating_sub(max);
    f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let s = String::from_utf8_lossy(&buf).replace("\r\n", "\n");
    Ok(if from > 0 { s.split_once('\n').map(|(_, rest)| rest.to_string()).unwrap_or(s) } else { s })
}

impl Jobs {
    fn publish(app: &AppHandle, v: &JobView) {
        emit(app, "task:bg-job", json!(v));
    }

    fn insert(&self, app: &AppHandle, view: JobView, path: Option<PathBuf>, stop: Option<CancellationToken>) {
        let mut all = self.all.lock().unwrap_or_else(|p| p.into_inner());
        if all.contains_key(&view.id) {
            return;
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        Self::publish(app, &view);
        all.insert(view.id.clone(), Job { view, seq, path, log: VecDeque::new(), stop });
        if all.len() > KEEP {
            let mut done: Vec<(u64, String)> = all.values().filter(|j| j.view.state != "run").map(|j| (j.seq, j.view.id.clone())).collect();
            done.sort();
            for (_, id) in done.into_iter().take(all.len() - KEEP) {
                all.remove(&id);
            }
        }
    }

    /// Claude Code가 백그라운드 셸을 시작했다(id는 Claude가 준 번호).
    pub fn start_claude(&self, app: &AppHandle, task: &str, key: &str, cid: &str, command: &str, path: PathBuf) {
        if !allowed_output(&path) {
            return;
        }
        let id = format!("c:{cid}");
        let view = JobView { id: id.clone(), task_id: task.into(), key: key.into(), command: command.into(), source: "claude", state: "run", started_at: crate::db::now(), ended_at: None, stoppable: false };
        self.insert(app, view, Some(path.clone()), None);
        // 알림이 오지 않아도 끝난 것을 알 수 있게 출력 파일 끝을 본다.
        let (all, app) = (self.all.clone(), app.clone());
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(WATCH).await;
                if all.lock().unwrap_or_else(|p| p.into_inner()).get(&id).is_none_or(|j| j.view.state != "run") {
                    return;
                }
                let p = path.clone();
                let code = tokio::task::spawn_blocking(move || tail_of(&p, 256).ok().and_then(|t| exit_code(&t))).await.ok().flatten();
                if let Some(c) = code {
                    finish_in(&all, &app, &id, if c == 0 { "ok" } else { "fail" });
                    return;
                }
            }
        });
    }

    /// Elysium이 워커 대신 명령을 시작한다. 돌려준 id로 줄을 보태고 끝낸다.
    pub fn start_elysium(&self, app: &AppHandle, task: &str, key: &str, command: &str, stop: CancellationToken) -> String {
        let id = format!("e:{}", self.ids.fetch_add(1, Ordering::Relaxed));
        let view = JobView { id: id.clone(), task_id: task.into(), key: key.into(), command: command.into(), source: "elysium", state: "run", started_at: crate::db::now(), ended_at: None, stoppable: true };
        self.insert(app, view, None, Some(stop));
        id
    }

    pub fn push_line(&self, id: &str, line: &str) {
        if let Some(j) = self.all.lock().unwrap_or_else(|p| p.into_inner()).get_mut(id) {
            j.log.push_back(line.to_string());
            if j.log.len() > LOG_KEEP {
                j.log.pop_front();
            }
        }
    }

    /// 끝났다. 이미 끝난 작업은 그대로 둔다.
    pub fn finish(&self, app: &AppHandle, id: &str, state: &'static str) {
        finish_in(&self.all, app, id, state);
    }

    /// Claude Code의 작업 알림(status: completed·failed·killed·stopped)
    pub fn finish_claude(&self, app: &AppHandle, cid: &str, status: &str) {
        let state = match status {
            "completed" => "ok",
            "failed" => "fail",
            _ => "stopped",
        };
        self.finish(app, &format!("c:{cid}"), state);
    }

    /// 실행(프로세스)이 끝나면 그 안에서 돌던 Claude 셸도 함께 끝난다.
    pub fn end_run(&self, app: &AppHandle, task: &str, key: &str) {
        let ids: Vec<String> = self.all.lock().unwrap_or_else(|p| p.into_inner()).values().filter(|j| j.view.source == "claude" && j.view.state == "run" && j.view.task_id == task && j.view.key == key).map(|j| j.view.id.clone()).collect();
        for id in ids {
            self.finish(app, &id, "stopped");
        }
    }

    /// 시작한 순서대로. task를 주면 그 작업만.
    pub fn list(&self, task: Option<&str>) -> Vec<JobView> {
        let all = self.all.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<(u64, JobView)> = all.values().filter(|j| task.is_none_or(|t| j.view.task_id == t)).map(|j| (j.seq, j.view.clone())).collect();
        v.sort_by_key(|(s, _)| *s);
        v.into_iter().map(|(_, j)| j).collect()
    }

    /// 지금까지의 출력 끝부분
    pub fn read(&self, id: &str) -> Result<String, String> {
        let (path, lines) = {
            let all = self.all.lock().unwrap_or_else(|p| p.into_inner());
            let j = all.get(id).ok_or("명령어 출력이 삭제되었거나 존재하지 않습니다.")?;
            (j.path.clone(), j.log.iter().cloned().collect::<Vec<_>>())
        };
        match path {
            Some(p) => tail_of(&p, READ_TAIL),
            None => {
                let s = lines.join("\n");
                let cut = s.len().saturating_sub(READ_TAIL as usize);
                let start = (cut..=s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
                Ok(s[start..].to_string())
            }
        }
    }

    /// Elysium이 돌리는 명령을 멈춘다.
    pub fn stop(&self, id: &str) -> bool {
        let all = self.all.lock().unwrap_or_else(|p| p.into_inner());
        match all.get(id).and_then(|j| j.stop.as_ref()) {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_claude_output_files_under_temp_are_read() {
        let t = std::env::temp_dir().join("claude");
        assert!(allowed_output(&t.join("abc").join("sid").join("tasks").join("b1.output")));
        assert!(!allowed_output(&t.join("abc").join("secret.txt")));
        assert!(!allowed_output(&t.join("..").join("x.output")));
        assert!(!allowed_output(Path::new("C:\\Users\\x\\.ssh\\id.output")));
    }

    #[test]
    fn exit_marker_tells_the_result() {
        assert_eq!(exit_code("bg-done-42

[exited with code 0]
"), Some(0));
        assert_eq!(exit_code("x
[exited with code 1]
"), Some(1));
        assert_eq!(exit_code("still running"), None);
    }

    #[test]
    fn tail_drops_the_cut_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.output");
        std::fs::write(&p, "first line\r\nsecond\nthird\n").unwrap();
        assert_eq!(tail_of(&p, 1000).unwrap(), "first line\nsecond\nthird\n");
        assert_eq!(tail_of(&p, 13).unwrap(), "third\n");
    }
}

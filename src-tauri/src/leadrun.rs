//! 총괄과 워커를 잇는 실행 흐름: 요청 검토, 명령 실행, 창 테스트 대기열, 사용자 질문, 워커 그룹.

use crate::audit;
use crate::db::new_id;
use crate::engines::{EngineId, Outcome, RunSpec};
use crate::jev;
use crate::lead::{self, Asked, Kind, Matched, Policy, Verdict, WorkerRequest};
use crate::orchestrator::{self, add_opt, cut, emit, AppState};
use crate::gitops;
use crate::store;
use crate::teamwork;
use crate::testqueue::TestItem;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::{oneshot, Notify};
use tokio_util::sync::CancellationToken;

/// 워커 하나가 쓸 수 있는 차례 수
const MAX_TURNS: u32 = 40;
/// 총괄 한 번을 기다리는 최대 시간
const LEAD_TIMEOUT: Duration = Duration::from_secs(300);
/// 총괄이 메인 폴더에서 직접 고치는 한 번(커밋 검토·직접 작업)을 기다리는 최대 시간
const LEAD_WORK_TIMEOUT: Duration = Duration::from_secs(1200);
/// 총괄 활동 기록을 몇 개까지 둘지
const TIMELINE_KEEP: usize = 300;
/// 창 테스트 로그를 화면에 보내는 간격
const LOG_FLUSH: Duration = Duration::from_millis(250);
const JEV_SAFE: f64 = 0.85;
const JEV_SCOPE: f64 = 0.7;
const JEV_WINDOW: f64 = 0.5;
const JEV_MANUAL: f64 = 0.6;
const JEV_BLOCKED: f64 = 0.6;
const REQUEST_CHARS: usize = 6000;

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerInfo {
    pub key: String,
    pub title: String,
    pub group: Option<usize>,
}

/// 총괄이 지금 단계에 더하는 워커
#[derive(Debug, Clone)]
pub struct SpawnReq {
    pub title: String,
    pub task: String,
    pub engine: Option<EngineId>,
}

/// 총괄이 지운 워커 표시(총괄에게 보이는 워커 목록)
const REMOVED_MARK: &str = " (삭제됨)";

/// 답을 기다리는 사용자 질문. PC와 휴대폰 중 먼저 온 답을 쓴다.
#[derive(Default)]
pub struct QuestionBoard {
    open: Mutex<HashMap<String, (Value, oneshot::Sender<String>)>>,
}

impl QuestionBoard {
    /// 질문을 올린다. 돌려준 번호가 info의 "id"에도 들어간다.
    pub fn post(&self, mut info: Value) -> (String, oneshot::Receiver<String>) {
        let id = new_id();
        info["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.open.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), (info, tx));
        (id, rx)
    }

    pub fn list(&self) -> Vec<Value> {
        self.open.lock().unwrap_or_else(|p| p.into_inner()).values().map(|(i, _)| i.clone()).collect()
    }

    pub fn answer(&self, id: &str, text: String) -> Result<(), String> {
        let found = self.open.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
        match found {
            Some((_, tx)) => tx.send(text).map_err(|_| "이미 끝난 질문입니다".to_string()),
            None => Err("이미 답했거나 끝난 질문입니다".into()),
        }
    }

    pub fn close(&self, id: &str) -> bool {
        self.open.lock().unwrap_or_else(|p| p.into_inner()).remove(id).is_some()
    }
}

/// 질문의 답을 기다려야 하는 워커. 진행 중인 차례는 끊지 않고 다음 차례만 미룬다.
#[derive(Default)]
pub struct Holds {
    counts: Mutex<HashMap<String, usize>>,
    changed: Notify,
}

impl Holds {
    pub fn add(&self, keys: &[String]) {
        let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        for k in keys {
            *c.entry(k.clone()).or_insert(0) += 1;
        }
    }

    pub fn release(&self, keys: &[String]) {
        {
            let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
            for k in keys {
                if let Some(n) = c.get_mut(k) {
                    *n -= 1;
                    if *n == 0 {
                        c.remove(k);
                    }
                }
            }
        }
        self.changed.notify_waiters();
    }

    pub fn held(&self, key: &str) -> bool {
        self.counts.lock().unwrap_or_else(|p| p.into_inner()).contains_key(key)
    }

    async fn wait(&self, key: &str, stop: &CancellationToken) {
        loop {
            let n = self.changed.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if !self.held(key) || stop.is_cancelled() {
                return;
            }
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = &mut n => {}
            }
        }
    }
}

/// 작업 하나의 총괄. 작업이 끝날 때까지 AppState.leads에 있다.
pub struct TaskLead {
    pub conv: String,
    pub task: String,
    pub mid: String,
    pub request: String,
    pub engine: EngineId,
    pub model: Option<String>,
    pub cwd: PathBuf,
    pub jev_key: Option<String>,
    pub cancel: CancellationToken,
    pub session: Mutex<Option<String>>,
    pub policy: Mutex<Policy>,
    pub workers: Mutex<Vec<WorkerInfo>>,
    pub decisions: Mutex<Vec<Value>>,
    pub group_log: Mutex<Vec<Value>>,
    pub holds: Holds,
    /// 앞선 판단으로 생긴 규칙이 다음 요청에 바로 적용되도록 검토를 한 번에 하나씩 한다.
    review: tokio::sync::Mutex<()>,
    calls: tokio::sync::Mutex<()>,
    /// 통합 단계에 들어가면 사용자 지시를 통합 중인 총괄에게 바로 보낸다.
    pub integrating: std::sync::atomic::AtomicBool,
    /// 파일 작업 시스템 지시(총괄이 직접 고칠 때 쓴다)
    pub work_system: Mutex<String>,
    /// 커밋할 저장소. None이면 커밋하지 않는다(설정에서 껐거나 git 저장소가 아님).
    pub repo: Mutex<Option<gitops::Repo>>,
    /// 메인 폴더를 바꾸는 일(워커 반영·커밋 검토·직접 작업·통합)은 한 번에 하나씩 한다.
    pub merge: tokio::sync::Mutex<()>,
    /// 지금 단계의 워커 작업 공간
    pub stage: Mutex<Option<Arc<StageGate>>>,
    /// 총괄이 한 일(화면의 실시간 기록, 끝나면 대화 기록에 남긴다)
    pub timeline: Mutex<Vec<Value>>,
    /// 반영했지만 아직 커밋하지 못한 경로(저장소 최상위 기준). 다음 커밋에 함께 담는다.
    pending: Mutex<BTreeSet<String>>,
    /// 워커가 일하는 단계 동안만 있다: 총괄이 새 워커를 보내는 곳
    pub spawner: Mutex<Option<tokio::sync::mpsc::UnboundedSender<SpawnReq>>>,
    /// 총괄이 지운 워커 key. 결과를 합치지 않고 화면에서도 뺀다.
    pub removed: Mutex<BTreeSet<String>>,
    /// 조직으로 일하는 단계 동안만 있다: 팀장끼리 주고받는 보고판
    pub org: Mutex<Option<Arc<OrgBoard>>>,
    /// 팀장 보고 기록(끝나면 대화 기록에 남긴다)
    pub org_log: Mutex<Vec<Value>>,
}

impl TaskLead {
    #[allow(clippy::too_many_arguments)]
    pub fn new(conv: &str, task: &str, mid: &str, request: &str, engine: EngineId, model: Option<String>, cwd: PathBuf, jev_key: Option<String>, cancel: CancellationToken, session: Option<String>) -> Self {
        TaskLead {
            conv: conv.into(),
            task: task.into(),
            mid: mid.into(),
            request: request.into(),
            engine,
            model,
            cwd,
            jev_key,
            cancel,
            session: Mutex::new(session),
            policy: Mutex::new(Policy::default()),
            workers: Mutex::new(vec![]),
            decisions: Mutex::new(vec![]),
            group_log: Mutex::new(vec![]),
            holds: Holds::default(),
            review: tokio::sync::Mutex::new(()),
            calls: tokio::sync::Mutex::new(()),
            integrating: std::sync::atomic::AtomicBool::new(false),
            work_system: Mutex::new(String::new()),
            repo: Mutex::new(None),
            merge: tokio::sync::Mutex::new(()),
            stage: Mutex::new(None),
            timeline: Mutex::new(vec![]),
            pending: Mutex::new(BTreeSet::new()),
            spawner: Mutex::new(None),
            removed: Mutex::new(BTreeSet::new()),
            org: Mutex::new(None),
            org_log: Mutex::new(vec![]),
        }
    }

    fn worker_key(&self, number: usize) -> Option<String> {
        self.workers.lock().unwrap_or_else(|p| p.into_inner()).get(number.checked_sub(1)?).map(|w| w.key.clone())
    }

    /// 세션을 이어 쓸 수 없는 AI도 판단할 수 있게 호출마다 맥락을 다시 만든다.
    fn context(&self) -> String {
        let request: String = self.request.chars().take(REQUEST_CHARS).collect();
        let removed = self.removed.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let workers: String = self.workers.lock().unwrap_or_else(|p| p.into_inner()).iter().enumerate().map(|(i, w)| format!("{}. {}{}\n", i + 1, w.title, if removed.contains(&w.key) { REMOVED_MARK } else { "" })).collect();
        let decisions = self.decisions.lock().unwrap_or_else(|p| p.into_inner());
        let recent: String = decisions.iter().rev().take(20).rev().map(|d| format!("- {} → {}\n", d["command"].as_str().unwrap_or(""), d["verdict"].as_str().unwrap_or(""))).collect();
        format!("당신은 이 작업의 총괄입니다. 워커들에게 일을 나눠 맡겼습니다. 평소에는 판단만 하지만 [커밋 검토]와 [직접 작업]에서는 메인 작업 폴더의 파일을 직접 고칩니다.\n\n[사용자 요청]\n{request}\n\n[워커]\n{workers}\n[지금까지의 명령 판단]\n{recent}")
    }
}

#[derive(Clone)]
pub struct Ctx {
    pub app: AppHandle,
    pub st: Arc<AppState>,
    pub lead: Arc<TaskLead>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandJudgment {
    pub safe: f64,
    pub in_scope: f64,
    pub window: f64,
    pub manual: f64,
}

impl CommandJudgment {
    pub fn clearly_safe(&self) -> bool {
        self.safe >= JEV_SAFE && self.in_scope >= JEV_SCOPE && !self.opens_window() && !self.needs_person()
    }
    pub fn opens_window(&self) -> bool {
        self.window >= JEV_WINDOW
    }
    pub fn needs_person(&self) -> bool {
        self.manual >= JEV_MANUAL
    }
}

fn own_tools_note() -> &'static str {
    "일반 명령은 당신의 명령 도구로 직접 실행하세요. 대기열 요청은 창이 뜨는 실행(게임·앱·브라우저 등)만 받습니다."
}

fn results_prompt(results: &[String]) -> String {
    format!("[Elysium 결과]\n{}\n\n이 결과를 반영해 맡은 작업을 이어 가세요. 끝나면 바꾼 파일과 다른 AI가 알아야 할 것을 짧게 요약하세요.", results.join("\n\n"))
}

/// 요청을 받자마자 워커에게 돌려주는 말: 검토와 실행은 뒤에서 하고 워커는 기다리지 않는다.
fn started_note(command: &str) -> String {
    format!("[명령 접수] {command}\n검토를 거쳐 백그라운드에서 실행합니다. 결과를 기다리지 말고 남은 다른 작업을 계속하세요. 끝나면 결과를 다음 차례에 전달합니다. 결과가 꼭 있어야 하고 할 일이 없을 때만 차례를 끝내세요.")
}

/// 워커가 맡긴 백그라운드 명령 수. 바뀔 때마다 화면에 알린다(작업이 끊겨 버려져도 줄어든다).
struct BgCount {
    app: AppHandle,
    task: String,
    key: String,
    n: Arc<std::sync::atomic::AtomicUsize>,
}

impl BgCount {
    fn publish(&self) {
        emit(&self.app, "task:agent-bg", json!({ "taskId": self.task, "key": self.key, "count": self.n.load(std::sync::atomic::Ordering::Relaxed) }));
    }
    fn up(&self) -> BgSlot {
        self.n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.publish();
        BgSlot { app: self.app.clone(), task: self.task.clone(), key: self.key.clone(), n: self.n.clone() }
    }
}

/// 백그라운드 명령 하나가 끝나면(또는 버려지면) 수를 줄인다.
struct BgSlot {
    app: AppHandle,
    task: String,
    key: String,
    n: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for BgSlot {
    fn drop(&mut self) {
        let left = self.n.fetch_sub(1, std::sync::atomic::Ordering::Relaxed).saturating_sub(1);
        emit(&self.app, "task:agent-bg", json!({ "taskId": self.task, "key": self.key, "count": left }));
    }
}

fn merge_outcome(mut a: Outcome, b: &Outcome) -> Outcome {
    a.cost_usd = add_opt(a.cost_usd, b.cost_usd);
    a.input_tokens = add_opt(a.input_tokens, b.input_tokens);
    a.output_tokens = add_opt(a.output_tokens, b.output_tokens);
    a.latency_ms += b.latency_ms;
    a.activities.extend(b.activities.iter().cloned());
    if !b.text.trim().is_empty() {
        a.text = b.text.clone();
    }
    if b.session_id.is_some() {
        a.session_id = b.session_id.clone();
    }
    a.ok = b.ok;
    a.error = b.error.clone();
    a
}

/// 멈추면 None. 기다리던 일이 버려지면 그 안의 프로세스도 함께 끝난다.
async fn wait_or_stop<T>(f: impl Future<Output = T>, stop: &CancellationToken) -> Option<T> {
    tokio::select! {
        _ = stop.cancelled() => None,
        v = f => Some(v),
    }
}


fn jev_used(ctx: &Ctx, role: &'static str, ms: u64, cost: f64) {
    let _ = store::record_usage(&ctx.st.db, &store::Usage { engine: "jev", role, ok: true, latency_ms: ms, input_tokens: None, output_tokens: None, cost_usd: Some(cost) });
}

/// 명령이 위험한지, 범위 안인지, 게임 창이 뜨는지, 사람이 닫아야 끝나는지. JEV를 못 쓰면 None.
async fn judge_command(ctx: &Ctx, req: &WorkerRequest) -> Option<CommandJudgment> {
    let key = ctx.lead.jev_key.as_deref()?;
    let state = json!({ "command": req.command, "why": cut(&req.why, 500), "user_request": cut(&ctx.lead.request, REQUEST_CHARS), "declared_kind": req.kind });
    let mut q = serde_json::Map::new();
    q.insert("risk".into(), json!({ "type": "choice", "instructions": "`command` is a PowerShell command a coding worker wants to run in its own copy of the project. How risky is it?", "criteria": {
        "safe": "Builds, tests, reads or inspects. Stays inside the project folder and is easy to undo.",
        "risky": "Deletes or overwrites data, touches files outside the project, changes system state, publishes or sends data, or installs software."
    }}));
    q.insert("scope".into(), json!({ "type": "choice", "instructions": "Does `command` plausibly serve `user_request`?", "criteria": {
        "inside": "It helps build, test or check what was requested.",
        "outside": "It has no clear connection to what was requested."
    }}));
    q.insert("window".into(), json!({ "type": "choice", "instructions": "Does `command` open a game or application window on the screen when it runs?", "criteria": {
        "opens_window": "It launches a game client or other graphical window, for example a Gradle task that runs a client.",
        "headless": "It runs without opening a window, for example compile, unit test, dedicated server or game test server."
    }}));
    q.insert("ending".into(), json!({ "type": "choice", "instructions": "Does `command` finish on its own?", "criteria": {
        "ends_itself": "It finishes without help, including automated tests that open and close a window.",
        "needs_person": "It keeps running until a person closes it, for example launching a game client for manual play such as runClient."
    }}));
    let r = jev::ask(&ctx.st.http, key, "jev-latest", Value::Object(state.as_object()?.clone()), q, 1, Duration::from_secs(12)).await.ok()?;
    jev_used(ctx, "command", r.latency_ms, r.response.usage.input_tokens as f64 * crate::pricing::JEV_INPUT / 1e6);
    let a = &r.response.answers;
    Some(CommandJudgment {
        safe: jev::prob(a, "risk", "safe")?,
        in_scope: jev::prob(a, "scope", "inside")?,
        window: jev::prob(a, "window", "opens_window")?,
        manual: jev::prob(a, "ending", "needs_person")?,
    })
}

/// 막히거나 계획에서 벗어났을 확률. JEV를 못 쓰면 None.
async fn judge_blocked(ctx: &Ctx, role: &'static str, part: &str, reply: &str) -> Option<f64> {
    let key = ctx.lead.jev_key.as_deref()?;
    let state = json!({ "user_request": cut(&ctx.lead.request, REQUEST_CHARS), "assigned_part": cut(part, 500), "latest_reply": cut(reply, 4000) });
    let mut q = serde_json::Map::new();
    q.insert("status".into(), json!({ "type": "choice", "instructions": "A worker just wrote `latest_reply` about `assigned_part`. Does its lead need to step in?", "criteria": {
        "on_track": "Work is finished or moving as planned.",
        "blocked": "It is stuck, skipped part of the task, changed the plan, reports a conflict with other work, or needs a decision it cannot make."
    }}));
    let r = jev::ask(&ctx.st.http, key, "jev-latest", state, q, 1, Duration::from_secs(12)).await.ok()?;
    jev_used(ctx, role, r.latency_ms, r.response.usage.input_tokens as f64 * crate::pricing::JEV_INPUT / 1e6);
    jev::prob(&r.response.answers, "status", "blocked")
}

/// 총괄을 한 번 부른다. 호출은 작업마다 한 번에 하나다. 이어 쓰기에 실패하면 새 세션으로 한 번 더 부른다.
async fn lead_call(ctx: &Ctx, prompt: String, stop: &CancellationToken) -> Option<String> {
    let lead = &ctx.lead;
    let _turn = wait_or_stop(lead.calls.lock(), stop).await?;
    let resume = if lead.engine.resumable() { lead.session.lock().unwrap_or_else(|p| p.into_inner()).clone() } else { None };
    let spec = RunSpec { engine: lead.engine, prompt, system: String::new(), cwd: lead.cwd.clone(), work: false, resume, plugin_dirs: vec![], model: lead.model.clone(), effort: Some("medium".into()), max_tokens: Some(2000), extra_dirs: vec![], steer: None, attachments: vec![] };
    let run = |spec: RunSpec| orchestrator::run_engine_as(&ctx.app, &ctx.st, &lead.conv, &lead.task, &lead.mid, "lead", spec, LEAD_TIMEOUT, stop, false, "lead");
    let mut o = run(spec.clone()).await;
    if !o.ok && spec.resume.is_some() && !stop.is_cancelled() {
        o = run(RunSpec { resume: None, ..spec }).await;
    }
    if let Some(s) = &o.session_id {
        *lead.session.lock().unwrap_or_else(|p| p.into_inner()) = Some(s.clone());
    }
    o.ok.then_some(o.text)
}

/// 총괄이 한 일을 화면에 알린다. kind: review·consult·merge·conflict·commit·hold·work·test·warn·info
pub fn note(ctx: &Ctx, kind: &str, text: impl Into<String>) {
    let entry = json!({ "taskId": ctx.lead.task, "kind": kind, "text": text.into(), "at": crate::db::now() });
    {
        let mut t = ctx.lead.timeline.lock().unwrap_or_else(|p| p.into_inner());
        t.push(entry.clone());
        let over = t.len().saturating_sub(TIMELINE_KEEP);
        t.drain(..over);
    }
    emit(&ctx.app, "lead:activity", entry);
}

/// 총괄이 메인 폴더에서 파일을 고치는 한 번. 판단 호출과 따로 돌아 명령 검토를 막지 않는다(메인 폴더 잠금은 부르는 쪽이 잡는다).
async fn lead_work_run(ctx: &Ctx, prompt: String, stop: &CancellationToken) -> Outcome {
    let lead = &ctx.lead;
    let commits = lead.repo.lock().unwrap_or_else(|p| p.into_inner()).is_some();
    let system = format!("{}{}{}", lead.work_system.lock().unwrap_or_else(|p| p.into_inner()), lead::LEAD_WINDOW_SYSTEM, if commits { lead::COMMIT_SYSTEM } else { "" });
    let spec = RunSpec { engine: lead.engine, prompt, system, cwd: lead.cwd.clone(), work: true, resume: None, plugin_dirs: vec![], model: lead.model.clone(), effort: Some("medium".into()), max_tokens: None, extra_dirs: vec![], steer: None, attachments: vec![] };
    orchestrator::run_engine_as(&ctx.app, &ctx.st, &lead.conv, &lead.task, &lead.mid, "lead", spec, LEAD_WORK_TIMEOUT, stop, false, "lead-work").await
}

/// 메인 폴더의 지금 상태(커밋 전 파일과 해시)
async fn dirty_now(repo: &gitops::Repo) -> BTreeMap<String, Option<[u8; 32]>> {
    let r = repo.clone();
    tokio::task::spawn_blocking(move || r.dirty()).await.unwrap_or_default()
}

/// before 이후 바뀐 파일과 앞서 커밋하지 못한 파일을 커밋 블록대로 커밋한다. 블록이 없으면 기본 메시지로 커밋한다.
async fn commit_changes(ctx: &Ctx, repo: &gitops::Repo, before: &BTreeMap<String, Option<[u8; 32]>>, plan: Option<lead::CommitPlan>, fallback: &str) {
    let lead = &ctx.lead;
    let after = dirty_now(repo).await;
    let mut paths: BTreeSet<String> = gitops::changed_between(before, &after).into_iter().collect();
    paths.extend(lead.pending.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned());
    if paths.is_empty() {
        return;
    }
    let plan = plan.unwrap_or(lead::CommitPlan { commit: true, message: String::new(), note: String::new() });
    if !plan.commit {
        lead.pending.lock().unwrap_or_else(|p| p.into_inner()).extend(paths);
        note(ctx, "hold", if plan.note.trim().is_empty() { "커밋 보류".to_string() } else { format!("커밋 보류: {}", plan.note) });
        return;
    }
    let message = if plan.message.is_empty() { fallback.to_string() } else { plan.message.clone() };
    let (r, list, msg) = (repo.clone(), paths.iter().cloned().collect::<Vec<_>>(), message.clone());
    match tokio::task::spawn_blocking(move || r.commit(&list, &msg)).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(Some(sha)) => {
            lead.pending.lock().unwrap_or_else(|p| p.into_inner()).clear();
            let files: Vec<String> = paths.into_iter().collect();
            note(ctx, "commit", format!("{sha} {message}"));
            audit::record(&ctx.st.db, "lead", "lead.commit", "ok", json!({ "task": lead.task, "sha": sha, "message": message, "files": files }));
            emit(&ctx.app, "lead:commit", json!({ "taskId": lead.task, "sha": sha, "message": message, "files": files, "note": plan.note }));
        }
        Ok(None) => lead.pending.lock().unwrap_or_else(|p| p.into_inner()).clear(),
        Err(e) => {
            lead.pending.lock().unwrap_or_else(|p| p.into_inner()).extend(paths);
            note(ctx, "warn", format!("커밋 실패: {}", cut(&e, 300)));
        }
    }
}

/// 총괄이 메인 폴더에서 직접 일한다(사용자 지시나 총괄 자신의 판단). 끝나면 바뀐 파일을 커밋한다.
pub async fn lead_work(ctx: &Ctx, work: String, stop: &CancellationToken) {
    let lead = &ctx.lead;
    note(ctx, "work", format!("직접 작업: {}", cut(&work, 160)));
    let Some(_main) = wait_or_stop(lead.merge.lock(), stop).await else { return };
    let repo = lead.repo.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let before = match &repo {
        Some(r) => Some(dirty_now(r).await),
        None => None,
    };
    let o = lead_work_run(ctx, lead::direct_work_prompt(&lead.context(), &work), stop).await;
    let (plan, clean) = lead::parse_commit(&o.text);
    if !clean.trim().is_empty() {
        emit(&ctx.app, "lead:said", json!({ "taskId": lead.task, "text": cut(clean.trim(), 1500) }));
    }
    if !o.ok {
        note(ctx, "warn", format!("직접 작업 실패: {}", o.error.as_deref().unwrap_or("-")));
    }
    if let (Some(repo), Some(before)) = (repo, before) {
        if !stop.is_cancelled() {
            commit_changes(ctx, &repo, &before, plan, &format!("chore: {}", cut(&work, 60))).await;
        }
    }
}

/// 통합 단계의 커밋: 통합 전에 메인 폴더를 잠그고 상태를 잡아 둔다. 커밋하지 않으면 None.
pub async fn integration_begin(ctx: &Ctx) -> Option<(tokio::sync::MutexGuard<'_, ()>, gitops::Repo, BTreeMap<String, Option<[u8; 32]>>)> {
    let repo = ctx.lead.repo.lock().unwrap_or_else(|p| p.into_inner()).clone()?;
    let guard = wait_or_stop(ctx.lead.merge.lock(), &ctx.lead.cancel).await?;
    let before = dirty_now(&repo).await;
    note(ctx, "info", "통합 시작");
    Some((guard, repo, before))
}

/// 통합이 끝난 뒤 바뀐 파일을 커밋하고 답에서 커밋 블록을 뺀다.
pub async fn integration_end(ctx: &Ctx, repo: &gitops::Repo, before: &BTreeMap<String, Option<[u8; 32]>>, text: &str, ok: bool) -> String {
    let (plan, clean) = lead::parse_commit(text);
    if ok && !ctx.lead.cancel.is_cancelled() {
        commit_changes(ctx, repo, before, plan, "chore: 병렬 작업 통합").await;
    }
    clean
}

/// 사용자에게 묻고 답을 기다린다. 관련 워커는 다음 차례를 미룬다. 작업을 멈추면 None.
pub async fn ask_user(ctx: &Ctx, asked: &Asked, requester: Option<&str>, stop: &CancellationToken) -> Option<String> {
    let lead = &ctx.lead;
    let mut keys: Vec<String> = asked.workers.iter().filter_map(|n| lead.worker_key(*n)).collect();
    if let Some(r) = requester.filter(|r| !keys.iter().any(|k| k == r)) {
        keys.push(r.to_string());
    }
    lead.holds.add(&keys);
    let (id, rx) = ctx.st.questions.post(json!({ "taskId": lead.task, "conversationId": lead.conv, "question": asked.question, "options": asked.options, "workers": keys }));
    let info = ctx.st.questions.list().into_iter().find(|q| q["id"] == json!(id)).unwrap_or_default();
    emit(&ctx.app, "lead:question", info);
    let answer = wait_or_stop(rx, stop).await.and_then(Result::ok);
    ctx.st.questions.close(&id);
    emit(&ctx.app, "lead:question-closed", json!({ "id": id, "taskId": lead.task }));
    lead.holds.release(&keys);
    answer
}

/// PC·휴대폰 공용 답변. 먼저 온 답만 받는다.
pub fn answer_question(app: &AppHandle, st: &AppState, id: &str, text: String, actor: &str) -> Result<(), String> {
    st.questions.answer(id, text)?;
    audit::record(&st.db, actor, "lead.question.answered", "ok", json!({ "id": id }));
    emit(app, "lead:question-closed", json!({ "id": id }));
    Ok(())
}

/// 총괄에게 일어난 일을 알리고 그 판단대로 움직인다. 사용자 질문이 나오면 답을 받아 한 번 더 판단한다.
pub async fn consult_lead(ctx: &Ctx, mut event: String, requester: Option<&str>, stop: &CancellationToken) {
    let lead = &ctx.lead;
    note(ctx, "consult", cut(&event, 200));
    for _ in 0..3 {
        let Some(text) = lead_call(ctx, lead::actions_prompt(&lead.context(), &event), stop).await else { return };
        let Ok(a) = lead::parse_actions(&text) else {
            emit(&ctx.app, "lead:said", json!({ "taskId": lead.task, "text": text }));
            return;
        };
        if let Some(r) = a.reply.as_deref().filter(|r| !r.trim().is_empty()) {
            emit(&ctx.app, "lead:said", json!({ "taskId": lead.task, "text": r }));
        }
        for (n, m) in &a.messages {
            if let Some(k) = lead.worker_key(*n) {
                orchestrator::message_agent(&ctx.app, &ctx.st, &lead.task, &k, m);
            }
        }
        for n in &a.stop {
            if let Some(k) = lead.worker_key(*n) {
                orchestrator::cancel_agent(&ctx.st, &lead.task, &k);
            }
        }
        for n in &a.remove {
            remove_worker(ctx, *n);
        }
        for s in &a.spawn {
            spawn_worker(ctx, s);
        }
        for (n, m) in &a.messages {
            note(ctx, "consult", format!("워커 {n}에게 지시: {}", cut(m, 140)));
        }
        if let Some(w) = a.work.clone() {
            // 직접 작업은 오래 걸릴 수 있으니 따로 돌려 다른 판단을 막지 않는다.
            let (c, s) = (ctx.clone(), stop.clone());
            tauri::async_runtime::spawn(async move { lead_work(&c, w, &s).await });
        }
        let Some(asked) = a.ask_user else { return };
        let Some(answer) = ask_user(ctx, &asked, requester, stop).await else { return };
        event = format!("사용자에게 \"{}\"라고 물었고 답은 \"{answer}\"입니다. 이에 맞춰 움직이세요.", asked.question);
    }
}

/// 총괄이 워커를 지운다: 강제로 끝내고, 결과를 합치지 않으며, 화면에서 뺀다.
fn remove_worker(ctx: &Ctx, number: usize) {
    let lead = &ctx.lead;
    let Some(key) = lead.worker_key(number) else { return };
    if !lead.removed.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone()) {
        return;
    }
    let label = worker_label(lead, &key).unwrap_or_else(|| key.clone());
    orchestrator::cancel_agent(&ctx.st, &lead.task, &key);
    emit(&ctx.app, "task:agent-removed", json!({ "taskId": lead.task, "key": key }));
    audit::record(&ctx.st.db, "lead", "lead.worker.removed", "ok", json!({ "task": lead.task, "key": key }));
    note(ctx, "consult", format!("{w}을(를) 강제 종료하고 삭제했습니다.", w = label));
}

/// 총괄이 지금 단계에 워커를 더한다. 단계가 아니면(나누기 전·통합 중) 만들지 못한다고 남긴다.
fn spawn_worker(ctx: &Ctx, s: &lead::Spawn) {
    let lead = &ctx.lead;
    let engine = s.engine.as_deref().and_then(|n| crate::engines::ALL.iter().copied().find(|e| e.as_str().eq_ignore_ascii_case(n.trim())));
    let req = SpawnReq { title: cut(s.title.trim(), 60), task: s.task.trim().to_string(), engine };
    let tx = lead.spawner.lock().unwrap_or_else(|p| p.into_inner()).clone();
    match tx {
        Some(tx) if tx.send(req).is_ok() => {}
        _ => spawn_refused(ctx, &s.title),
    }
}

/// 새 워커를 만들지 못했다고 총괄 기록에 남긴다.
pub fn spawn_refused(ctx: &Ctx, title: &str) {
    note(ctx, "warn", format!("현재 워커가 실행 중이 아니므로 {t} 워커를 생성할 수 없습니다.", t = cut(title, 60)));
}

/// 사용자가 총괄에게 보낸 지시. 통합 중이면 통합하는 총괄에게 바로 전한다.
pub fn user_message(ctx: Ctx, text: String, about: Option<String>, direct: bool) {
    let event = match about.as_deref().and_then(|k| worker_label(&ctx.lead, k)) {
        Some(w) => format!("사용자가 {w}의 문제를 알렸습니다: {text}"),
        None => format!("사용자의 전체 지시: {text}"),
    };
    emit(&ctx.app, "lead:user", json!({ "taskId": ctx.lead.task, "text": text, "about": about }));
    if direct && !ctx.lead.integrating.load(std::sync::atomic::Ordering::Relaxed) {
        let work = match about.as_deref().and_then(|k| worker_label(&ctx.lead, k)) {
            Some(w) => format!("{w}와 관련된 사용자 지시: {text}"),
            None => text,
        };
        tauri::async_runtime::spawn(async move {
            let stop = ctx.lead.cancel.clone();
            lead_work(&ctx, work, &stop).await
        });
        return;
    }
    if ctx.lead.integrating.load(std::sync::atomic::Ordering::Relaxed) {
        orchestrator::message_task(&ctx.app, &ctx.st, &ctx.lead.task, &event);
        return;
    }
    tauri::async_runtime::spawn(async move {
        let stop = ctx.lead.cancel.clone();
        consult_lead(&ctx, event, None, &stop).await
    });
}

fn worker_label(lead: &TaskLead, key: &str) -> Option<String> {
    let workers = lead.workers.lock().unwrap_or_else(|p| p.into_inner());
    workers.iter().position(|w| w.key == key).map(|i| format!("워커 {} '{}'", i + 1, workers[i].title))
}

struct Ruling {
    run: bool,
    window: bool,
    by: &'static str,
    verdict: Verdict,
    pattern: Option<String>,
    reason: String,
}

fn publish_policy(ctx: &Ctx) {
    let p = ctx.lead.policy.lock().unwrap_or_else(|p| p.into_inner()).clone();
    emit(&ctx.app, "lead:policy", json!({ "taskId": ctx.lead.task, "policy": p }));
}

/// 사용자가 자동 허용을 끈다. 다음 요청부터 다시 검토한다.
pub fn revoke(ctx_app: &AppHandle, st: &AppState, lead: &TaskLead, actor: &str) {
    *lead.policy.lock().unwrap_or_else(|p| p.into_inner()) = Policy::default();
    audit::record(&st.db, actor, "lead.policy.revoked", "ok", json!({ "task": lead.task }));
    emit(ctx_app, "lead:policy", json!({ "taskId": lead.task, "policy": Policy::default() }));
}

fn record_ruling(ctx: &Ctx, key: &str, req: &WorkerRequest, r: &Ruling) {
    let verdict = serde_json::to_value(r.verdict).unwrap_or_default();
    let d = json!({ "id": new_id(), "taskId": ctx.lead.task, "key": key, "command": req.command, "why": req.why, "verdict": verdict, "by": r.by, "pattern": r.pattern, "window": r.window, "reason": r.reason, "at": crate::db::now() });
    ctx.lead.decisions.lock().unwrap_or_else(|p| p.into_inner()).push(d.clone());
    audit::record(&ctx.st.db, "lead", "worker.command", verdict.as_str().unwrap_or(""), d.clone());
    emit(&ctx.app, "lead:decision", d);
}

/// 자동 허용 규칙 → JEV → 총괄 순서로 판단한다. 창 판정은 자동 허용에서도 돈다.
async fn decide(ctx: &Ctx, key: &str, req: &WorkerRequest, stop: &CancellationToken) -> Ruling {
    let lead = &ctx.lead;
    let judged = judge_command(ctx, req).await;
    // 규칙으로 분명한 창 띄우기는 JEV·총괄·자동 허용과 상관없이 언제나 창 테스트 대기열로 간다.
    let mut window = req.kind == Kind::Window || lead::opens_window(&req.command) || judged.is_some_and(|j| j.opens_window());
    let manual = judged.is_some_and(|j| j.needs_person());
    let auto = |window: bool| {
        let m = lead.policy.lock().unwrap_or_else(|p| p.into_inner()).matches(&req.command);
        match m {
            Some(Matched::All) => Some(Ruling { run: true, window, by: "all", verdict: Verdict::All, pattern: None, reason: String::new() }),
            Some(Matched::Rule(p, w)) => Some(Ruling { run: true, window: window || w, by: "rule", verdict: Verdict::Similar, pattern: Some(p), reason: String::new() }),
            None => None,
        }
    };
    let deny = |reason: String, window: bool| Ruling { run: false, window, by: "lead", verdict: Verdict::Deny, pattern: None, reason };
    if !manual {
        if let Some(r) = auto(window) {
            return r;
        }
    }
    emit(&ctx.app, "task:agent-wait", json!({ "taskId": lead.task, "key": key, "note": "review" }));
    let Some(mut guard) = wait_or_stop(lead.review.lock(), stop).await else { return deny("작업이 중지되었습니다".into(), window) };
    if !manual {
        if let Some(r) = auto(window) {
            return r;
        }
        if judged.is_some_and(|j| j.clearly_safe()) && !window {
            return Ruling { run: true, window, by: "jev", verdict: Verdict::Once, pattern: None, reason: String::new() };
        }
    }
    let opinion = judged.map(|j| format!("안전 {:.2}, 요청 범위 {:.2}, 창 열림 {:.2}, 사람이 닫아야 끝남 {:.2}", j.safe, j.in_scope, j.window, j.manual));
    let label = worker_label(lead, key).unwrap_or_else(|| key.to_string());
    note(ctx, "review", format!("{label}의 명령 검토: {}", cut(&req.command, 160)));
    let mut extra = String::new();
    for _ in 0..2 {
        let prompt = format!("{}{extra}", lead::review_prompt(&lead.context(), &label, req, opinion.as_deref()));
        let Some(text) = lead_call(ctx, prompt, stop).await else { return deny("총괄이 판단하지 못해 거부했습니다".into(), window) };
        let d = match lead::parse_decision(&text) {
            Ok(d) => d,
            Err(_) => return deny("총괄의 답을 해석하지 못해 거부했습니다".into(), window),
        };
        window |= d.window;
        match d.decision {
            Verdict::Deny => return deny(d.reason, window),
            Verdict::Once => return Ruling { run: true, window, by: "lead", verdict: Verdict::Once, pattern: None, reason: d.reason },
            Verdict::All => {
                lead.policy.lock().unwrap_or_else(|p| p.into_inner()).allow_all = true;
                publish_policy(ctx);
                return Ruling { run: true, window, by: "lead", verdict: Verdict::All, pattern: None, reason: d.reason };
            }
            Verdict::Similar => {
                let pattern = d.pattern.clone().unwrap_or_default();
                lead.policy.lock().unwrap_or_else(|p| p.into_inner()).add_similar(&pattern, d.window);
                publish_policy(ctx);
                return Ruling { run: true, window, by: "lead", verdict: Verdict::Similar, pattern: Some(pattern), reason: d.reason };
            }
            Verdict::AskUser => {
                // 답을 기다리는 동안 다른 워커의 검토는 계속되어야 한다.
                drop(guard);
                let asked = Asked { question: d.question.clone().unwrap_or_default(), options: d.options.clone(), workers: d.workers.clone() };
                emit(&ctx.app, "task:agent-wait", json!({ "taskId": lead.task, "key": key, "note": "question" }));
                let Some(answer) = ask_user(ctx, &asked, Some(key), stop).await else { return deny("작업이 중지되었습니다".into(), window) };
                guard = match wait_or_stop(lead.review.lock(), stop).await {
                    Some(g) => g,
                    None => return deny("작업이 중지되었습니다".into(), window),
                };
                extra = format!("\n\n[사용자에게 물은 말] {}\n[사용자의 답] {answer}\n이제 질문하지 말고 결정하세요.", asked.question);
            }
        }
    }
    deny("사용자 답을 받은 뒤에도 총괄이 결정하지 못해 거부했습니다".into(), window)
}

/// 요청이 command로 왔어도 창 테스트로 판정되면 창 테스트의 시간 제한을 쓴다.
fn timeout_for(req: &WorkerRequest, window: bool) -> u64 {
    lead::clamp_timeout(if window { Kind::Window } else { req.kind }, req.timeout_sec)
}

/// 워커 대신 뒤에서 돌리는 명령. 출력을 줄마다 모아 에이전트 탭에서 보고 멈출 수 있게 한다.
async fn exec_bg(ctx: &Ctx, key: &str, cwd: &Path, req: &WorkerRequest, stop: &CancellationToken) -> String {
    let own = stop.child_token();
    let bg = &ctx.st.bg;
    let id = bg.start_elysium(&ctx.app, &ctx.lead.task, key, &req.command, own.clone());
    let run = crate::api_agent::powershell_live(cwd, &req.command, timeout_for(req, false), |line| bg.push_line(&id, line));
    let (state, text) = match wait_or_stop(run, &own).await {
        Some(Ok(body)) => ("ok", format!("[명령 성공] {}
{body}", req.command)),
        Some(Err(body)) => ("fail", format!("[명령 실패] {}
{body}", req.command)),
        None => ("stopped", format!("[명령 중지] {}
실행 중에 중지되었습니다.", req.command)),
    };
    bg.finish(&ctx.app, &id, state);
    text
}

/// 창 테스트를 실행하며 출력을 줄마다 보관하고 짧은 간격으로 화면에 보낸다.
async fn exec_test_live(app: &AppHandle, queue: &crate::testqueue::TestQueue, id: u64, cwd: &Path, req: &WorkerRequest, stop: &CancellationToken) -> String {
    let secs = timeout_for(req, true);
    let fresh: Mutex<Vec<String>> = Mutex::new(vec![]);
    let on_line = |line: &str| {
        queue.push_log(id, line);
        fresh.lock().unwrap_or_else(|p| p.into_inner()).push(line.to_string());
    };
    let flush = || {
        let lines = std::mem::take(&mut *fresh.lock().unwrap_or_else(|p| p.into_inner()));
        if !lines.is_empty() {
            emit(app, "test:log", json!({ "id": id, "lines": lines }));
        }
    };
    let run = crate::api_agent::powershell_live(cwd, &req.command, secs, on_line);
    tokio::pin!(run);
    let mut tick = tokio::time::interval(LOG_FLUSH);
    let out = loop {
        tokio::select! {
            r = &mut run => break Some(r),
            _ = stop.cancelled() => break None,
            _ = tick.tick() => flush(),
        }
    };
    flush();
    match out {
        Some(Ok(body)) => format!("[명령 성공] {}\n{body}", req.command),
        Some(Err(body)) => format!("[명령 실패] {}\n{body}", req.command),
        None => format!("[명령 중지] {}\n실행 중에 중지되었습니다.", req.command),
    }
}

struct PendingTest {
    handle: tokio::task::JoinHandle<String>,
}

/// 끝난 백그라운드 일의 결과를 꺼내고 나머지는 남긴다.
async fn take_finished(pending: &mut Vec<PendingTest>) -> Vec<String> {
    let mut out = vec![];
    let mut left = vec![];
    for p in pending.drain(..) {
        if p.handle.is_finished() {
            out.push(p.handle.await.unwrap_or_else(|e| format!("[명령 오류] {e}")));
        } else {
            left.push(p);
        }
    }
    *pending = left;
    out
}

/// 대기열에 넣고 자기 차례에 실행한다. 사용자가 이 테스트만 끄면 워커는 멈추지 않고 그 결과를 받는다.
async fn spawn_window_test(ctx: &Ctx, key: &str, cwd: PathBuf, req: WorkerRequest, stop: &CancellationToken) -> (PendingTest, Option<usize>) {
    let q = ctx.st.tests.clone();
    let id = q.next_id();
    let item = TestItem { id, task: ctx.lead.task.clone(), key: key.to_string(), command: req.command.clone(), started_at: None };
    let own = stop.child_token();
    let parent = stop.clone();
    let queue = q.clone();
    let app = ctx.app.clone();
    note(ctx, "test", format!("창 테스트 등록: {}", cut(&req.command, 160)));
    let handle = tokio::spawn(async move {
        let stopped = |parent: &CancellationToken| if parent.is_cancelled() { "작업이 중지되었습니다" } else { "사용자가 이 테스트를 껐습니다" };
        let record = item.clone();
        let Some(guard) = queue.acquire(item, &own).await else {
            let why = stopped(&parent);
            queue.finish(&record, None, false, why);
            return format!("[창 테스트 취소] {}\n{why}", req.command);
        };
        let started = queue.snapshot().running.iter().find(|i| i.id == id).and_then(|i| i.started_at);
        let result = exec_test_live(&app, &queue, id, &cwd, &req, &own).await;
        drop(guard);
        let (ok, text) = if own.is_cancelled() { (false, format!("[창 테스트 취소] {}\n{}", req.command, stopped(&parent))) } else { (result.starts_with("[명령 성공]"), result) };
        let summary = text.lines().nth(1).unwrap_or("").to_string();
        queue.finish(&record, started, ok, if summary.is_empty() { &text } else { &summary });
        text
    });
    // 줄에 들어간 뒤의 순번을 알려 준다.
    for _ in 0..50 {
        let v = q.snapshot();
        if v.waiting.iter().chain(&v.running).any(|i| i.id == id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (PendingTest { handle }, q.position(id))
}

/// 명령이 막혀 총괄의 검토를 거쳐야 하는 엔진
pub fn needs_review(engine: EngineId) -> bool {
    engine == EngineId::Antigravity
}

/// 엔진에 맞는 명령·대기열 사용법
pub fn worker_note(engine: EngineId) -> &'static str {
    if needs_review(engine) {
        lead::REQUEST_SYSTEM
    } else if engine.resumable() {
        lead::QUEUE_SYSTEM
    } else {
        lead::NO_WINDOW_SYSTEM
    }
}

/// 한 단계의 워커들. 차례를 끝낸 워커도 같은 단계의 다른 워커가 일하는 동안은 메시지를 기다렸다가 같은 세션으로 이어 간다.
/// 모두가 끝내거나 쉬면 단계가 끝난다.
pub struct Crew {
    busy: tokio::sync::watch::Sender<usize>,
}

impl Crew {
    pub fn new(units: usize) -> Self {
        Crew { busy: tokio::sync::watch::channel(units).0 }
    }
    /// 워커(또는 그룹·이어 쓸 수 없는 워커) 하나가 일을 멈췄다.
    pub fn rest(&self) {
        self.busy.send_modify(|n| *n = n.saturating_sub(1));
    }
    fn wake(&self) {
        self.busy.send_modify(|n| *n += 1);
    }
    /// 단계 중에 워커가 새로 들어왔다.
    pub fn join(&self) {
        self.wake();
    }
    async fn all_resting(&self) {
        let mut rx = self.busy.subscribe();
        let _ = rx.wait_for(|n| *n == 0).await;
    }
}

/// 메시지 칸에 글이 있는가
fn has_mail(st: &AppState, slot: &str) -> bool {
    st.agent_inboxes.lock().unwrap_or_else(|p| p.into_inner()).get(slot).is_some_and(|m| !m.is_empty())
}

/// 끝낸 워커가 메시지를 기다린다. 메시지가 오면 true, 단계의 모두가 쉬거나 멈추면 false.
async fn park(st: &AppState, slot: &str, crew: &Crew, stop: &CancellationToken) -> bool {
    let all = crew.all_resting();
    tokio::pin!(all);
    loop {
        if has_mail(st, slot) {
            return true;
        }
        tokio::select! {
            _ = &mut all => return has_mail(st, slot),
            _ = stop.cancelled() => return false,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
}

/// 하위 작업을 맡은 워커 하나. 세션을 이어 쓸 수 없는 엔진은 요청 왕복 없이 한 번에 돈다.
pub async fn run_part_worker(ctx: &Ctx, spec: RunSpec, key: &str, title: &str, budget: Duration, check_drift: bool, crew: Option<&Crew>) -> Outcome {
    if spec.engine.resumable() {
        let reviewed = needs_review(spec.engine);
        run_worker(ctx, spec, WorkerOpts { key, title, role: "agent", stream: false, budget, reviewed, check_drift, crew }).await
    } else {
        let lead = &ctx.lead;
        let slot = orchestrator::agent_slot(&lead.task, key);
        let stop = ctx.st.agent_stops.lock().unwrap_or_else(|p| p.into_inner()).get(&slot).cloned().unwrap_or_else(|| lead.cancel.clone());
        let o = orchestrator::run_engine_as(&ctx.app, &ctx.st, &lead.conv, &lead.task, &lead.mid, "agent", spec, budget, &stop, false, key).await;
        if let Some(c) = crew {
            if o.ok && !stop.is_cancelled() {
                checkpoint(ctx, key, &o.text).await;
                org_report_solo(ctx, key, &o.text);
            }
            c.rest();
        }
        o
    }
}

/// 그룹 라운드 상한
const MAX_ROUNDS: u32 = 12;
/// 다른 그룹원에게 전하는 새 글 하나의 최대 글자 수
const GROUP_NOTE_CHARS: usize = 1200;
/// 세션을 이어 쓸 수 없는 그룹원에게 다시 주는, 이미 본 글 하나의 최대 글자 수
const GROUP_OLD_CHARS: usize = 240;
/// 그룹장이 나눈 역할 분담의 최대 글자 수
const GROUP_PLAN_CHARS: usize = 2000;
/// 팀장 보고 하나의 최대 글자 수
const ORG_POST_CHARS: usize = 500;
/// 한 번에 받는 팀장 보고 수(오래된 것은 버린다)
const ORG_TAKE: usize = 6;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct MemberState {
    alive: bool,
    done: bool,
    held: bool,
}

#[derive(Debug, PartialEq)]
struct RoundPlan {
    run: Vec<usize>,
    finished: bool,
}

/// 다음 라운드에 누가 일할지. 모두가 보류되면 멈추지 않게 모두 돌린다.
fn next_round(members: &[MemberState], round: u32, cap: u32) -> RoundPlan {
    let open: Vec<usize> = members.iter().enumerate().filter(|(_, m)| m.alive && !m.done).map(|(i, _)| i).collect();
    if round >= cap || open.is_empty() {
        return RoundPlan { run: vec![], finished: true };
    }
    let free: Vec<usize> = open.iter().copied().filter(|i| !members[*i].held).collect();
    RoundPlan { run: if free.is_empty() { open } else { free }, finished: false }
}

/// 첫 라운드는 그룹장 혼자 역할을 나눈다(그룹원마다 역할만 제안하는 차례를 아낀다). 그룹장이 없으면 모두가 제안한다.
fn first_round(members: &[MemberState], round: u32, cap: u32) -> RoundPlan {
    if round == 0 && members.first().is_some_and(|m| m.alive) {
        RoundPlan { run: vec![0], finished: false }
    } else {
        next_round(members, round, cap)
    }
}

/// 그룹 블록이 없거나 읽을 수 없으면 다시 쓰게 하고, 두 번 연속이면 끝낸 것으로 본다. (글, 이번에 놓쳤는지)
fn note_or_miss(note: Option<lead::GroupNote>, clean: String, misses: &mut u32) -> (lead::GroupNote, bool) {
    match note {
        Some(n) => {
            *misses = 0;
            (n, false)
        }
        None => {
            *misses += 1;
            // 블록 없이 쓴 본문은 길 수 있으니 끝부분만 전한다.
            (lead::GroupNote { say: solo_report(&clean), done: *misses >= 2, ..Default::default() }, true)
        }
    }
}

#[cfg(test)]
fn group_followup(others: &[(String, String)], held: bool) -> String {
    group_followup_with(others, held, false)
}

/// 멈춘 그룹원을 뺀다. 라운드 사이에 눌러도 다음 라운드부터 빠진다.
fn drop_stopped(state: &mut [MemberState], stops: &[CancellationToken]) {
    for (m, s) in state.iter_mut().zip(stops) {
        if s.is_cancelled() {
            m.alive = false;
        }
    }
}

/// 파일이 겹쳤다는 알림은 그 사람이 다음에 일할 때까지 남긴다.
fn update_notices(notices: &mut [bool], ran: &[usize], clashes: &[(usize, usize)]) {
    for r in ran {
        notices[*r] = false;
    }
    for (later, first) in clashes {
        notices[*later] = true;
        notices[*first] = true;
    }
}

/// 다음 차례 앞에 붙이는 말. 새 글이 없으면 머리말 없이 이어 가라고만 한다.
fn group_followup_with(others: &[(String, String)], held: bool, missed: bool) -> String {
    let mut s = String::new();
    if !others.is_empty() {
        s.push_str("[다른 그룹원의 글]\n");
        for (who, text) in others {
            s.push_str(&format!("[{who}]\n{}\n\n", cut(text, GROUP_NOTE_CHARS)));
        }
    }
    if held {
        s.push_str("당신이 맡겠다고 한 파일을 다른 그룹원도 맡았습니다. 이번 차례에는 파일을 고치지 말고 누가 맡을지 정리하세요.\n");
    } else {
        s.push_str("이어서 맡은 일을 하세요.\n");
    }
    if missed {
        s.push_str("지난 차례의 elysium-group 블록을 읽지 못했습니다. 올바른 JSON으로 다시 쓰세요. 문자열 안에서 줄바꿈과 따옴표를 쓰지 마세요.\n");
    }
    s.push_str("끝에 elysium-group 블록을 쓰세요.");
    s
}

/// 그룹 기록 한 줄
struct Said {
    who: usize,
    round: u32,
    text: String,
}

fn said_label(names: &[String], s: &Said) -> String {
    format!("{} · {}라운드", names[s.who], s.round + 1)
}

/// 세션을 이어 쓰는 그룹원에게: 아직 보지 못한 다른 그룹원의 글만(이미 본 글을 다시 보내지 않는다)
fn unseen(log: &[Said], seen: usize, me: usize, names: &[String]) -> Vec<(String, String)> {
    log[seen.min(log.len())..].iter().filter(|s| s.who != me && !s.text.trim().is_empty()).map(|s| (said_label(names, s), s.text.clone())).collect()
}

/// 세션을 이어 쓸 수 없는 그룹원에게: 지난 기록 전체(자기 글도, 앞 차례를 기억하지 못하므로)를 주되 이미 본 글은 짧게 줄인다.
/// 그룹장이 나눈 역할 분담(첫 글)은 줄이지 않는다.
fn recap(log: &[Said], seen: usize, names: &[String]) -> Vec<(String, String)> {
    log.iter().enumerate().filter(|(_, s)| !s.text.trim().is_empty()).map(|(k, s)| (said_label(names, s), if k < seen && k > 0 { cut(&s.text, GROUP_OLD_CHARS) } else { s.text.clone() })).collect()
}

/// 그룹장이 첫 차례에 받는 말
fn leader_plan_prompt(n: usize) -> String {
    format!("당신은 그룹원 1이자 그룹장입니다. 이번 차례에는 파일을 고치지 말고, 그룹원 {n}명이 맡을 일과 파일을 겹치지 않게 나누세요. elysium-group 블록의 say에 '그룹원 1: …; 그룹원 2: …' 꼴로 짧게 쓰고, claim에는 당신이 맡을 파일만 쓰세요.")
}

/// 그룹원의 첫 차례: 그룹장이 나눈 역할이 있으면 그대로 시작하고, 없으면 역할을 제안한다.
fn member_start_prompt(original: &str, name: &str, plan: Option<&str>) -> String {
    match plan {
        Some(p) => format!("{original}\n\n[그룹장의 역할 분담]\n{}\n\n당신은 {name}입니다. 분담대로 맡은 일을 하세요. 파일을 고치기 전에 claim에 적으세요.", cut(p, GROUP_PLAN_CHARS)),
        None => format!("{original}\n\n당신은 {name}입니다. 이번 라운드는 파일을 고치지 말고 그룹에서 맡을 역할과 파일을 elysium-group 블록으로 제안하세요."),
    }
}

/// 그룹장이 팀장 보고판에 올릴 글: 그룹장이 쓴 report, 없으면 이번 라운드 그룹원 글을 짧게 모은다.
fn team_report(leader_report: &str, said: &[(String, String)]) -> String {
    if !leader_report.trim().is_empty() {
        return cut(leader_report.trim(), ORG_POST_CHARS);
    }
    let all = said.iter().filter(|(_, t)| !t.trim().is_empty()).map(|(who, t)| format!("{who}: {}", cut(t.trim(), 160))).collect::<Vec<_>>().join(" / ");
    cut(&all, ORG_POST_CHARS)
}

/// 그룹이 끝난 뒤 통합 AI에게 넘길 요약: 그룹원마다 마지막 글과 맡은 파일(요약만 쓰게 하는 차례를 아낀다)
fn group_digest(names: &[String], last: &[String], claims: &[BTreeSet<String>]) -> String {
    names
        .iter()
        .enumerate()
        .filter(|(i, _)| !last[*i].trim().is_empty())
        .map(|(i, n)| {
            let files = if claims[i].is_empty() { String::new() } else { format!(" (파일: {})", claims[i].iter().cloned().collect::<Vec<_>>().join(", ")) };
            format!("- {n}{files}: {}", cut(last[i].trim(), 600))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 워커 여럿이 한 하위 작업을 대화하며 함께 한다. 첫 라운드는 그룹장이 역할을 나누고, 끝나면 그룹원의 마지막 글로 결과를 정리한다.
pub async fn run_group(ctx: &Ctx, index: usize, title: &str, members: Vec<(String, RunSpec)>, budget: Duration) -> Outcome {
    let mut specs: Vec<RunSpec> = members.iter().map(|(_, s)| s.clone()).collect();
    let keys: Vec<String> = members.iter().map(|(k, _)| k.clone()).collect();
    let n = keys.len();
    let names: Vec<String> = (0..n).map(|m| format!("그룹원 {}", m + 1)).collect();
    let mut state = vec![MemberState { alive: true, done: false, held: false }; n];
    // 라운드 사이에도 멈추거나 메시지를 받도록 그룹이 끝날 때까지 등록해 둔다.
    let slots: Vec<String> = keys.iter().map(|k| orchestrator::agent_slot(&ctx.lead.task, k)).collect();
    let registered: Vec<(CancellationToken, bool)> = slots.iter().map(|s| register_line(&ctx.st.agent_stops, &ctx.st.agent_inboxes, s, &ctx.lead.cancel)).collect();
    let stops: Vec<CancellationToken> = registered.iter().map(|(t, _)| t.clone()).collect();
    let _lines: Vec<WorkerLine> = slots.iter().zip(&registered).map(|(slot, (_, owned))| WorkerLine { st: &ctx.st, slot: slot.clone(), owned: *owned }).collect();
    let mut notices = vec![false; n];
    let mut misses = vec![0u32; n];
    let mut missed = vec![false; n];
    let mut merged = Outcome { ok: true, ..Default::default() };
    let originals: Vec<String> = specs.iter().map(|s| s.prompt.clone()).collect();
    let mut log: Vec<Said> = vec![];
    // 그룹원마다 지금까지 받은 기록의 길이
    let mut seen = vec![0usize; n];
    let mut started = vec![false; n];
    let mut last_say = vec![String::new(); n];
    let mut claims: Vec<BTreeSet<String>> = vec![BTreeSet::new(); n];
    let mut plan: Option<String> = None;
    let board = org_board(ctx);
    let part = board.as_ref().and_then(|b| b.part_of(&keys[0]));
    let mut round = 0;
    loop {
        drop_stopped(&mut state, &stops);
        let now = first_round(&state, round, MAX_ROUNDS);
        if now.finished || ctx.lead.cancel.is_cancelled() {
            break;
        }
        let leader = state.iter().position(|m| m.alive).unwrap_or(0);
        for &i in &now.run {
            specs[i].prompt = if round == 0 && now.run == [0] {
                format!("{}\n\n{}", originals[0], leader_plan_prompt(n))
            } else if !started[i] {
                member_start_prompt(&originals[i], &names[i], plan.as_deref())
            } else if specs[i].resume.is_some() {
                group_followup_with(&unseen(&log, seen[i], i, &names), notices[i], missed[i])
            } else {
                format!("{}\n\n{}", originals[i], group_followup_with(&recap(&log, seen[i], &names), notices[i], missed[i]))
            };
            // 조직: 그룹장은 다른 팀장의 새 보고를 함께 받는다.
            if let (true, Some(b), Some(p)) = (i == leader && started[i], &board, part) {
                let posts = b.take(p);
                if !posts.is_empty() {
                    specs[i].prompt = format!("{}\n{}", board_prompt(&posts), specs[i].prompt);
                }
            }
            started[i] = true;
            seen[i] = log.len();
        }
        let runs = now.run.iter().map(|&i| {
            let (spec, key, title) = (specs[i].clone(), keys[i].clone(), title.to_string());
            async move { (i, run_part_worker(ctx, spec, &key, &title, budget, false, None).await) }
        });
        let outs = futures_util::future::join_all(runs).await;
        let mut said: Vec<(usize, lead::GroupNote)> = vec![];
        for (i, o) in outs {
            let (note, clean) = lead::parse_group(&o.text);
            if let Some(s) = &o.session_id {
                specs[i].resume = Some(s.clone());
            }
            merged = merge_outcome(merged, &Outcome { text: String::new(), ok: true, error: None, ..o.clone() });
            state[i].alive = o.ok;
            let (note, miss) = note_or_miss(note, clean, &mut misses[i]);
            missed[i] = miss;
            state[i].done = note.done;
            if round == 0 && i == 0 && o.ok && !miss && !note.say.trim().is_empty() {
                plan = Some(note.say.clone());
            }
            if !note.say.trim().is_empty() {
                last_say[i] = note.say.clone();
            }
            claims[i].extend(note.claim.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()));
            let entry = json!({ "taskId": ctx.lead.task, "group": index, "round": round, "key": keys[i], "who": names[i], "text": note.say, "claim": note.claim, "done": note.done });
            ctx.lead.group_log.lock().unwrap_or_else(|p| p.into_inner()).push(entry.clone());
            emit(&ctx.app, "group:say", entry);
            log.push(Said { who: i, round, text: note.say.clone() });
            said.push((i, note));
        }
        // 조직: 그룹장이 이번 라운드에 일했으면 팀 상황을 다른 팀장에게 올린다.
        if let (Some(b), Some(p), Some((_, ln))) = (&board, part, said.iter().find(|(i, _)| *i == leader)) {
            let all: Vec<(String, String)> = said.iter().map(|(i, x)| (names[*i].clone(), x.say.clone())).collect();
            org_post(ctx, b, p, &team_report(&ln.report, &all));
        }
        let notes: Vec<(usize, &lead::GroupNote)> = said.iter().map(|(i, n)| (*i, n)).collect();
        let held = lead::claim_overlaps(&notes);
        update_notices(&mut notices, &now.run, &lead::claim_clashes(&notes));
        for (i, m) in state.iter_mut().enumerate() {
            m.held = held.contains(&i);
        }
        let joined: String = said.iter().map(|(i, n)| format!("{}: {}\n", names[*i], cut(&n.say, GROUP_NOTE_CHARS))).collect();
        // 그룹장 혼자 역할을 나눈 첫 라운드는 막힐 일이 없어 묻지 않는다.
        if !(round == 0 && now.run == [0]) && judge_blocked(ctx, "group", title, &joined).await.is_some_and(|p| p >= JEV_BLOCKED) {
            consult_lead(ctx, format!("그룹 '{title}'이 막힌 것으로 보입니다. 최근 대화:\n{joined}"), None, &ctx.lead.cancel).await;
        }
        round += 1;
    }
    if ctx.lead.cancel.is_cancelled() {
        merged.ok = false;
        merged.error = Some(orchestrator::AGENT_STOPPED.to_string());
        return merged;
    }
    drop_stopped(&mut state, &stops);
    let Some(writer) = state.iter().position(|m| m.alive) else {
        merged.ok = false;
        merged.error = Some("그룹원이 모두 실패했습니다".to_string());
        return merged;
    };
    // 그룹원이 남긴 마지막 글로 정리한다. 아무도 글을 남기지 않았을 때만 한 번 더 묻는다.
    let digest = group_digest(&names, &last_say, &claims);
    if !digest.is_empty() {
        merged.text = digest;
        return merged;
    }
    let mut spec = specs[writer].clone();
    spec.prompt = "그룹 작업이 끝났습니다. 그룹 전체가 바꾼 파일과 다른 AI가 알아야 할 것을 짧게 요약하세요. 이번에는 elysium-group 블록을 쓰지 마세요.".to_string();
    let o = run_part_worker(ctx, spec, &keys[writer], title, budget, false, None).await;
    let (_, clean) = lead::parse_group(&o.text);
    merge_outcome(merged, &Outcome { text: clean, ..o })
}

/// 팀장 보고판: 같은 부서의 팀장끼리 짧은 보고를 주고받는다. 보고는 받는 쪽이 어차피 일하는 다음 차례 앞에 붙고,
/// 보고 때문에 쉬는 워커를 깨우지 않는다(토큰을 아낀다).
pub struct OrgBoard {
    titles: Vec<String>,
    peers: Vec<Vec<usize>>,
    /// 팀장 실행 key → 하위 작업
    leaders: HashMap<String, usize>,
    posts: Mutex<Vec<(usize, String)>>,
    /// 하위 작업마다 지금까지 받은 보고의 길이
    seen: Mutex<Vec<usize>>,
}

impl OrgBoard {
    pub fn new(titles: Vec<String>, peers: Vec<Vec<usize>>, leaders: HashMap<String, usize>) -> Self {
        let n = titles.len();
        OrgBoard { titles, peers, leaders, posts: Mutex::new(vec![]), seen: Mutex::new(vec![0; n]) }
    }

    /// 보고를 주고받는 팀장이면 그 하위 작업
    pub fn part_of(&self, key: &str) -> Option<usize> {
        self.leaders.get(key).copied().filter(|i| self.peers.get(*i).is_some_and(|p| !p.is_empty()))
    }

    /// 보고를 올린다. 바로 앞 보고와 같으면 올리지 않는다. 받는 하위 작업들을 돌려준다.
    fn post(&self, from: usize, text: &str) -> Option<(Vec<usize>, String)> {
        let text = cut(text.trim(), ORG_POST_CHARS);
        let to = self.peers.get(from).filter(|p| !p.is_empty())?.clone();
        if text.is_empty() {
            return None;
        }
        let mut posts = self.posts.lock().unwrap_or_else(|p| p.into_inner());
        if posts.iter().rev().find(|(f, _)| *f == from).is_some_and(|(_, t)| *t == text) {
            return None;
        }
        posts.push((from, text.clone()));
        Some((to, text))
    }

    /// to가 아직 받지 않은 다른 팀장의 보고(최근 몇 개). 받은 것으로 표시한다.
    pub fn take(&self, to: usize) -> Vec<(String, String)> {
        let Some(peers) = self.peers.get(to) else { return vec![] };
        let posts = self.posts.lock().unwrap_or_else(|p| p.into_inner());
        let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
        let from = seen[to].min(posts.len());
        seen[to] = posts.len();
        let fresh: Vec<(String, String)> = posts[from..].iter().filter(|(f, _)| peers.contains(f)).map(|(f, t)| (self.titles[*f].clone(), t.clone())).collect();
        fresh[fresh.len().saturating_sub(ORG_TAKE)..].to_vec()
    }
}

pub fn board_prompt(posts: &[(String, String)]) -> String {
    let mut s = String::from("[다른 팀장의 보고] 필요한 것만 반영하세요. 답할 일이 있으면 다음 보고에 쓰세요.\n");
    for (who, t) in posts {
        s.push_str(&format!("- {who}: {t}\n"));
    }
    s
}

fn org_board(ctx: &Ctx) -> Option<Arc<OrgBoard>> {
    ctx.lead.org.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// 팀장 보고를 올리고 화면과 기록에 남긴다.
fn org_post(ctx: &Ctx, board: &OrgBoard, from: usize, text: &str) {
    if let Some((to, text)) = board.post(from, text) {
        let entry = json!({ "taskId": ctx.lead.task, "from": from, "title": board.titles[from], "to": to, "text": text, "at": crate::db::now() });
        ctx.lead.org_log.lock().unwrap_or_else(|p| p.into_inner()).push(entry.clone());
        emit(&ctx.app, "org:post", entry);
    }
}

/// 글의 끝부분(대개 요약). 줄 중간에서 자르지 않게 첫 줄바꿈 뒤부터 쓴다.
fn solo_report(text: &str) -> String {
    let t = text.trim();
    let n = t.chars().count();
    if n <= ORG_POST_CHARS {
        return t.to_string();
    }
    let tail: String = t.chars().skip(n - ORG_POST_CHARS).collect();
    match tail.find('\n') {
        Some(i) if i < tail.len() / 2 => tail[i + 1..].to_string(),
        _ => tail,
    }
}

/// 혼자 일하는 팀장(key)이 쉬기 전에 다른 팀장에게 보고한다.
fn org_report_solo(ctx: &Ctx, key: &str, text: &str) {
    if let Some(b) = org_board(ctx) {
        if let Some(p) = b.part_of(key) {
            org_post(ctx, &b, p, &solo_report(text));
        }
    }
}

/// 팀장(key)이 다음 차례 앞에 받을 다른 팀장의 새 보고
fn org_inbox(ctx: &Ctx, key: &str) -> Option<String> {
    let b = org_board(ctx)?;
    let posts = b.take(b.part_of(key)?);
    (!posts.is_empty()).then(|| board_prompt(&posts))
}

pub struct WorkerOpts<'a> {
    pub key: &'a str,
    pub title: &'a str,
    pub role: &'static str,
    pub stream: bool,
    pub budget: Duration,
    /// true면 명령 요청도 검토해 실행한다(명령이 막힌 워커). false면 창 테스트 요청만 받는다.
    pub reviewed: bool,
    /// 차례가 끝날 때 JEV에게 막혔는지 묻는다. 그룹은 라운드마다 따로 묻는다.
    pub check_drift: bool,
    /// 있으면 차례를 끝낸 뒤에도 단계가 끝날 때까지 메시지를 기다린다.
    pub crew: Option<&'a Crew>,
}

/// 이 작업의 등록을 지운다. 차례 사이에도 멈추거나 말을 걸 수 있게 worker 수명 동안 살아 있다.
struct WorkerLine<'a> {
    st: &'a AppState,
    slot: String,
    owned: bool,
}

/// 멈춤 토큰과 메시지 칸을 등록한다. 그룹이 먼저 잡아 두었으면 그것을 같이 쓴다. (토큰, 이 호출이 등록했는지)
fn register_line(stops: &Mutex<HashMap<String, CancellationToken>>, inboxes: &Mutex<HashMap<String, Vec<String>>>, slot: &str, parent: &CancellationToken) -> (CancellationToken, bool) {
    let mut s = stops.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = s.get(slot) {
        return (t.clone(), false);
    }
    let t = parent.child_token();
    s.insert(slot.to_string(), t.clone());
    inboxes.lock().unwrap_or_else(|p| p.into_inner()).insert(slot.to_string(), vec![]);
    (t, true)
}

/// 워커가 끝나면 그 워커가 맡긴 창 테스트도 끝내 대기열 자리를 돌려준다.
struct TestScope {
    token: CancellationToken,
}

impl TestScope {
    fn new(stop: &CancellationToken) -> Self {
        TestScope { token: stop.child_token() }
    }
}

impl Drop for TestScope {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl Drop for WorkerLine<'_> {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        self.st.agent_stops.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.slot);
        self.st.agent_inboxes.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.slot);
    }
}

/// 워커 하나를 끝까지 돌린다. 차례가 끝날 때마다 요청을 처리하고 결과를 담아 같은 대화를 잇는다.
pub async fn run_worker(ctx: &Ctx, mut spec: RunSpec, opts: WorkerOpts<'_>) -> Outcome {
    let (lead, st) = (&ctx.lead, &ctx.st);
    let slot = orchestrator::agent_slot(&lead.task, opts.key);
    let (stop, owned) = register_line(&st.agent_stops, &st.agent_inboxes, &slot, &lead.cancel);
    let _line = WorkerLine { st, slot: slot.clone(), owned };
    let tests = TestScope::new(&stop);
    let bg = BgCount { app: ctx.app.clone(), task: lead.task.clone(), key: opts.key.to_string(), n: Arc::new(std::sync::atomic::AtomicUsize::new(0)) };
    let mut merged = Outcome::default();
    let mut pending: Vec<PendingTest> = vec![];
    let mut resting = false;
    let wait_note = |note: &str| emit(&ctx.app, "task:agent-wait", json!({ "taskId": lead.task, "key": opts.key, "note": note }));
    for turn in 0..MAX_TURNS {
        if lead.holds.held(opts.key) {
            wait_note("question");
            lead.holds.wait(opts.key, &stop).await;
        }
        let mail = std::mem::take(st.agent_inboxes.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&slot).unwrap_or(&mut vec![]));
        if !mail.is_empty() {
            spec.prompt = format!("[총괄·사용자가 보낸 메시지]\n{}\n\n{}", mail.join("\n"), spec.prompt);
        }
        if let Some(board) = org_inbox(ctx, opts.key) {
            spec.prompt = format!("{board}\n{}", spec.prompt);
        }
        let o = orchestrator::run_engine_as(&ctx.app, st, &lead.conv, &lead.task, &lead.mid, opts.role, spec.clone(), opts.budget, &stop, opts.stream, opts.key).await;
        let parsed = lead::parse_requests(&o.text);
        merged = merge_outcome(merged, &Outcome { text: parsed.clean.clone(), ..o.clone() });
        if stop.is_cancelled() || !o.ok {
            if stop.is_cancelled() {
                merged.ok = false;
                merged.error.get_or_insert_with(|| orchestrator::AGENT_STOPPED.to_string());
                if !lead.cancel.is_cancelled() {
                    emit(&ctx.app, "task:agent", json!({ "taskId": lead.task, "conversationId": lead.conv, "key": opts.key, "engine": spec.engine, "label": spec.engine.label(), "role": opts.role, "state": "fail", "stopped": true, "error": orchestrator::AGENT_STOPPED }));
                }
            }
            break;
        }
        let mut results: Vec<String> = vec![];
        if let Some(e) = parsed.error {
            results.push(format!("[요청 형식 오류] {e}. 형식을 고쳐 다시 쓰세요."));
        }
        for req in parsed.requests {
            if !opts.reviewed && req.kind != Kind::Window && !lead::opens_window(&req.command) {
                results.push(format!("[요청 거부] {}\n{}", req.command, own_tools_note()));
                continue;
            }
            // 검토·실행은 뒤에서 한다: 워커는 바로 다음 일을 하고, 결과는 끝나는 대로 다음 차례에 받는다.
            let (c, key, cwd, scope, stop2, reviewed, slot) = (ctx.clone(), opts.key.to_string(), spec.cwd.clone(), tests.token.clone(), stop.clone(), opts.reviewed, bg.up());
            let command = req.command.clone();
            let handle = tokio::spawn(async move {
                let _slot = slot;
                let ruling = if reviewed { decide(&c, &key, &req, &stop2).await } else { Ruling { run: true, window: true, by: "self", verdict: Verdict::Once, pattern: None, reason: String::new() } };
                record_ruling(&c, &key, &req, &ruling);
                if stop2.is_cancelled() {
                    return format!("[명령 중지] {}\n작업이 중지되었습니다.", req.command);
                }
                if !ruling.run {
                    format!("[요청 거부] {}\n이유: {}", req.command, ruling.reason)
                } else if ruling.window {
                    let (p, _) = spawn_window_test(&c, &key, cwd, req.clone(), &scope).await;
                    p.handle.await.unwrap_or_else(|e| format!("[창 테스트 오류] {e}"))
                } else {
                    exec_bg(&c, &key, &cwd, &req, &scope).await
                }
            });
            pending.push(PendingTest { handle });
            results.push(started_note(&command));
        }
        // 이미 끝난 명령·테스트의 결과는 이번에 함께 전한다.
        results.extend(take_finished(&mut pending).await);
        // 할 일이 없어 차례를 끝낸 워커만 기다린다. 다 기다리지 않고 하나라도 끝나면 그 결과로 바로 이어 간다.
        if results.is_empty() && !pending.is_empty() {
            wait_note("command");
            let handles: Vec<_> = pending.drain(..).map(|p| p.handle).collect();
            let (first, _, rest) = futures_util::future::select_all(handles).await;
            results.push(first.unwrap_or_else(|e| format!("[명령 오류] {e}")));
            pending = rest.into_iter().map(|handle| PendingTest { handle }).collect();
            results.extend(take_finished(&mut pending).await);
        }
        if stop.is_cancelled() {
            merged.ok = false;
            merged.error.get_or_insert_with(|| orchestrator::AGENT_STOPPED.to_string());
            break;
        }
        if results.is_empty() {
            // 막혔다면 총괄이 메시지를 보내거나 사용자에게 묻고, 그러면 한 차례 더 돈다.
            if opts.check_drift && judge_blocked(ctx, "drift", opts.title, &parsed.clean).await.is_some_and(|p| p >= JEV_BLOCKED) {
                wait_note("review");
                consult_lead(ctx, format!("워커 '{}'가 막혔거나 계획에서 벗어난 것으로 보입니다. 워커의 마지막 글:\n{}", opts.title, cut(&parsed.clean, 4000)), Some(opts.key), &stop).await;
            }
            let mut mail = has_mail(st, &slot);
            // 끝낸 뒤에 총괄·사용자가 보낸 지시도 받도록, 같은 단계의 다른 워커가 일하는 동안 기다린다.
            if let (false, Some(crew)) = (mail, opts.crew) {
                // 쉬기 전에 지금까지 바꾼 파일을 메인 폴더에 반영하고 총괄이 검토해 커밋한다.
                wait_note("commit");
                checkpoint(ctx, opts.key, &parsed.clean).await;
                org_report_solo(ctx, opts.key, &parsed.clean);
                crew.rest();
                resting = true;
                wait_note("idle");
                mail = park(st, &slot, crew, &stop).await;
                if mail {
                    crew.wake();
                    resting = false;
                }
            }
            if !mail {
                break;
            }
            results.push("위 메시지를 반영해 이어 가세요.".to_string());
        }
        if turn + 1 == MAX_TURNS {
            merged.text.push_str("\n\n(차례 수 상한에 닿아 여기서 멈췄습니다.)");
            break;
        }
        spec.resume = merged.session_id.clone().or(spec.resume.take());
        spec.prompt = results_prompt(&results);
    }
    if let (false, Some(crew)) = (resting, opts.crew) {
        crew.rest();
    }
    merged
}

/// 한 단계의 워커 작업 공간. 워커가 쉴 때마다 바꾼 파일을 메인 폴더에 반영한다.
pub struct StageGate {
    root: PathBuf,
    scope: teamwork::Scope,
    keys: Mutex<HashMap<String, usize>>,
    units: Mutex<Vec<GateUnit>>,
    /// 총괄이 풀지 못해 통합 AI에게 넘길 충돌
    open: Mutex<Vec<Clash>>,
}

struct GateUnit {
    title: String,
    who: String,
    dir: PathBuf,
    /// 워커 작업 공간을 마지막으로 반영한 때의 상태(처음에는 단계의 기준)
    last: Arc<teamwork::Snapshot>,
    files: BTreeSet<String>,
}

/// 메인 폴더에 반영하지 못한 워커 판
#[derive(Debug, Clone)]
pub struct Clash {
    pub path: String,
    pub who: String,
    pub copy: PathBuf,
    pub deleted: bool,
}

/// 단계의 하위 작업 하나: (실행 key들, 이름, 맡은 AI, 작업 공간)
pub type UnitSpec = (Vec<String>, String, String, PathBuf);

impl StageGate {
    pub fn new(root: PathBuf, base: Arc<teamwork::Snapshot>, units: Vec<UnitSpec>) -> Self {
        let mut keys = HashMap::new();
        let units = units
            .into_iter()
            .enumerate()
            .map(|(i, (ks, title, who, dir))| {
                for k in ks {
                    keys.insert(k, i);
                }
                GateUnit { title, who, dir, last: base.clone(), files: BTreeSet::new() }
            })
            .collect();
        StageGate { root, scope: base.scope, keys: Mutex::new(keys), units: Mutex::new(units), open: Mutex::new(vec![]) }
    }

    /// 단계 중에 늘린 워커의 작업 공간을 더한다. base는 그 공간을 만든 때의 메인 폴더 상태다.
    pub fn add_unit(&self, key: &str, title: String, who: String, dir: PathBuf, base: Arc<teamwork::Snapshot>) {
        let mut units = self.units.lock().unwrap_or_else(|p| p.into_inner());
        units.push(GateUnit { title, who, dir, last: base, files: BTreeSet::new() });
        self.keys.lock().unwrap_or_else(|p| p.into_inner()).insert(key.to_string(), units.len() - 1);
    }

    /// 이 key의 하위 작업에서 메인 폴더에 반영한 파일
    pub fn files_of(&self, key: &str) -> Vec<String> {
        let i = self.keys.lock().unwrap_or_else(|p| p.into_inner()).get(key).copied();
        i.map(|i| self.files(i)).unwrap_or_default()
    }

    /// 이 하위 작업에서 메인 폴더에 반영한 파일
    pub fn files(&self, i: usize) -> Vec<String> {
        self.units.lock().unwrap_or_else(|p| p.into_inner()).get(i).map(|u| u.files.iter().cloned().collect()).unwrap_or_default()
    }

    /// 통합 AI에게 넘길 충돌(가져가면 비운다)
    pub fn take_open(&self) -> Vec<Clash> {
        std::mem::take(&mut *self.open.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// 워커(또는 그룹)가 바꾼 파일을 메인 폴더에 반영하고, 저장소면 총괄이 검토·보정해 커밋한다.
/// 메인 폴더 파일이 워커가 본 판과 다르면 덮지 않고 충돌로 넘긴다(총괄이 검토에서 합치거나 통합 AI가 합친다).
pub async fn checkpoint(ctx: &Ctx, key: &str, summary: &str) {
    let lead = &ctx.lead;
    let Some(gate) = lead.stage.lock().unwrap_or_else(|p| p.into_inner()).clone() else { return };
    let Some(i) = gate.keys.lock().unwrap_or_else(|p| p.into_inner()).get(key).copied() else { return };
    let Some(_main) = wait_or_stop(lead.merge.lock(), &lead.cancel).await else { return };
    let (dir, last, title, who) = {
        let u = &gate.units.lock().unwrap_or_else(|p| p.into_inner())[i];
        (u.dir.clone(), u.last.clone(), u.title.clone(), u.who.clone())
    };
    let (d, scope) = (dir.clone(), gate.scope);
    let Ok(Ok(snap)) = tokio::task::spawn_blocking(move || teamwork::snapshot_like(&d, scope)).await else { return };
    let changes = teamwork::diff(&last, &snap);
    if changes.paths().is_empty() {
        return;
    }
    let repo = lead.repo.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let before = match &repo {
        Some(r) => Some(dirty_now(r).await),
        None => None,
    };
    let (root, d, l, c) = (gate.root.clone(), dir.clone(), last.clone(), changes.clone());
    let applied = tokio::task::spawn_blocking(move || teamwork::apply_checked(&root, &d, &l, &c)).await.unwrap_or_default();
    {
        let mut units = gate.units.lock().unwrap_or_else(|p| p.into_inner());
        units[i].last = Arc::new(snap);
        units[i].files.extend(applied.written.iter().cloned());
    }
    if !applied.written.is_empty() {
        note(ctx, "merge", format!("'{title}' 변경 {}개를 메인 폴더에 반영", applied.written.len()));
    }
    for e in applied.errors.iter().take(3) {
        note(ctx, "warn", e.clone());
    }
    let clashes: Vec<Clash> = applied.clashes.iter().map(|(path, deleted)| Clash { path: path.clone(), who: format!("{title} · {who}"), copy: dir.clone(), deleted: *deleted }).collect();
    if !clashes.is_empty() {
        note(ctx, "conflict", format!("'{title}' 충돌 {}개: {}", clashes.len(), clashes.iter().map(|c| c.path.as_str()).take(5).collect::<Vec<_>>().join(", ")));
    }
    let (Some(repo), Some(before)) = (repo, before) else {
        gate.open.lock().unwrap_or_else(|p| p.into_inner()).extend(clashes);
        return;
    };
    let (r, b) = (repo.clone(), before.clone());
    let (changed, diff, log) = tokio::task::spawn_blocking(move || {
        let changed = gitops::changed_between(&b, &r.dirty());
        let diff = r.diff(&changed);
        (changed, diff, r.recent_log(5))
    })
    .await
    .unwrap_or_default();
    if changed.is_empty() && clashes.is_empty() {
        return;
    }
    note(ctx, "review", format!("'{title}' 변경을 메인에 넣어도 되는지 검토"));
    let conflicts: Vec<(String, String)> = clashes.iter().map(|c| (c.path.clone(), if c.deleted { "(워커가 지움)".to_string() } else { teamwork::version_path(&c.copy, &c.path) })).collect();
    let prompt = lead::commit_review_prompt(&lead.context(), &title, &cut(summary, 3000), &changed, &diff, &conflicts, &log);
    let o = lead_work_run(ctx, prompt, &lead.cancel).await;
    let (plan, clean) = lead::parse_commit(&o.text);
    if !o.ok || lead.cancel.is_cancelled() {
        note(ctx, "warn", format!("'{title}' 검토를 끝내지 못했습니다. 변경은 메인 폴더에 남겨 통합에서 다시 봅니다."));
        lead.pending.lock().unwrap_or_else(|p| p.into_inner()).extend(changed);
        gate.open.lock().unwrap_or_else(|p| p.into_inner()).extend(clashes);
        return;
    }
    if let Some(p) = plan.as_ref().filter(|p| !p.note.trim().is_empty()) {
        note(ctx, "info", format!("검토 메모: {}", cut(&p.note, 200)));
    } else if !clean.trim().is_empty() {
        note(ctx, "info", format!("검토 메모: {}", cut(clean.trim(), 200)));
    }
    commit_changes(ctx, &repo, &before, plan, &format!("chore: {title}")).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_prompt_lists_each_command() {
        let p = results_prompt(&["A 결과".to_string(), "B 결과".to_string()]);
        assert!(p.contains("A 결과") && p.contains("B 결과"));
        assert!(p.contains("이어"), "{p}");
    }

    #[test]
    fn started_note_tells_worker_not_to_wait() {
        let n = started_note("./gradlew build");
        assert!(n.contains("./gradlew build") && n.contains("백그라운드") && n.contains("기다리지 말고"), "{n}");
    }

    #[tokio::test]
    async fn finished_jobs_are_taken_and_running_ones_stay() {
        let done = tokio::spawn(async { "A".to_string() });
        let slow = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            "B".to_string()
        });
        while !done.is_finished() {
            tokio::task::yield_now().await;
        }
        let mut pending = vec![PendingTest { handle: done }, PendingTest { handle: slow }];
        assert_eq!(take_finished(&mut pending).await, vec!["A"]);
        assert_eq!(pending.len(), 1);
        pending[0].handle.abort();
    }

    #[test]
    fn merged_outcome_sums_usage_and_keeps_last_text() {
        let a = Outcome { text: "앞".into(), input_tokens: Some(2), cost_usd: Some(0.5), latency_ms: 10, activities: vec!["x".into()], ok: true, ..Default::default() };
        let b = Outcome { text: "뒤".into(), input_tokens: Some(3), output_tokens: Some(1), latency_ms: 5, activities: vec!["y".into()], ok: true, session_id: Some("s".into()), ..Default::default() };
        let m = merge_outcome(a, &b);
        assert_eq!((m.text.as_str(), m.input_tokens, m.output_tokens, m.cost_usd, m.latency_ms), ("뒤", Some(5), Some(1), Some(0.5), 15));
        assert_eq!(m.activities, vec!["x", "y"]);
        assert_eq!(m.session_id.as_deref(), Some("s"));
    }

    #[test]
    fn merge_keeps_earlier_text_when_the_last_turn_wrote_none() {
        let a = Outcome { text: "요약".into(), ok: true, ..Default::default() };
        assert_eq!(merge_outcome(a, &Outcome { ok: true, ..Default::default() }).text, "요약");
    }

    #[tokio::test]
    async fn crew_ends_only_when_everyone_rests_and_wakes_again() {
        let c = Crew::new(2);
        c.rest();
        assert!(tokio::time::timeout(Duration::from_millis(50), c.all_resting()).await.is_err(), "한 명이 아직 일한다");
        c.rest();
        tokio::time::timeout(Duration::from_secs(1), c.all_resting()).await.unwrap();
        c.wake();
        assert!(tokio::time::timeout(Duration::from_millis(50), c.all_resting()).await.is_err(), "메시지로 깨어나면 다시 일하는 중");
    }

    #[tokio::test]
    async fn stop_during_wait_ends_the_wait() {
        let stop = CancellationToken::new();
        let s2 = stop.clone();
        let w = tokio::spawn(async move { wait_or_stop(std::future::pending::<u8>(), &s2).await });
        stop.cancel();
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), w).await.unwrap().unwrap(), None);
        let stop = CancellationToken::new();
        assert_eq!(wait_or_stop(async { 7u8 }, &stop).await, Some(7));
    }

    #[test]
    fn first_answer_wins_and_second_is_an_error() {
        let board = QuestionBoard::default();
        let (id, rx) = board.post(json!({ "question": "q" }));
        assert_eq!(board.list().len(), 1);
        board.answer(&id, "a".into()).unwrap();
        assert!(board.answer(&id, "b".into()).is_err());
        assert_eq!(rx.blocking_recv().unwrap(), "a");
        assert!(board.list().is_empty());
    }

    #[test]
    fn closed_question_leaves_the_board() {
        let board = QuestionBoard::default();
        let (id, _rx) = board.post(json!({}));
        assert!(board.close(&id));
        assert!(!board.close(&id));
        assert!(board.answer(&id, "a".into()).is_err());
    }

    #[test]
    fn holds_count_per_worker() {
        let h = Holds::default();
        h.add(&["k".to_string()]);
        h.add(&["k".to_string()]);
        assert!(h.held("k"));
        h.release(&["k".to_string()]);
        assert!(h.held("k"));
        h.release(&["k".to_string()]);
        assert!(!h.held("k") && !h.held("other"));
    }

    fn m(alive: bool, done: bool, held: bool) -> MemberState {
        MemberState { alive, done, held }
    }

    #[test]
    fn missing_group_block_is_not_done_until_the_second_miss() {
        let mut misses = 0;
        let (n, missed) = note_or_miss(None, "그냥 쓴 글".into(), &mut misses);
        assert!(missed && !n.done, "첫 번째는 다시 쓰게 한다");
        assert_eq!(n.say, "그냥 쓴 글");
        let (n, missed) = note_or_miss(None, String::new(), &mut misses);
        assert!(missed && n.done, "두 번 연속이면 끝낸 것으로 본다");
        let ok = lead::GroupNote { say: "s".into(), ..Default::default() };
        let (n, missed) = note_or_miss(Some(ok), String::new(), &mut misses);
        assert!(!missed && !n.done);
        assert_eq!(misses, 0, "제대로 쓰면 다시 센다");
    }

    #[test]
    fn leader_plans_alone_in_the_first_round() {
        let all = [m(true, false, false), m(true, false, false), m(true, false, false)];
        assert_eq!(first_round(&all, 0, 12).run, vec![0], "첫 라운드는 그룹장만");
        assert_eq!(first_round(&all, 1, 12).run, vec![0, 1, 2]);
        let no_leader = [m(false, false, false), m(true, false, false)];
        assert_eq!(first_round(&no_leader, 0, 12).run, vec![1], "그룹장이 없으면 남은 그룹원이 제안한다");
        assert!(member_start_prompt("일", "그룹원 2", Some("그룹원 2: b.rs")).contains("b.rs"));
        assert!(member_start_prompt("일", "그룹원 2", None).contains("제안"));
    }

    #[test]
    fn members_get_only_what_they_have_not_seen() {
        let names: Vec<String> = (1..=3).map(|i| format!("그룹원 {i}")).collect();
        let log = vec![Said { who: 0, round: 0, text: "분담".into() }, Said { who: 1, round: 1, text: "a 끝".into() }, Said { who: 2, round: 1, text: String::new() }, Said { who: 0, round: 1, text: "b 시작".into() }];
        assert_eq!(unseen(&log, 1, 1, &names).iter().map(|x| x.1.as_str()).collect::<Vec<_>>(), vec!["b 시작"], "자기 글·빈 글·이미 본 글은 뺀다");
        let plan = Said { who: 0, round: 0, text: "분".repeat(GROUP_OLD_CHARS + 100) };
        let long = Said { who: 1, round: 1, text: "가".repeat(GROUP_OLD_CHARS + 100) };
        let r = recap(&[plan, long, Said { who: 0, round: 2, text: "새 글".into() }], 2, &names);
        assert_eq!(r[0].1.chars().count(), GROUP_OLD_CHARS + 100, "역할 분담은 줄이지 않는다");
        assert_eq!(r[1].1.chars().count(), GROUP_OLD_CHARS, "이미 본 글은 줄인다(자기 글도 준다)");
        assert_eq!(r[2].1, "새 글");
        assert_eq!(group_followup_with(&[], false, false), "이어서 맡은 일을 하세요.\n끝에 elysium-group 블록을 쓰세요.", "새 글이 없으면 머리말도 없다");
    }

    #[test]
    fn group_ends_with_a_digest_instead_of_another_turn() {
        let names: Vec<String> = vec!["그룹원 1".into(), "그룹원 2".into()];
        let claims = vec![BTreeSet::from(["a.rs".to_string()]), BTreeSet::new()];
        let d = group_digest(&names, &["a 완료".into(), String::new()], &claims);
        assert_eq!(d, "- 그룹원 1 (파일: a.rs): a 완료");
        assert!(group_digest(&names, &[String::new(), String::new()], &claims).is_empty());
    }

    #[test]
    fn org_board_delivers_each_report_once_to_peers_only() {
        let leaders = HashMap::from([("agent:0".to_string(), 0), ("agent:1".to_string(), 1), ("agent:2".to_string(), 2)]);
        let b = OrgBoard::new(vec!["화면".into(), "서버".into(), "문서".into()], vec![vec![1], vec![0], vec![]], leaders);
        assert_eq!(b.part_of("agent:2"), None, "받을 팀이 없으면 보고하지 않는다");
        assert!(b.post(0, "화면 API 정함").is_some());
        assert!(b.post(0, "화면 API 정함").is_none(), "같은 말은 다시 올리지 않는다");
        assert_eq!(b.take(1), vec![("화면".to_string(), "화면 API 정함".to_string())]);
        assert!(b.take(1).is_empty(), "한 번 받은 보고는 다시 주지 않는다");
        assert!(b.take(0).is_empty(), "자기 보고는 받지 않는다");
        assert!(board_prompt(&[("화면".into(), "x".into())]).contains("- 화면: x"));
        assert_eq!(team_report("", &[("그룹원 1".into(), "a".into()), ("그룹원 2".into(), String::new())]), "그룹원 1: a");
        assert_eq!(team_report("요약", &[]), "요약");
    }

    #[test]
    fn solo_report_keeps_the_end_of_long_text() {
        let text = format!("{}\n요약: 끝", "가".repeat(ORG_POST_CHARS * 2));
        let r = solo_report(&text);
        assert!(r.ends_with("요약: 끝") && r.chars().count() <= ORG_POST_CHARS);
        assert_eq!(solo_report(" 짧음 "), "짧음");
    }

    #[test]
    fn group_followup_asks_to_rewrite_a_broken_block() {
        let p = group_followup_with(&[], false, true);
        assert!(p.contains("읽지 못했습니다"), "{p}");
        assert!(!group_followup_with(&[], false, false).contains("읽지 못했습니다"));
    }

    fn lead_for_test() -> TaskLead {
        TaskLead::new("c", "t", "m", "r", EngineId::Claude, None, PathBuf::from("."), None, CancellationToken::new(), None)
    }

    #[tokio::test]
    async fn stop_ends_the_wait_for_a_review_held_by_another_worker() {
        let lead = lead_for_test();
        let held = lead.review.lock().await;
        let stop = lead.cancel.child_token();
        let waiting = wait_or_stop(lead.review.lock(), &stop);
        stop.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(2), waiting).await.unwrap().is_none());
        drop(held);
    }

    #[test]
    fn registered_line_is_reused_and_kept_for_the_group() {
        let (stops, inboxes) = (Mutex::new(HashMap::new()), Mutex::new(HashMap::new()));
        let parent = CancellationToken::new();
        let (group_stop, group_owned) = register_line(&stops, &inboxes, "t/k", &parent);
        assert!(group_owned);
        let (member_stop, owned) = register_line(&stops, &inboxes, "t/k", &parent);
        assert!(!owned, "그룹이 잡아 둔 등록은 워커가 지우지 않는다");
        member_stop.cancel();
        assert!(group_stop.is_cancelled(), "같은 토큰이어야 라운드 사이에도 멈춘다");
    }

    #[test]
    fn overlap_notice_survives_until_the_member_runs() {
        let mut notices = vec![false, false, false];
        update_notices(&mut notices, &[0, 1], &[(1, 0)]);
        assert_eq!(notices, vec![true, true, false], "둘 다 알린다");
        update_notices(&mut notices, &[0], &[]);
        assert_eq!(notices, vec![false, true, false], "쉬고 있던 사람의 알림은 남는다");
        update_notices(&mut notices, &[1], &[]);
        assert_eq!(notices, vec![false, false, false]);
    }

    #[test]
    fn stopped_member_leaves_the_group() {
        let stops = vec![CancellationToken::new(), CancellationToken::new()];
        stops[1].cancel();
        let mut state = vec![m(true, false, false), m(true, false, false)];
        drop_stopped(&mut state, &stops);
        assert_eq!(next_round(&state, 1, 12).run, vec![0]);
    }

    #[test]
    fn window_ruling_gets_the_window_timeout() {
        let req = WorkerRequest { command: "x".into(), why: String::new(), kind: Kind::Command, timeout_sec: None };
        assert_eq!(timeout_for(&req, false), 600);
        assert_eq!(timeout_for(&req, true), 1800, "총괄이 창 테스트로 본 명령");
    }

    #[test]
    fn test_scope_cancels_tests_when_the_worker_ends() {
        let stop = CancellationToken::new();
        let token = {
            let scope = TestScope::new(&stop);
            scope.token.clone()
        };
        assert!(token.is_cancelled());
        assert!(!stop.is_cancelled(), "그룹이 다시 쓰는 워커 토큰은 멀쩡하다");
    }

    #[test]
    fn held_member_skips_the_round() {
        assert_eq!(next_round(&[m(true, false, false), m(true, false, true)], 1, 12), RoundPlan { run: vec![0], finished: false });
    }

    #[test]
    fn everyone_held_still_runs_so_the_group_cannot_stall() {
        assert_eq!(next_round(&[m(true, false, true), m(true, false, true)], 1, 12).run, vec![0, 1]);
    }

    #[test]
    fn round_cap_stops_the_group() {
        assert!(next_round(&[m(true, false, false)], 12, 12).finished);
    }

    #[test]
    fn failed_member_does_not_end_the_group() {
        let p = next_round(&[m(false, false, false), m(true, false, false)], 1, 12);
        assert_eq!(p, RoundPlan { run: vec![1], finished: false });
        assert!(next_round(&[m(false, false, false), m(true, true, false)], 1, 12).finished, "남은 모두가 끝나면 끝");
    }

    #[test]
    fn group_followup_carries_others_and_cuts_long_notes() {
        let long = "가".repeat(GROUP_NOTE_CHARS + 50);
        let p = group_followup(&[("그룹원 2".into(), long)], false);
        assert!(p.contains("그룹원 2"));
        assert_eq!(p.matches('가').count(), GROUP_NOTE_CHARS);
        assert!(group_followup(&[], true).contains("정리"));
    }

    #[test]
    fn only_blocked_engines_need_review_and_each_engine_gets_its_note() {
        assert!(needs_review(EngineId::Antigravity) && !needs_review(EngineId::Claude));
        assert_eq!(worker_note(EngineId::Antigravity), lead::REQUEST_SYSTEM);
        assert_eq!(worker_note(EngineId::Claude), lead::QUEUE_SYSTEM);
        assert_eq!(worker_note(EngineId::Codex), lead::NO_WINDOW_SYSTEM);
    }

    #[test]
    fn command_requests_are_refused_for_ais_with_their_own_tools() {
        assert!(own_tools_note().contains("직접"));
    }

    #[test]
    fn safe_judgment_needs_every_signal() {
        let ok = CommandJudgment { safe: 0.9, in_scope: 0.8, window: 0.1, manual: 0.0 };
        assert!(ok.clearly_safe() && !ok.opens_window() && !ok.needs_person());
        assert!(!CommandJudgment { safe: 0.8, ..ok }.clearly_safe());
        assert!(!CommandJudgment { in_scope: 0.6, ..ok }.clearly_safe());
        assert!(!CommandJudgment { window: 0.6, ..ok }.clearly_safe(), "창이 뜨면 총괄이 본다");
        assert!(CommandJudgment { window: 0.5, ..ok }.opens_window());
        assert!(CommandJudgment { manual: 0.6, ..ok }.needs_person());
    }
}

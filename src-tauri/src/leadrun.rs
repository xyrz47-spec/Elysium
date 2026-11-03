//! 총괄과 워커를 잇는 실행 흐름: 요청 검토, 명령 실행, 게임 테스트 대기열, 사용자 질문, 워커 그룹.

use crate::audit;
use crate::db::new_id;
use crate::engines::{EngineId, Outcome, RunSpec};
use crate::jev;
use crate::lead::{self, Asked, Kind, Matched, Policy, Verdict, WorkerRequest};
use crate::orchestrator::{self, add_opt, cut, emit, AppState};
use crate::store;
use crate::testqueue::TestItem;
use serde_json::{json, Value};
use std::collections::HashMap;
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
const FOLLOW_UP_MIN: Duration = Duration::from_secs(120);
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
        }
    }

    fn worker_key(&self, number: usize) -> Option<String> {
        self.workers.lock().unwrap_or_else(|p| p.into_inner()).get(number.checked_sub(1)?).map(|w| w.key.clone())
    }

    /// 세션을 이어 쓸 수 없는 AI도 판단할 수 있게 호출마다 맥락을 다시 만든다.
    fn context(&self) -> String {
        let request: String = self.request.chars().take(REQUEST_CHARS).collect();
        let workers: String = self.workers.lock().unwrap_or_else(|p| p.into_inner()).iter().enumerate().map(|(i, w)| format!("{}. {}\n", i + 1, w.title)).collect();
        let decisions = self.decisions.lock().unwrap_or_else(|p| p.into_inner());
        let recent: String = decisions.iter().rev().take(20).rev().map(|d| format!("- {} → {}\n", d["command"].as_str().unwrap_or(""), d["verdict"].as_str().unwrap_or(""))).collect();
        format!("당신은 이 작업의 총괄입니다. 워커들에게 일을 나눠 맡겼고, 지금은 파일을 고치지 않고 판단만 합니다.\n\n[사용자 요청]\n{request}\n\n[워커]\n{workers}\n[지금까지의 명령 판단]\n{recent}")
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
    "일반 명령은 당신의 명령 도구로 직접 실행하세요. 대기열 요청은 게임 창이 뜨는 테스트만 받습니다."
}

fn results_prompt(results: &[String]) -> String {
    format!("[Elysium 결과]\n{}\n\n이 결과를 반영해 맡은 작업을 이어 가세요. 끝나면 바꾼 파일과 다른 AI가 알아야 할 것을 짧게 요약하세요.", results.join("\n\n"))
}

fn queued_note(command: &str, position: Option<usize>) -> String {
    match position {
        Some(n) => format!("[게임 테스트 대기] {command}\n대기열 {n}번째입니다. 남은 다른 작업이 있으면 그 일을 하고, 없으면 차례를 끝내세요. 결과는 나중에 전달합니다."),
        None => format!("[게임 테스트 시작] {command}\n지금 실행을 시작했습니다. 남은 다른 작업이 있으면 그 일을 하고, 없으면 차례를 끝내세요. 결과는 나중에 전달합니다."),
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
        let Some(asked) = a.ask_user else { return };
        let Some(answer) = ask_user(ctx, &asked, requester, stop).await else { return };
        event = format!("사용자에게 \"{}\"라고 물었고 답은 \"{answer}\"입니다. 이에 맞춰 움직이세요.", asked.question);
    }
}

/// 사용자가 총괄에게 보낸 지시. 통합 중이면 통합하는 총괄에게 바로 전한다.
pub fn user_message(ctx: Ctx, text: String, about: Option<String>) {
    let event = match about.as_deref().and_then(|k| worker_label(&ctx.lead, k)) {
        Some(w) => format!("사용자가 {w}의 문제를 알렸습니다: {text}"),
        None => format!("사용자의 전체 지시: {text}"),
    };
    emit(&ctx.app, "lead:user", json!({ "taskId": ctx.lead.task, "text": text, "about": about }));
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
    let mut window = req.kind == Kind::Window || judged.is_some_and(|j| j.opens_window());
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
    let opinion = judged.map(|j| format!("안전 {:.2}, 요청 범위 {:.2}, 게임 창 {:.2}, 사람이 닫아야 끝남 {:.2}", j.safe, j.in_scope, j.window, j.manual));
    let label = worker_label(lead, key).unwrap_or_else(|| key.to_string());
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

/// 요청이 command로 왔어도 게임 테스트로 판정되면 게임 테스트의 시간 제한을 쓴다.
fn timeout_for(req: &WorkerRequest, window: bool) -> u64 {
    lead::clamp_timeout(if window { Kind::Window } else { req.kind }, req.timeout_sec)
}

async fn exec_command(cwd: &Path, req: &WorkerRequest, window: bool, stop: &CancellationToken) -> String {
    let secs = timeout_for(req, window);
    match wait_or_stop(crate::api_agent::powershell(cwd, &req.command, secs), stop).await {
        Some(Ok(body)) => format!("[명령 성공] {}\n{body}", req.command),
        Some(Err(body)) => format!("[명령 실패] {}\n{body}", req.command),
        None => format!("[명령 중지] {}\n실행 중에 중지되었습니다.", req.command),
    }
}

struct PendingTest {
    handle: tokio::task::JoinHandle<String>,
}

/// 대기열에 넣고 자기 차례에 실행한다. 사용자가 이 테스트만 끄면 워커는 멈추지 않고 그 결과를 받는다.
async fn spawn_window_test(ctx: &Ctx, key: &str, cwd: PathBuf, req: WorkerRequest, stop: &CancellationToken) -> (PendingTest, Option<usize>) {
    let q = ctx.st.tests.clone();
    let id = q.next_id();
    let item = TestItem { id, task: ctx.lead.task.clone(), key: key.to_string(), command: req.command.clone() };
    let own = stop.child_token();
    let parent = stop.clone();
    let queue = q.clone();
    let handle = tokio::spawn(async move {
        let stopped = |parent: &CancellationToken| if parent.is_cancelled() { "작업이 중지되었습니다" } else { "사용자가 이 테스트를 껐습니다" };
        let Some(guard) = queue.acquire(item, &own).await else { return format!("[게임 테스트 취소] {}\n{}", req.command, stopped(&parent)) };
        let result = exec_command(&cwd, &req, true, &own).await;
        drop(guard);
        if own.is_cancelled() {
            format!("[게임 테스트 취소] {}\n{}", req.command, stopped(&parent))
        } else {
            result
        }
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
            c.rest();
        }
        o
    }
}

/// 그룹 라운드 상한
const MAX_ROUNDS: u32 = 12;
const GROUP_NOTE_CHARS: usize = 2500;

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

/// 그룹 블록이 없거나 읽을 수 없으면 다시 쓰게 하고, 두 번 연속이면 끝낸 것으로 본다. (글, 이번에 놓쳤는지)
fn note_or_miss(note: Option<lead::GroupNote>, clean: String, misses: &mut u32) -> (lead::GroupNote, bool) {
    match note {
        Some(n) => {
            *misses = 0;
            (n, false)
        }
        None => {
            *misses += 1;
            (lead::GroupNote { say: clean, claim: vec![], done: *misses >= 2 }, true)
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

fn group_followup_with(others: &[(String, String)], held: bool, missed: bool) -> String {
    let mut s = String::from("[다른 그룹원의 글]\n");
    for (who, text) in others {
        s.push_str(&format!("[{who}]\n{}\n\n", cut(text, GROUP_NOTE_CHARS)));
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

/// 워커 여럿이 한 하위 작업을 대화하며 함께 한다. 첫 라운드는 역할만 정하고, 끝나면 첫 번째 생존 그룹원이 결과를 정리한다.
pub async fn run_group(ctx: &Ctx, index: usize, title: &str, members: Vec<(String, RunSpec)>, budget: Duration) -> Outcome {
    let mut specs: Vec<RunSpec> = members.iter().map(|(_, s)| s.clone()).collect();
    let keys: Vec<String> = members.iter().map(|(k, _)| k.clone()).collect();
    let names: Vec<String> = (0..keys.len()).map(|m| format!("그룹원 {}", m + 1)).collect();
    let mut state = vec![MemberState { alive: true, done: false, held: false }; keys.len()];
    // 라운드 사이에도 멈추거나 메시지를 받도록 그룹이 끝날 때까지 등록해 둔다.
    let slots: Vec<String> = keys.iter().map(|k| orchestrator::agent_slot(&ctx.lead.task, k)).collect();
    let registered: Vec<(CancellationToken, bool)> = slots.iter().map(|s| register_line(&ctx.st.agent_stops, &ctx.st.agent_inboxes, s, &ctx.lead.cancel)).collect();
    let stops: Vec<CancellationToken> = registered.iter().map(|(t, _)| t.clone()).collect();
    let _lines: Vec<WorkerLine> = slots.iter().zip(&registered).map(|(slot, (_, owned))| WorkerLine { st: &ctx.st, slot: slot.clone(), owned: *owned }).collect();
    let mut notices = vec![false; keys.len()];
    let mut misses = vec![0u32; keys.len()];
    let mut missed = vec![false; keys.len()];
    let mut merged = Outcome { ok: true, ..Default::default() };
    let originals: Vec<String> = specs.iter().map(|s| s.prompt.clone()).collect();
    let mut log: Vec<(String, String)> = vec![];
    let mut round = 0;
    for (i, s) in specs.iter_mut().enumerate() {
        s.prompt = format!("{}\n\n당신은 {}입니다. 이번 라운드는 파일을 고치지 말고 그룹에서 맡을 역할과 파일을 elysium-group 블록으로 제안하세요.", originals[i], names[i]);
    }
    loop {
        drop_stopped(&mut state, &stops);
        let plan = next_round(&state, round, MAX_ROUNDS);
        if plan.finished || ctx.lead.cancel.is_cancelled() {
            break;
        }
        let runs = plan.run.iter().map(|&i| {
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
            let entry = json!({ "taskId": ctx.lead.task, "group": index, "round": round, "key": keys[i], "who": names[i], "text": note.say, "claim": note.claim, "done": note.done });
            ctx.lead.group_log.lock().unwrap_or_else(|p| p.into_inner()).push(entry.clone());
            emit(&ctx.app, "group:say", entry);
            log.push((format!("{} · {}라운드", names[i], round + 1), note.say.clone()));
            said.push((i, note));
        }
        let notes: Vec<(usize, &lead::GroupNote)> = said.iter().map(|(i, n)| (*i, n)).collect();
        let held = lead::claim_overlaps(&notes);
        update_notices(&mut notices, &plan.run, &lead::claim_clashes(&notes));
        for (i, m) in state.iter_mut().enumerate() {
            m.held = held.contains(&i);
        }
        let joined: String = said.iter().map(|(i, n)| format!("{}: {}\n", names[*i], cut(&n.say, GROUP_NOTE_CHARS))).collect();
        if judge_blocked(ctx, "group", title, &joined).await.is_some_and(|p| p >= JEV_BLOCKED) {
            consult_lead(ctx, format!("그룹 '{title}'이 막힌 것으로 보입니다. 최근 대화:\n{joined}"), None, &ctx.lead.cancel).await;
        }
        round += 1;
        for (i, s) in specs.iter_mut().enumerate() {
            let others: Vec<(String, String)> = log.iter().filter(|(who, _)| !who.starts_with(&names[i])).cloned().collect();
            let recent = &others[others.len().saturating_sub(keys.len() * 2)..];
            let body = group_followup_with(recent, notices[i], missed[i]);
            // 세션을 이어 쓸 수 없으면 원래 요청과 전체 기록을 다시 준다.
            s.prompt = if s.resume.is_some() { body } else { format!("{}\n\n{}", originals[i], group_followup_with(&others, notices[i], missed[i])) };
        }
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
    let mut spec = specs[writer].clone();
    spec.prompt = "그룹 작업이 끝났습니다. 그룹 전체가 바꾼 파일과 다른 AI가 알아야 할 것을 짧게 요약하세요. 이번에는 elysium-group 블록을 쓰지 마세요.".to_string();
    let o = run_part_worker(ctx, spec, &keys[writer], title, budget, false, None).await;
    let (_, clean) = lead::parse_group(&o.text);
    merge_outcome(merged, &Outcome { text: clean, ..o })
}

pub struct WorkerOpts<'a> {
    pub key: &'a str,
    pub title: &'a str,
    pub role: &'static str,
    pub stream: bool,
    pub budget: Duration,
    /// true면 명령 요청도 검토해 실행한다(명령이 막힌 워커). false면 게임 테스트 요청만 받는다.
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

/// 워커가 끝나면 그 워커가 맡긴 게임 테스트도 끝내 대기열 자리를 돌려준다.
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
    let mut merged = Outcome::default();
    let mut pending: Vec<PendingTest> = vec![];
    let mut used = Duration::ZERO;
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
        let left = opts.budget.saturating_sub(used).max(FOLLOW_UP_MIN);
        let o = orchestrator::run_engine_as(&ctx.app, st, &lead.conv, &lead.task, &lead.mid, opts.role, spec.clone(), left, &stop, opts.stream, opts.key).await;
        used += Duration::from_millis(o.latency_ms);
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
            if !opts.reviewed && req.kind != Kind::Window {
                results.push(format!("[요청 거부] {}\n{}", req.command, own_tools_note()));
                continue;
            }
            let ruling = if opts.reviewed { decide(ctx, opts.key, &req, &stop).await } else { Ruling { run: true, window: true, by: "self", verdict: Verdict::Once, pattern: None, reason: String::new() } };
            record_ruling(ctx, opts.key, &req, &ruling);
            if stop.is_cancelled() {
                break;
            }
            if !ruling.run {
                results.push(format!("[요청 거부] {}\n이유: {}", req.command, ruling.reason));
            } else if ruling.window {
                let (p, position) = spawn_window_test(ctx, opts.key, spec.cwd.clone(), req.clone(), &tests.token).await;
                pending.push(p);
                results.push(queued_note(&req.command, position));
            } else {
                wait_note("command");
                results.push(exec_command(&spec.cwd, &req, false, &stop).await);
            }
        }
        // 이미 끝난 테스트의 결과는 이번에 함께 전한다.
        let mut unfinished = vec![];
        for p in pending.drain(..) {
            if p.handle.is_finished() {
                results.push(p.handle.await.unwrap_or_else(|e| format!("[게임 테스트 오류] {e}")));
            } else {
                unfinished.push(p);
            }
        }
        pending = unfinished;
        // 할 일이 없어 차례를 끝낸 워커는 프로세스 없이 테스트 결과를 기다린다.
        if results.is_empty() && !pending.is_empty() {
            wait_note("test");
            for p in pending.drain(..) {
                results.push(p.handle.await.unwrap_or_else(|e| format!("[게임 테스트 오류] {e}")));
            }
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
    fn queued_result_tells_worker_to_continue_or_end_turn() {
        let n = queued_note("x", Some(2));
        assert!(n.contains("2번째") && n.contains("다른 작업") && n.contains("차례를 끝내"), "{n}");
        assert!(queued_note("x", None).contains("시작"));
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
        let ok = lead::GroupNote { say: "s".into(), claim: vec![], done: false };
        let (n, missed) = note_or_miss(Some(ok), String::new(), &mut misses);
        assert!(!missed && !n.done);
        assert_eq!(misses, 0, "제대로 쓰면 다시 센다");
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
        assert_eq!(timeout_for(&req, true), 1800, "총괄이 게임 테스트로 본 명령");
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
        assert!(!CommandJudgment { window: 0.6, ..ok }.clearly_safe(), "게임 창은 총괄이 본다");
        assert!(CommandJudgment { window: 0.5, ..ok }.opens_window());
        assert!(CommandJudgment { manual: 0.6, ..ok }.needs_person());
    }
}

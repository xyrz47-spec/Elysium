//! 요청 하나를 처리하는 흐름: 스킬 고르기 → 엔진 고르기 → (작업 모드면 승인) → 초안 → (필요하면) 다른 AI가 검토 → 저장.

use crate::attachments::{self, AttachedFile, Attachment};
use crate::audit;
use crate::db::{new_id, Db};
use crate::jev;
use crate::secrets::{self, SecretName};
use crate::engines::{self, EngineId, EngineInfo, Event, Outcome, RunSpec, Steer};
use crate::intent;
use crate::settings::{ReviewMode, Settings};
use crate::skills::{self, Catalog, Skill};
use crate::memory;
use crate::roadmap;
use crate::store;
use crate::teamwork;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub struct AppState {
    pub db: Db,
    pub http: reqwest::Client,
    pub data_dir: PathBuf,
    pub settings: RwLock<Settings>,
    pub engines: RwLock<Vec<EngineInfo>>,
    pub tasks: Mutex<HashMap<String, CancellationToken>>,
    /// 실행 중인 에이전트 하나하나("작업 번호/실행 key"). 에이전트 탭에서 하나만 멈추거나 말을 걸 수 있다.
    pub agents: Mutex<HashMap<String, AgentLine>>,
    /// 작업 중 메시지 칸(작업 번호 → 칸). 작업이 끝나면 지운다.
    pub steers: Mutex<HashMap<String, Steer>>,
    pub approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    /// 창을 띄우는 실행(창 테스트)의 대기열(앱 전체에 하나)
    pub tests: std::sync::Arc<crate::testqueue::TestQueue>,
    /// 병렬 작업의 총괄(작업 번호 → 총괄). 작업이 끝나면 지운다.
    pub leads: Mutex<HashMap<String, std::sync::Arc<crate::leadrun::TaskLead>>>,
    /// 차례 사이에도 워커를 멈출 수 있게 둔다("작업 번호/실행 key" → 토큰).
    pub agent_stops: Mutex<HashMap<String, CancellationToken>>,
    /// 차례 사이에 온 메시지. 다음 차례에 전한다.
    pub agent_inboxes: Mutex<HashMap<String, Vec<String>>>,
    /// 사용자의 답을 기다리는 총괄 질문
    pub questions: crate::leadrun::QuestionBoard,
    /// 에이전트가 뒤에서 돌리는 명령(에이전트 탭에서 출력을 본다)
    pub bg: crate::bgjobs::Jobs,
    /// How each agent of a running task was started and its CLI session ("task/key" → run). Side questions fork it.
    /// Kept between turns (a resting worker can still be asked) and dropped when the task ends.
    pub agent_runs: Mutex<HashMap<String, AgentRun>>,
}

/// One agent's run as it was started, plus the session it is writing to now.
#[derive(Debug, Clone)]
pub struct AgentRun {
    pub spec: RunSpec,
    pub session: Option<String>,
}

/// 실행 중인 에이전트로 가는 선.
pub struct AgentLine {
    /// 이 에이전트 전체를 멈춘다(작업 토큰의 자식)
    stop: CancellationToken,
    /// 지금 차례만 끊는다(stop의 자식). 말을 걸면 끊고 메시지를 반영해 이어 간다.
    turn: CancellationToken,
    inbox: Vec<String>,
    steer: Option<Steer>,
}

impl AgentLine {
    /// 차례 중간에 읽을 수 있는 엔진(API 작업 모드)은 칸에 넣고, 나머지는 지금 차례를 끊는다.
    fn deliver(&mut self, text: String) {
        if let Some(s) = &self.steer {
            if let Some(v) = s.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
                v.push(text);
                return;
            }
        }
        self.inbox.push(text);
        self.turn.cancel();
    }
}

impl AppState {
    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
    pub fn installed(&self) -> Vec<EngineId> {
        self.engines.read().unwrap_or_else(|p| p.into_inner()).iter().filter(|e| e.installed).map(|e| e.id).collect()
    }
    pub fn catalog(&self) -> Catalog {
        let t = store::toggles(&self.db).unwrap_or_default();
        skills::load(&self.data_dir, &t)
    }
}

// ---------------------------------------------------------------------------
// 계획(순수 함수: 테스트 대상)

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRef {
    pub id: String,
    pub name: String,
    pub how: &'static str,
    /// 스킬 폴더(참고 파일을 읽을 수 있게 CLI에 넘긴다)
    #[serde(skip)]
    pub dir: Option<String>,
}

/// 스킬 본문 + 참고 파일 목록(필요한 것만 열어 읽게). 파일을 못 읽는 API 엔진은 본문만으로 답한다.
fn skill_body(s: &Skill) -> String {
    let refs = skills::references(&s.path);
    if refs.is_empty() {
        return s.body.clone();
    }
    let mut b = format!("{}\n\n[이 스킬의 참고 파일]\n지금 단계에 필요한 파일만 읽기 도구로 열어 읽으세요(모두 읽지 마세요). 파일을 읽을 수 없는 환경이면 위 본문만으로 답하세요.\n", s.body.trim_end());
    for (path, title) in refs {
        b.push_str(&format!("- {path}{}\n", if title.is_empty() { String::new() } else { format!(" — {title}") }));
    }
    b
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub primary: EngineId,
    /// 자동으로 고른 경우, 실패하면 차례로 시도할 엔진
    pub fallbacks: Vec<EngineId>,
    pub reviewer: Option<EngineId>,
    pub skill: Option<(SkillRef, String)>,
    pub prompt: String,
    pub work: bool,
    pub reasons: Vec<String>,
    /// 이 계획을 세울 때 쓴 JEV 판단(있으면)
    pub jev: Option<Judgment>,
    /// JEV 난이도로 정한 노력 단계(있으면)
    pub budget: Option<Budget>,
    /// 정확도가 중요할 때: 두 AI가 따로 답하고(필요하면 서로 반론) 판정 AI가 정리한다
    pub debate: Option<Debate>,
    /// 요청 글에서 고른 모델(엔진 → 모델). 그 엔진이 답할 때 기본값·JEV 모델보다 먼저 쓴다.
    pub models: Vec<(EngineId, String)>,
    /// 동시 에이전트: 여러 AI가 같은 요청에 동시에 따로 답하고 종합 AI가 하나로 정리한다(검토·토론 대신).
    pub team: Option<Team>,
    /// 회의: 여러 AI가 여러 라운드 동안 서로의 발언을 읽고 대화하며 푼다(동시 에이전트·토론·검토 대신).
    pub council: Option<Council>,
    /// 동시 에이전트를 정하며 남긴 이유(회의로 바뀌면 지운다)
    pub team_reasons: Vec<String>,
    /// OpenCode에 줄 무료 모델(요청·설정·첨부·민감도·JEV 판단으로 정한다)
    pub free_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Council {
    pub agents: Vec<TeamAgent>,
    /// 최대 라운드(합의하면 더 일찍 끝난다)
    pub rounds: u8,
    /// 합의하지 못했을 때 회의 기록을 정리하는 AI
    pub synth: EngineId,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TeamAgent {
    pub engine: EngineId,
    /// None이면 그 엔진의 기본(또는 JEV) 모델
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub agents: Vec<TeamAgent>,
    /// 답들을 하나로 종합하는 AI
    pub synth: EngineId,
    /// 요청 글에서 정한 워커 노력치(없으면 노력 단계를 따른다)
    pub agent_effort: Option<String>,
    /// 요청 글에서 정한 팀이면 JEV가 AI를 다시 배정하지 않는다
    pub pinned: bool,
    /// 요청 글에 적힌 팀별 인원. agents를 앞에서부터 이 수만큼 묶는다(없으면 비어 있다).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<usize>,
    /// 요청 글에 적힌 자리별 노력치(agents와 같은 순서)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub member_efforts: Vec<Option<String>>,
    /// 요청 글에 적힌 팀마다의 부서(groups와 같은 순서, 바깥 → 안쪽)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depts: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Debate {
    pub a: EngineId,
    pub b: EngineId,
    pub judge: EngineId,
    /// 서로의 답을 보고 반론·수정하는 2차 라운드를 할지(가벼운 단계면 건너뛴다)
    pub rebuttal: bool,
}

/// JEV 판단 결과(요청 종류·난이도)
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Judgment {
    pub model: String,
    /// code · writing · research · analysis · chat · translate
    pub task: Option<String>,
    pub task_confidence: Option<f64>,
    /// 0(아주 쉬움) ~ 3(아주 어려움)
    pub difficulty: Option<f64>,
    pub difficulty_confidence: Option<f64>,
    /// 0(틀려도 괜찮음) ~ 3(틀리면 큰일): 정확도가 얼마나 중요한지
    pub stakes: Option<f64>,
    pub stakes_confidence: Option<f64>,
    /// 최신 웹 정보가 필요할 확률(0~1)
    #[serde(default)]
    pub fresh: Option<f64>,
    /// 이 PC의 파일·프로젝트를 열어 봐야 할 확률(0~1)
    #[serde(default)]
    pub local: Option<f64>,
    /// 켜진 스킬 중 가장 맞는 스킬 id와, 그 스킬이 이 요청을 위한 것일 확률
    pub skill: Option<String>,
    pub skill_fit: Option<f64>,
    /// 가장 알맞은 AI(엔진 id)와 확신도
    pub engine: Option<String>,
    pub engine_confidence: Option<f64>,
    /// 작업을 나눌 독립 부분의 수와, 나누지 않는 편이 낫다고 본 확률
    pub parts: Option<u8>,
    pub parts_single: Option<f64>,
    /// 요청·첨부에 비밀번호·개인 정보 같은 민감한 내용이 있을 확률(0~1)
    #[serde(default)]
    pub sensitive: Option<f64>,
    /// 파일을 고치는 작업의 위험도(0~3)
    #[serde(default)]
    pub risk: Option<f64>,
    /// OpenCode 무료 모델 중 가장 알맞은 모델과 확신도
    #[serde(default)]
    pub free_model: Option<String>,
    #[serde(default)]
    pub free_model_confidence: Option<f64>,
    pub ms: u64,
    pub cost_usd: f64,
}

/// JEV 단가(USD / 1M 입력 토큰, 2026-10-31 확인값)
const JEV_PRICE: f64 = crate::pricing::JEV_INPUT;
/// 이 확신도보다 낮은 JEV 판단은 쓰지 않고 규칙으로 정한다.
const JEV_MIN_CONFIDENCE: f64 = 0.5;
/// 웹·파일 도구가 필요할 확률이 이 이상이면 도구가 있는 CLI를 API보다 먼저 쓴다(API 대화 모드는 도구가 없다).
const JEV_TOOLS: f64 = 0.6;
/// 여러 AI의 답이 같은 결론일 확률이 이 이상이면 반론·판정·종합을 건너뛰고 가장 좋은 답을 쓴다.
const JEV_AGREE: f64 = 0.8;
/// 초안이 이미 완성됐을 확률이 이 이상이면 검토를 건너뛴다.
const JEV_DRAFT_OK: f64 = 0.85;
/// 스킬이 이 요청을 위한 것일 확률이 이 이상이면 적용한다. 실측(2026-11-01, 샘플 18개): 맞는 요청 0.80 이상, 스킬이 필요 없는 요청 0.30 이하.
const JEV_SKILL_FIT: f64 = 0.6;
/// 나누지 않는 편이 낫다고 본 확률이 이 이하일 때만 병렬로 돈다(병렬은 사본·통합 비용이 든다). 실측: 나눌 요청 0.04 이하, 아닌 요청 0.70 이상.
const JEV_SPLIT_MAX_SINGLE: f64 = 0.3;
/// 완성 확률이 이 값 아래면 검토 계획이 없어도 다른 AI에게 검토를 맡긴다. 실측(2026-11-02): 맞는 답 0.84 이상, 틀린 답 0.30, 답을 못 한 경우 0.00.
const JEV_ESCALATE: f64 = 0.4;
/// 검토 답이 초안보다 나쁠 확률이 이 이상이면 초안을 쓴다.
const JEV_KEEP_DRAFT: f64 = 0.75;
/// 작업이 끝나지 않았을 확률이 이 이상이면 같은 AI에게 한 차례 더 맡긴다. 실측: 끝난 보고 0.51, 남은 일을 적은 보고 1.00, 사용자에게 되묻는 보고 0.01.
const JEV_UNFINISHED: f64 = 0.7;
/// 오류가 일시적일 확률이 이 이상이면 같은 AI로 한 번 더 시도한다. 실측: 429·503·연결 끊김 0.98 이상, 키·로그인·한도 소진·없는 모델 0.16 이하.
const JEV_RETRY: f64 = 0.6;
/// 바뀐 파일이 사용자에게 건넬 결과물일 확률이 이 이상이면 답에 붙인다.
const JEV_DELIVERABLE: f64 = 0.7;
/// 답에 자동으로 붙이는 파일 수와 JEV에게 보여 주는 후보 수
const JEV_MAX_AUTO_ATTACH: usize = 5;
const JEV_MAX_ATTACH_CANDIDATES: usize = 30;
/// 민감한 내용일 확률이 이 이상이면 입력을 보관하지 않는 무료 모델만 쓴다.
const JEV_SENSITIVE: f64 = 0.5;
/// JEV에게 보여 주는 첨부 파일 앞부분 글자 수와, 다시 붙일지 물어보는 이전 첨부 수
const JEV_FILE_CHARS: usize = 1500;
const JEV_MAX_EARLIER: usize = 12;
/// JEV에 보내는 스킬 설명 하나의 최대 글자 수
const JEV_SKILL_CHARS: usize = 240;
/// JEV에 보내는 답 하나의 최대 글자 수
const JEV_ANSWER_CHARS: usize = 6000;

pub fn jev_questions() -> serde_json::Map<String, Value> {
    let mut q = serde_json::Map::new();
    q.insert("task".into(), json!({"type": "choice", "instructions": "What kind of work does `request` mainly ask for?", "criteria": {
        "code": "Writing, fixing, reviewing or explaining code, scripts, files or software tools",
        "writing": "Drafting or editing prose such as emails, reports, posts, documents or stories",
        "research": "Finding facts, current events, prices, sources or other up-to-date information",
        "analysis": "Reasoning, planning, comparing options, strategy, math or decisions",
        "chat": "Greeting, small talk, or a quick simple question with a short answer",
        "translate": "Translating text between languages"
    }}));
    q.insert("difficulty".into(), json!({"type": "score", "instructions": "How much reasoning capability does a complete, high-quality answer to `request` need?", "criteria": [
        "Trivial: greeting or a one-line fact",
        "Short factual question, classification or brief rewrite",
        "Multi-part explanation, summary, comparison, plan or code explanation",
        "Complex coding, architecture, research synthesis or multi-step reasoning"
    ]}));
    q.insert("stakes".into(), json!({"type": "score", "instructions": "How important is factual accuracy for `request`: how costly would a wrong, outdated or made-up answer be for the user?", "criteria": [
        "Not important: greeting, chit-chat, brainstorming or creative play",
        "Low: casual information where small errors do not matter",
        "High: facts, numbers, code correctness, technical or historical claims the user will rely on",
        "Critical: health, law, money, safety, calculations or decisions with real consequences"
    ]}));
    q.insert("fresh".into(), json!({"type": "choice", "instructions": "Does a good answer to `request` need current information from the web?", "criteria": {
        "needed": "Needs recent or live information: news, prices, releases, versions, schedules, weather, scores, or documentation that may have changed in the last year",
        "no": "General knowledge, reasoning or the text in `request` is enough"
    }}));
    q.insert("local".into(), json!({"type": "choice", "instructions": "Does answering `request` require opening the user's own files, folders, project or code on this computer?", "criteria": {
        "needed": "Refers to the user's files, folder, project, repository, codebase, logs or documents that must be read to answer",
        "no": "Can be answered without reading any local file"
    }}));
    q.insert("sensitive".into(), json!({"type": "noul", "instructions": "Does `request` (or `attachments`, if present) contain sensitive data: passwords, API keys or tokens, personal identity numbers, private contact details, medical or financial records, or confidential business information?"}));
    q
}

/// 판단용 파일 설명: 이름·종류·크기와 글 파일의 앞부분. 파일의 내용은 판단 자료일 뿐이다.
fn file_brief(data_dir: &std::path::Path, a: &Attachment) -> Value {
    json!({ "name": a.name, "kind": a.kind, "size_kb": a.size.div_ceil(1024), "excerpt": attachments::excerpt(&attachments::path_of(data_dir, a), &a.kind, JEV_FILE_CHARS) })
}

/// OpenCode 무료 모델을 고를 때 JEV에게 보여 주는 사실(문맥 크기·이미지·데이터 보관). 2026-11-02 models.dev·opencode.ai/docs/zen 확인.
fn free_model_brief(m: &str) -> String {
    let fact = match m {
        "opencode/deepseek-v4.1-flash-free" => "DeepSeek V4.1 Flash: 200K context, reasons, reads images, prompts are not retained.",
        "opencode/glm-5.3-flash-free" => "GLM-5.3 Flash: 200K context, reasons, reads images, prompts are not retained.",
        "opencode/mimo-v2.6-flash-free" => "MiMo V2.6 Flash: 200K context, reasons, reads images; prompts may be used to improve the model.",
        "opencode/nemotron-3.5-super-vl-free" => "Nemotron 3.5 Super VL: 1M context for very long input, reads images; use is logged by NVIDIA.",
        "opencode/nemotron-3.5-lightning-free" => "Nemotron 3.5 Lightning: 262K context, built for speed, text only; use is logged by NVIDIA.",
        "opencode/beam-free" => "Beam: 1M context for very long input, text only; prompts are kept 30 days but not used for training.",
        "opencode/flint-40b-free" => "Flint 40B: 262K context, small open model, text only; prompts are kept 30 days and may be used to improve it.",
        "opencode/big-pickle" => "Big Pickle: 200K context, reasons, text only; prompts may be used to improve the model.",
        _ => "A free OpenCode model.",
    };
    fact.to_string()
}

/// OpenCode에 줄 무료 모델. 이미지가 있으면 이미지를 읽는 모델만, 민감한 요청이면 입력을 보관하지 않는 모델만 후보로 둔다.
pub fn pick_free_model(models: &[String], setting: Option<&str>, jev: Option<&Judgment>, images: bool) -> Option<String> {
    let sensitive = jev.and_then(|j| j.sensitive).is_some_and(|p| p >= JEV_SENSITIVE);
    let fits = |m: &str| engines::is_free_model(m) && (!images || engines::OPENCODE_VISION.contains(&m)) && (!sensitive || engines::OPENCODE_ZERO_RETENTION.contains(&m));
    if let Some(m) = setting.filter(|m| fits(m)) {
        return Some(m.to_string());
    }
    // 이미지는 JEV가 모델을 고르지 않는다: 이름만으로는 실제로 이미지를 보는지 알 수 없다.
    if let Some(m) = jev.filter(|j| !images && j.free_model_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE).and_then(|j| j.free_model.as_deref()).filter(|m| fits(m) && models.iter().any(|x| x == m)) {
        return Some(m.to_string());
    }
    if images && !sensitive && (models.is_empty() || models.iter().any(|x| x == engines::OPENCODE_IMAGE_FIRST)) {
        return Some(engines::OPENCODE_IMAGE_FIRST.to_string());
    }
    if fits(engines::OPENCODE_DEFAULT) && (models.is_empty() || models.iter().any(|x| x == engines::OPENCODE_DEFAULT)) {
        return Some(engines::OPENCODE_DEFAULT.to_string());
    }
    models.iter().find(|m| fits(m)).cloned().or_else(|| Some(engines::OPENCODE_DEFAULT.to_string()))
}

/// 답이 약할 때 검토를 맡길 AI: 초안을 쓴 회사가 아닌 가장 강한 AI. 초안보다 약하면 맡기지 않는다.
fn stronger_reviewer(drafted_by: EngineId, installed: &[EngineId]) -> Option<EngineId> {
    installed.iter().copied().filter(|e| e.vendor() != drafted_by.vendor() && strength(*e) >= strength(drafted_by)).max_by_key(|e| strength(*e))
}

pub(crate) fn cut(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// JEV가 본 여러 답의 관계: 같은 결론일 확률과 가장 좋은 답.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Consensus {
    pub agree: f64,
    /// answers 안의 위치
    pub best: usize,
    pub ms: u64,
    pub cost_usd: f64,
}

/// 같은 요청에 대한 여러 답이 같은 결론인지, 어느 답이 가장 좋은지 JEV에게 한 번에 묻는다.
pub async fn compare_answers(http: &reqwest::Client, key: &str, request: &str, answers: &[&str]) -> Result<Consensus, String> {
    let mut state = serde_json::Map::new();
    state.insert("request".into(), Value::String(cut(request, memory::FILTER_REQUEST_CHARS)));
    let mut best = serde_json::Map::new();
    for (i, a) in answers.iter().enumerate() {
        let k = format!("a{}", i + 1);
        state.insert(k.clone(), Value::String(cut(a, JEV_ANSWER_CHARS)));
        best.insert(k.clone(), Value::String(format!("`{k}` is the most correct, complete and useful answer to `request`")));
    }
    let mut q = serde_json::Map::new();
    q.insert("agree".into(), json!({"type": "choice", "instructions": "`a1`… are answers from different AIs to the same `request`. Do they reach the same conclusion?", "criteria": {
        "same": "All answers agree on the key facts, numbers, results, code behaviour and recommendations, even if wording, length or order differ",
        "differ": "At least one answer contradicts another on a key fact, number, result or recommendation, or misses a key part the others cover"
    }}));
    q.insert("best".into(), json!({"type": "choice", "instructions": "Which answer is the best answer to `request`?", "criteria": best}));
    let r = jev::ask(http, key, "jev-latest", Value::Object(state), q, 1, Duration::from_secs(15)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let a = &r.response.answers;
    let agree = jev::prob(a, "agree", "same").ok_or("JEV가 답을 비교하지 못했습니다")?;
    let best = jev::choice(a, "best").and_then(|(c, _)| c.trim_start_matches('a').parse::<usize>().ok()).filter(|n| (1..=answers.len()).contains(n)).map(|n| n - 1).unwrap_or(0);
    Ok(Consensus { agree, best, ms: r.latency_ms, cost_usd: r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6 })
}

/// 검토 답이 초안보다 나빠졌을 확률. (확률, ms, 비용)
pub async fn review_worse(http: &reqwest::Client, key: &str, request: &str, draft: &str, review: &str) -> Result<(f64, u64, f64), String> {
    let state = json!({ "request": cut(request, memory::FILTER_REQUEST_CHARS), "draft": cut(draft, JEV_ANSWER_CHARS), "review": cut(review, JEV_ANSWER_CHARS) });
    let mut q = serde_json::Map::new();
    q.insert("verdict".into(), json!({"type": "choice", "instructions": "`review` is a reviewer's rewrite of `draft`, an answer to `request`. Compare them as answers to `request`.", "criteria": {
        "better": "`review` is at least as correct and complete as `draft`, and fixes or keeps everything that matters",
        "worse": "`review` lost correct content, added an error, or answers `request` less well than `draft`"
    }}));
    let r = jev::ask(http, key, "jev-latest", state, q, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let p = jev::prob(&r.response.answers, "verdict", "worse").ok_or("JEV returned no verdict")?;
    Ok((p, r.latency_ms, r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6))
}

/// 파일을 고치는 작업의 마지막 보고를 보고 일이 끝나지 않았을 확률을 묻는다. 사용자에게 되묻는 경우는 이어 가지 않는다. (확률, ms, 비용)
pub async fn work_unfinished(http: &reqwest::Client, key: &str, request: &str, report: &str) -> Result<(f64, u64, f64), String> {
    let state = json!({ "request": cut(request, memory::FILTER_REQUEST_CHARS), "report": cut(report, JEV_ANSWER_CHARS) });
    let mut q = serde_json::Map::new();
    q.insert("status".into(), json!({"type": "choice", "instructions": "`report` is an AI agent's final report after working on `request` by editing files. What is the state of the work?", "criteria": {
        "done": "Everything `request` asks for is reported as finished, including when some optional step was skipped with a clear reason",
        "unfinished": "The agent stopped early: it says parts are still to do, left placeholders or TODOs, hit an error it did not resolve, or only described what it would do next",
        "needs_user": "The agent cannot continue without a decision, credential or information only the user has, and asks for it"
    }}));
    let r = jev::ask(http, key, "jev-latest", state, q, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let p = jev::prob(&r.response.answers, "status", "unfinished").ok_or("JEV returned no status")?;
    Ok((p, r.latency_ms, r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6))
}

/// 실패한 AI를 바로 다시 불러 볼 만한지(일시적인 오류일 확률). (확률, ms, 비용)
pub async fn failure_transient(http: &reqwest::Client, key: &str, engine: &str, error: &str) -> Result<(f64, u64, f64), String> {
    let state = json!({ "ai": engine, "error": cut(error, 800) });
    let mut q = serde_json::Map::new();
    q.insert("kind".into(), json!({"type": "choice", "instructions": "`error` is why a call to the AI named `ai` failed. Would the same call likely succeed if repeated a few seconds later?", "criteria": {
        "transient": "Temporary: overload, rate limit that resets soon, network drop, timeout, stream cut, or a server error",
        "permanent": "Will fail again: not logged in, bad key, used-up quota or credit, unknown model, invalid request, refusal, or missing software"
    }}));
    let r = jev::ask(http, key, "jev-latest", state, q, 1, Duration::from_secs(8)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let p = jev::prob(&r.response.answers, "kind", "transient").ok_or("JEV returned no kind")?;
    Ok((p, r.latency_ms, r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6))
}

/// 앞선 차례의 첨부 파일 중 지금 요청에 필요한 것을 고른다. (위치들, ms, 비용)
pub async fn pick_earlier_files(http: &reqwest::Client, key: &str, data_dir: &std::path::Path, text: &str, files: &[Attachment]) -> Result<(Vec<usize>, u64, f64), String> {
    let mut list = serde_json::Map::new();
    let mut q = serde_json::Map::new();
    for (i, f) in files.iter().enumerate() {
        let k = format!("f{i}");
        list.insert(k.clone(), file_brief(data_dir, f));
        q.insert(k.clone(), json!({"type": "choice", "instructions": format!("`files.{k}` was attached earlier in this chat session. Does the assistant need to open it to answer `request`?"), "criteria": {
            "needed": "`request` refers to, continues work on, or depends on the content of this file",
            "skip": "`request` is unrelated to this file"
        }}));
    }
    let state = json!({ "request": cut(text, memory::FILTER_REQUEST_CHARS), "files": list });
    let r = jev::ask(http, key, "jev-latest", state, q, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let a = &r.response.answers;
    let keep = (0..files.len()).filter(|i| jev::prob(a, &format!("f{i}"), "needed").unwrap_or(0.0) >= 0.5).collect();
    Ok((keep, r.latency_ms, r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6))
}

/// JEV 없이 앞선 첨부를 다시 붙이는 규칙: 요청이 파일 이름을 말할 때만.
fn earlier_by_name(text: &str, files: &[Attachment]) -> Vec<usize> {
    let low = text.to_lowercase();
    (0..files.len()).filter(|i| low.contains(&files[*i].name.to_lowercase())).collect()
}

/// 입력창 추천: 보내기 전에 어떤 모드와 스킬이 맞는지.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Suggestion {
    /// chat · work · build · council
    pub mode: Option<String>,
    pub mode_confidence: Option<f64>,
    pub skill: Option<String>,
    pub skill_fit: Option<f64>,
    pub ms: u64,
    pub cost_usd: f64,
}

pub async fn suggest(http: &reqwest::Client, key: &str, text: &str, skills: &[&Skill], has_files: bool) -> Result<Suggestion, String> {
    let (mut state, mut q) = route_request(text, &RouteAsk { skills: skills.to_vec(), ..Default::default() });
    state["has_attachments"] = json!(has_files);
    q.insert("mode".into(), json!({"type": "choice", "instructions": "A user is typing `request` to an AI app that has several ways to run it. Which way fits best?", "criteria": {
        "chat": "A question, explanation, advice or text answer: no file on disk needs to change",
        "work": "Change, fix or refactor specific existing files or an existing project",
        "build": "Create a whole new app, website, tool, script or project and finish it",
        "council": "A hard open-ended decision or design question where several AIs arguing the trade-offs would clearly beat one answer"
    }}));
    let r = jev::ask(http, key, "jev-latest", state, q, 0, Duration::from_secs(6)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let a = &r.response.answers;
    let mode = jev::choice(a, "mode");
    Ok(Suggestion {
        mode: mode.as_ref().map(|m| m.0.clone()),
        mode_confidence: mode.as_ref().map(|m| m.1),
        skill: jev::choice(a, "skill").map(|c| c.0).or_else(|| (skills.len() == 1).then(|| skills[0].id.clone())),
        skill_fit: jev::noul(a, "skill_fit"),
        ms: r.latency_ms,
        cost_usd: r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6,
    })
}

/// 초안이 이미 요청에 완전하고 정확하게 답했을 확률(검토를 건너뛸지 정한다). (확률, ms, 비용)
pub async fn draft_ready(http: &reqwest::Client, key: &str, request: &str, draft: &str) -> Result<(f64, u64, f64), String> {
    let state = json!({ "request": cut(request, memory::FILTER_REQUEST_CHARS), "draft": cut(draft, JEV_ANSWER_CHARS) });
    let mut q = serde_json::Map::new();
    q.insert("ready".into(), json!({"type": "choice", "instructions": "`draft` is an AI's answer to `request`. Would a careful expert reviewer leave it as it is?", "criteria": {
        "complete": "`draft` fully and correctly answers every part of `request` with no errors, gaps, vague steps or made-up facts",
        "fix": "`draft` has an error, a missing part, a vague or unrunnable step, a doubtful claim, or does not really answer `request`"
    }}));
    let r = jev::ask(http, key, "jev-latest", state, q, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let p = jev::prob(&r.response.answers, "ready", "complete").ok_or("JEV가 초안을 판단하지 못했습니다")?;
    Ok((p, r.latency_ms, r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6))
}

/// JEV에게 이번 요청에 대해 무엇을 물을지.
#[derive(Default)]
pub struct RouteAsk<'a> {
    /// 요청 종류·난이도·정확도·도구 필요 여부
    pub core: bool,
    /// 지금 켜진 스킬(자동 적용 후보)
    pub skills: Vec<&'a Skill>,
    /// 고를 수 있는 AI
    pub engines: Vec<EngineId>,
    /// 몇 부분으로 나눌지(파일을 고치는 요청만)
    pub parts: bool,
    /// 이 요청에 붙은 파일 설명
    pub files: Vec<Value>,
    /// 고를 수 있는 OpenCode 무료 모델(이 요청에 OpenCode를 쓸 수 있을 때만)
    pub free_models: Vec<String>,
    /// 파일을 고치는 요청인지(위험도를 묻는다)
    pub work: bool,
}

/// AI를 고를 때 JEV에게 보여 주는 설명: 무엇을 잘하는지, 파일·웹을 쓸 수 있는지, 비용.
fn engine_brief(e: EngineId) -> &'static str {
    match e {
        EngineId::Claude => "Claude Code CLI: strongest at coding, multi-file edits and careful reasoning; can read and edit files and search the web. Most expensive and slower.",
        EngineId::Codex => "Codex CLI: strong at coding and code edits; can read and edit files. Mid cost.",
        EngineId::Antigravity => "Antigravity CLI (Gemini): good at research with web search and at reading files; can edit files. Mid cost.",
        EngineId::OpenCode => "OpenCode CLI with free models only: costs nothing; can read and edit files and search the web. Weaker than paid AIs, so only for simple or moderate work.",
        EngineId::ClaudeApi => "Claude API: careful writing and reasoning; no web access in chat. Mid to high cost.",
        EngineId::OpenAi => "OpenAI API: general writing, reasoning and code answers; no web access in chat. Mid cost.",
        EngineId::GeminiApi => "Gemini API: very fast and cheap; no file or web access in chat. Best for quick questions, translation and simple rewrites.",
        EngineId::Nvidia => "NVIDIA API (open models): cheapest; no file or web access in chat. Only for simple questions and short rewrites.",
    }
}

fn engine_criteria(engines: &[EngineId]) -> Value {
    Value::Object(engines.iter().map(|e| (e.as_str().to_string(), Value::String(engine_brief(*e).to_string()))).collect())
}

/// 같은 상태에 대한 독립 질문이라 한 요청으로 묶는다(모델은 병렬로 판단한다). 선택지가 하나면 고를 것이 없으니 Choice를 묻지 않는다.
fn route_request(text: &str, ask: &RouteAsk) -> (Value, serde_json::Map<String, Value>) {
    let mut state = serde_json::Map::new();
    state.insert("request".into(), Value::String(text.to_string()));
    if !ask.files.is_empty() {
        state.insert("attachments".into(), Value::Array(ask.files.clone()));
    }
    let mut q = if ask.core { jev_questions() } else { serde_json::Map::new() };
    if ask.work {
        q.insert("risk".into(), json!({"type": "score", "instructions": "An AI will carry out `request` by editing files and running commands on the user's computer. How much damage could a mistake do?", "criteria": [
            "None: new files only, or easily undone edits",
            "Low: ordinary code or text edits in a project",
            "High: bulk changes, deleting or moving many files, or changing configuration, data or deployment",
            "Critical: production systems, credentials, payments, or anything that cannot be undone"
        ]}));
    }
    if ask.free_models.len() >= 2 {
        let list: serde_json::Map<String, Value> = ask.free_models.iter().take(255).map(|m| (m.clone(), Value::String(free_model_brief(m)))).collect();
        q.insert("free_model".into(), json!({"type": "choice", "instructions": "If OpenCode handles `request`, which of its free models fits best? Weigh input length, whether images are attached, and how private the content is.", "criteria": list}));
    }
    if !ask.skills.is_empty() {
        // API 한도: Choice 선택지 255개
        let list: serde_json::Map<String, Value> = ask.skills.iter().take(255).map(|s| (s.id.clone(), Value::String(if s.description.trim().is_empty() { s.name.clone() } else { cut(&s.description, JEV_SKILL_CHARS) }))).collect();
        state.insert("skills".into(), Value::Object(list.clone()));
        if list.len() >= 2 {
            q.insert("skill".into(), json!({"type": "choice", "instructions": "Which skill in `skills` best matches what `request` asks the assistant to do?", "criteria": list}));
        }
        q.insert("skill_fit".into(), json!({"type": "noul", "instructions": "Is `request` a task that one of `skills` is specifically designed for, so that following that skill's procedure would clearly improve the result? Answer no for greetings, small talk, simple questions answerable in a sentence or two, and when the action asked for differs from what the skill does (for example, changing code is not reviewing it)."}));
    }
    if ask.engines.len() >= 2 {
        q.insert("engine".into(), json!({"type": "choice", "instructions": "Which AI should handle `request` to get a good result at the lowest cost and time? Prefer the cheapest AI that can do it well.", "criteria": engine_criteria(&ask.engines)}));
    }
    if ask.parts {
        q.insert("parts".into(), json!({"type": "choice", "instructions": "An AI will carry out `request` by editing files in a project. Into how many independent parts should the work be split to finish most efficiently? Splitting has overhead, so only split when the parts are clearly separate.", "criteria": {
            "one": "One agent should do it: small, or a single coherent change where the parts depend on each other or touch the same files",
            "two": "Exactly two substantial parts that can be done at the same time without seeing each other's results and touch different files",
            "three": "Exactly three such independent parts",
            "four": "Four or more such independent parts"
        }}));
    }
    (Value::Object(state), q)
}

pub async fn judge(http: &reqwest::Client, key: &str, text: &str) -> Result<Judgment, String> {
    route(http, key, text, &RouteAsk { core: true, ..Default::default() }).await
}

pub async fn route(http: &reqwest::Client, key: &str, text: &str, ask: &RouteAsk<'_>) -> Result<Judgment, String> {
    let (state, questions) = route_request(text, ask);
    let r = jev::ask(http, key, "jev-latest", state, questions, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let a = &r.response.answers;
    let picked = jev::choice(a, "engine");
    let parts = jev::choice(a, "parts");
    let only_skill = (ask.skills.len() == 1).then(|| ask.skills[0].id.clone());
    let task = jev::choice(a, "task");
    let diff = jev::score(a, "difficulty");
    let stakes = jev::score(a, "stakes");
    Ok(Judgment {
        model: r.response.model.clone(),
        task: task.as_ref().map(|t| t.0.clone()),
        task_confidence: task.as_ref().map(|t| t.1),
        difficulty: diff.map(|d| d.0),
        difficulty_confidence: diff.map(|d| d.1),
        stakes: stakes.map(|s| s.0),
        stakes_confidence: stakes.map(|s| s.1),
        fresh: jev::prob(a, "fresh", "needed"),
        local: jev::prob(a, "local", "needed"),
        skill: jev::choice(a, "skill").map(|c| c.0).or(only_skill),
        skill_fit: match a.get("skill_fit") {
            Some(jev::Answer::Noul { noul }) => Some(*noul),
            _ => None,
        },
        engine: picked.as_ref().map(|c| c.0.clone()),
        engine_confidence: picked.as_ref().map(|c| c.1),
        parts: parts.as_ref().map(|c| match c.0.as_str() {
            "two" => 2,
            "three" => 3,
            "four" => 4,
            _ => 1,
        }),
        parts_single: jev::prob(a, "parts", "one"),
        sensitive: jev::noul(a, "sensitive"),
        risk: jev::score(a, "risk").map(|x| x.0),
        free_model: jev::choice(a, "free_model").map(|c| c.0),
        free_model_confidence: jev::choice(a, "free_model").map(|c| c.1),
        ms: r.latency_ms,
        cost_usd: r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6,
    })
}

/// JEV가 고른 스킬: 켜져 있고 이 요청을 위한 것일 때만.
fn jev_skill<'a>(jev: Option<&Judgment>, catalog: &'a Catalog) -> Option<(&'a Skill, f64)> {
    let j = jev?;
    let fit = j.skill_fit.filter(|f| *f >= JEV_SKILL_FIT)?;
    let id = j.skill.as_deref()?;
    catalog.enabled_skills().find(|s| s.id == id).map(|s| (s, fit))
}

/// JEV가 고른 AI. 고를 수 없는 AI이거나 도구가 필요한데 도구 없는 API를 고른 경우, 어려운 요청에 약한 AI를 고른 경우는 따르지 않는다.
fn jev_engine(jev: Option<&Judgment>, available: &[EngineId], tools_first: bool) -> Option<EngineId> {
    let j = jev?;
    let e = j.engine.as_deref().filter(|_| j.engine_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE).and_then(EngineId::parse)?;
    if !available.contains(&e) || (tools_first && e.is_api() && available.iter().any(|x| !x.is_api())) {
        return None;
    }
    let hard = j.difficulty.filter(|_| j.difficulty_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE).is_some_and(|d| d >= 2.0);
    (!hard || strength(e) >= 4).then_some(e)
}

/// JEV가 본 작업 분할 수(나누지 않으면 None).
fn jev_parts(jev: Option<&Judgment>) -> Option<u8> {
    let j = jev?;
    j.parts.filter(|n| *n >= 2 && j.parts_single.is_some_and(|p| p <= JEV_SPLIT_MAX_SINGLE))
}

/// JEV 기억 거르기 결과(사용량 기록용 시간·비용 포함)
pub struct MemoryFilter {
    pub selection: memory::Selection,
    pub ms: u64,
    pub cost_usd: f64,
}

/// 세션 메모리(요약 항목·최근 대화)를 하나씩 "지금 요청에 필요한가"로 JEV에게 물어 필요한 것만 남긴다.
/// 질문은 한 번의 요청으로 묶어 보낸다. Ok(None)은 거를 만큼 기억이 길지 않아 부르지 않은 것.
pub async fn filter_memory(http: &reqwest::Client, key: &str, text: &str, summary: Option<&str>, history: &[memory::Turn]) -> Result<Option<MemoryFilter>, String> {
    let Some(c) = memory::candidates(summary, history) else { return Ok(None) };
    let (state, questions) = memory::filter_questions(text, &c, history);
    let r = jev::ask(http, key, "jev-latest", state, questions, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let a = &r.response.answers;
    let selection = memory::select(&c, |k| jev::prob(a, k, "needed"));
    Ok(Some(MemoryFilter { selection, ms: r.latency_ms, cost_usd: r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6 }))
}

/// JEV 난이도로 정한 노력 단계. 쉬운 요청에 큰 모델·높은 노력을 쓰지 않아 토큰을 아낀다.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Budget {
    /// light · standard · deep
    pub tier: &'static str,
    pub effort: &'static str,
    /// Claude Code에 줄 모델 별칭(None이면 기본 모델)
    pub claude_model: Option<&'static str>,
    pub max_tokens: u32,
}

/// 실측(2026-10-17, 인사 한 문장): haiku·low $0.0007 / sonnet·medium $0.0103 / 기본(opus) 약 $0.03.
pub fn budget(jev: Option<&Judgment>, work: bool) -> Option<Budget> {
    let j = jev?;
    let d = j.difficulty.filter(|_| j.difficulty_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE)?;
    let light_task = matches!(j.task.as_deref(), Some("chat") | Some("translate"));
    let tier = if (d < 1.0 || (light_task && d < 1.5)) && !work {
        Budget { tier: "light", effort: "low", claude_model: Some("haiku"), max_tokens: 1500 }
    } else if d < 2.0 {
        // 파일을 고치는 작업은 가장 가벼운 단계로 내리지 않는다.
        Budget { tier: "standard", effort: "medium", claude_model: Some("sonnet"), max_tokens: 4000 }
    } else {
        Budget { tier: "deep", effort: "high", claude_model: None, max_tokens: 8192 }
    };
    Some(tier)
}

/// 판정·정리를 맡길 때의 대략적인 강함(사실·계산 정확도). 클수록 강하다.
fn strength(e: EngineId) -> u8 {
    match e {
        EngineId::Claude => 7,
        EngineId::ClaudeApi => 6,
        EngineId::OpenAi | EngineId::Codex => 5,
        EngineId::GeminiApi | EngineId::Antigravity => 4,
        EngineId::OpenCode => 3,
        EngineId::Nvidia => 2,
    }
}

/// 스킬이 고정한 노력 단계
pub fn budget_for(tier: &str) -> Option<Budget> {
    Some(match tier {
        "light" => Budget { tier: "light", effort: "low", claude_model: Some("haiku"), max_tokens: 1500 },
        "standard" => Budget { tier: "standard", effort: "medium", claude_model: Some("sonnet"), max_tokens: 4000 },
        "deep" => Budget { tier: "deep", effort: "high", claude_model: None, max_tokens: 8192 },
        _ => return None,
    })
}

fn tier_label(t: &str) -> &'static str {
    match t {
        "light" => "가볍게",
        "standard" => "보통",
        _ => "깊게",
    }
}

/// 요청 종류별로 먼저 시도할 엔진 순서. 대화·번역처럼 가벼운 일은 빠른 API를, 코드는 CLI를 앞에 둔다.
fn order_for(kind: &str) -> Vec<EngineId> {
    use EngineId::*;
    match kind {
        "code" => vec![Claude, Codex, Antigravity, ClaudeApi, OpenAi, GeminiApi, OpenCode, Nvidia],
        "research" => vec![Antigravity, Claude, GeminiApi, ClaudeApi, OpenAi, Codex, OpenCode, Nvidia],
        "chat" | "translate" => vec![GeminiApi, Nvidia, ClaudeApi, OpenAi, OpenCode, Claude, Antigravity, Codex],
        "writing" | "analysis" => vec![Claude, ClaudeApi, Antigravity, GeminiApi, OpenAi, Codex, OpenCode, Nvidia],
        _ => vec![Claude, Antigravity, Codex, ClaudeApi, GeminiApi, OpenAi, OpenCode, Nvidia],
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "code" => "코드·파일",
        "research" => "조사",
        "chat" => "가벼운 대화",
        "translate" => "번역",
        "writing" => "글쓰기",
        "analysis" => "분석",
        _ => "일반",
    }
}

const CODE_WORDS: &[&str] = &[
    "코드", "함수", "버그", "에러", "오류", "컴파일", "리팩", "구현", "파일", "폴더", "스크립트", "테스트", "API", "깃", "파이썬", "자바스크립트", "타입스크립트",
    "러스트", "python", "javascript", "typescript", "rust", "sql", "html", "css", "정규식",
];
const DEEP_WORDS: &[&str] = & ["자세히", "완성", "보고서", "분석", "계획", "설계", "비교", "전략", "단계별", "장단점", "검토", "조사", "정리해", "기획"];

fn looks_like_code(t: &str) -> bool {
    let low = t.to_lowercase();
    t.contains("```") || CODE_WORDS.iter().any(|w| low.contains(&w.to_lowercase())) || t.contains(".rs") || t.contains(".py") || t.contains(".ts")
}

const ACCURACY_WORDS: &[&str] = &["정확", "팩트", "사실인지", "사실 확인", "검증", "틀리면", "근거", "출처", "계산해"];

/// JEV가 없을 때: 정확도가 중요해 보이는 요청
fn needs_accuracy(t: &str) -> bool {
    ACCURACY_WORDS.iter().any(|w| t.contains(w))
}

fn complex(t: &str) -> bool {
    t.chars().count() >= 140 || DEEP_WORDS.iter().any(|w| t.contains(w))
}

#[cfg(test)]
pub fn plan(text: &str, choice: Option<EngineId>, work_requested: bool, installed: &[EngineId], catalog: &Catalog, review: ReviewMode, jev: Option<&Judgment>) -> Result<Plan, String> {
    plan_with(text, choice, work_requested, installed, catalog, review, jev, 1)
}

/// agents: 설정의 기본 동시 에이전트 수(1이면 끔). 요청 글에서 수나 모델을 여럿 정하면 그쪽이 먼저다.
#[allow(clippy::too_many_arguments)]
pub fn plan_with(text: &str, choice: Option<EngineId>, work_requested: bool, installed: &[EngineId], catalog: &Catalog, review: ReviewMode, jev: Option<&Judgment>, agents: u8) -> Result<Plan, String> {
    plan_read(text, choice, work_requested, installed, catalog, review, jev, agents, TeamText::Parse)
}

/// team_lines: AI가 요청 글에서 뽑은 팀 구성표(파서가 글을 못 읽었을 때만 있다)
#[allow(clippy::too_many_arguments)]
fn plan_read(text: &str, choice: Option<EngineId>, work_requested: bool, installed: &[EngineId], catalog: &Catalog, review: ReviewMode, jev: Option<&Judgment>, agents: u8, team: TeamText) -> Result<Plan, String> {
    // 글의 내용으로 적은 팀이면 팀·여러 모델·에이전트 수를 쓰지 않고, 자동 분할도 하지 않는다.
    let ignore_team = matches!(team, TeamText::Ignore);
    let team_lines = match &team {
        TeamText::Lines(l) => Some(l.as_str()),
        _ => None,
    };
    let given = match &team {
        TeamText::Given(r) => Some(r.clone()),
        _ => None,
    };
    let mut reasons = vec![];
    // 1) 스킬
    let (skill, prompt): (Option<&Skill>, String) = match catalog.explicit(text) {
        Some((s, rest)) => {
            reasons.push(format!("/{} 스킬을 불렀습니다", s.name));
            (Some(s), if rest.is_empty() { s.description.clone() } else { rest.to_string() })
        }
        None => match jev_skill(jev, catalog) {
            Some((s, fit)) => {
                reasons.push(format!("JEV: {name} 스킬 적용 ({pct}%)", name = s.name, pct = (fit * 100.0).round() as u32));
                (Some(s), text.trim().to_string())
            }
            None => (None, text.trim().to_string()),
        },
    };
    // 모델·에이전트 수 지시("opus를 사용하여", "에이전트 3개로")는 엔진을 고르는 데만 쓰고 AI에게는 할 일만 넘긴다.
    let prompt = intent::strip_directives(&prompt);
    if prompt.is_empty() {
        return Err("요청 내용이 비어 있습니다".into());
    }
    let work = work_requested || skill.is_some_and(|s| s.mode.as_deref() == Some("work"));
    if work && !work_requested {
        reasons.push("스킬이 작업 모드(파일 수정)를 요구합니다".into());
    }

    // 2) 엔진
    if installed.is_empty() {
        return Err("사용할 수 있는 AI가 없습니다. Claude Code·Codex·Antigravity를 설치하거나 설정에서 API 키를 넣으세요".into());
    }
    // 요청 종류: JEV가 충분히 확신하면 그 판단을, 아니면 규칙을 쓴다.
    let jev_task = jev.and_then(|j| j.task.clone().filter(|_| j.task_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE));
    let kind = if work { "code".to_string() } else { jev_task.clone().unwrap_or_else(|| if looks_like_code(&prompt) { "code".into() } else { "general".into() }) };
    if let (Some(j), Some(t)) = (jev, jev_task.as_ref()) {
        reasons.push(format!("JEV: {} 요청으로 판단(확신 {:.0}%)", kind_label(t), j.task_confidence.unwrap_or(0.0) * 100.0));
    }
    let order = order_for(&kind);
    // 작업 모드도 API를 쓸 수 있다(엘리시움이 파일·PowerShell 도구를 대신 실행). 다만 CLI가 있으면 CLI가 먼저다.
    let usable = |e: &EngineId| installed.contains(e);
    let mut available: Vec<EngineId> = order.iter().copied().filter(usable).collect();
    // JEV가 최신 웹 정보나 이 PC의 파일이 필요하다고 보면 도구가 있는 CLI를 먼저 쓴다(API 대화 모드는 도구가 없어 헛답이 된다).
    let needs_tools = !work && jev.is_some_and(|j| j.fresh.unwrap_or(0.0) >= JEV_TOOLS || j.local.unwrap_or(0.0) >= JEV_TOOLS);
    let first_before = available.first().copied();
    if work || needs_tools {
        available.sort_by_key(|e| e.is_api());
    }
    if needs_tools && first_before.is_some_and(|e| e.is_api()) && available.first().is_some_and(|e| !e.is_api()) {
        let what = if jev.and_then(|j| j.local).unwrap_or(0.0) >= JEV_TOOLS { "local" } else { "fresh" };
        reasons.push(if what == "local" {
            "JEV: 로컬 파일 조회가 필요하여 도구 사용이 가능한 CLI 에이전트를 우선 호출합니다.".to_string()
        } else {
            "JEV: 최신 웹 정보가 필요하여 검색 도구를 지원하는 CLI 에이전트를 우선 호출합니다.".to_string()
        });
    }
    if available.is_empty() {
        return Err("사용할 수 있는 AI가 없습니다".into());
    }
    let skill_engine = skill.and_then(|s| s.engine.as_deref()).and_then(EngineId::parse);
    // 요청 글에서 고른 모델: 입력창에서 고른 AI보다 먼저 따른다(그 AI로 그 모델을 쓸 수 있으면 그 AI로).
    let wanted = intent::requested_models(text, installed, work);
    for m in &wanted.missing {
        reasons.push(format!("요청하신 {m} 모델을 사용할 수 없어 사용 가능한 AI를 자동으로 선택했습니다"));
    }
    // 요청 글에 적힌 팀(워커 모델·수·총괄)은 JEV 판단과 설정보다 먼저다.
    // 혼합 팀은 작업 모드에서만 받는다. 적힌 모델을 돌릴 수 없으면 다른 AI로 바꾸지 않고 멈춘다.
    let roster = if let Some(r) = given {
        Some(r)
    } else if work && !ignore_team {
        match team_lines {
            Some(lines) => intent::roster_from_answer(lines, text, installed, work),
            None => intent::requested_roster(text, installed, work),
        }
        .map_err(|e| match e {
            intent::RosterError::Missing(name) => format!("{name} 모델을 사용할 수 없어 팀을 실행할 수 없으므로, 모델을 설치하거나 팀에서 제외해 주세요."),
            intent::RosterError::TooMany(n) => format!("팀 인원이 {n}명이며, 최대 허용 인원은 {max}명입니다.", max = intent::MAX_AGENTS),
        })?
    } else {
        None
    };
    let ask = if roster.is_some() || ignore_team { None } else { intent::requested_team(text, installed, work) };
    let lead = roster.as_ref().and_then(|r| r.lead.as_ref()).or(ask.as_ref().and_then(|a| a.lead.as_ref()));
    let mut models: Vec<(EngineId, String)> = vec![];
    let picked = lead.or(wanted.picks.first()).map(|mp| match choice.and_then(|c| mp.alts.iter().find(|(e, _)| *e == c)) {
        Some((e, m)) => (*e, m.clone(), mp),
        None => (mp.engine, mp.model.clone(), mp),
    });
    let (primary, fallbacks) = if let Some((engine, model, mp)) = picked {
        let (label, name) = (engine.label(), mp.name.as_str());
        reasons.push(format!("요청하신 {name} 모델에 맞춰 {label}에서 답변을 생성합니다."));
        if let Some(m) = model {
            models.push((engine, m));
        }
        // 같은 모델을 쓸 수 있는 다른 AI가 있으면 실패할 때 그쪽으로 넘어간다.
        let mut fb = vec![];
        // 요청 글이 정한 총괄이 실패해도 고르지 않은 API로 넘어가지 않는다.
        for (e, m) in mp.alts.iter().filter(|(e, _)| *e != engine && !(work && lead.is_some() && e.is_api())) {
            if let Some(m) = m {
                models.push((*e, m.clone()));
            }
            fb.push(*e);
        }
        (engine, fb)
    } else if let Some(c) = choice {
        if !installed.contains(&c) {
            return Err(format!("{}를 쓸 수 없습니다({})", c.label(), if c.is_api() { "API 키 없음" } else { "설치 안 됨" }));
        }
        reasons.push(format!("직접 고른 {}", c.label()));
        if work && c.is_api() {
            reasons.push(format!("{}는 엘리시움의 파일·PowerShell 도구로 작업합니다", c.label()));
        }
        (c, vec![])
    } else if let Some(e) = skill_engine.filter(|e| usable(e)) {
        reasons.push(format!("스킬이 {}를 지정했습니다", e.label()));
        (e, available.iter().copied().filter(|x| *x != e).collect())
    } else if let Some(e) = jev_engine(jev, &available, work || needs_tools) {
        reasons.push(format!("JEV: {name} 선택 ({pct}%)", name = e.label(), pct = (jev.and_then(|j| j.engine_confidence).unwrap_or(0.0) * 100.0).round() as u32));
        (e, available.iter().copied().filter(|x| *x != e).collect())
    } else {
        let p = available[0];
        reasons.push(format!("{} 요청이라 {}가 먼저 답합니다", kind_label(&kind), p.label()));
        (p, available[1..].to_vec())
    };

    // 동시 에이전트: 요청 글의 모델 여럿 > 요청 글의 에이전트 수 > 설정의 기본 수
    let asked_agents = intent::requested_agents(text).filter(|_| !ignore_team);
    // 요청 글에서 고른 모델 여럿으로 팀을 꾸리는지(글의 내용으로 적은 팀이면 첫 모델만 답한다)
    let picked_team = if ignore_team { 0 } else { wanted.picks.len() };
    // agents 0: JEV가 정한다. 대화 답을 여럿 합치는 것은 효율이 나쁘므로 파일 작업을 나눌 수 있을 때만 병렬로 돈다.
    let jev_split = if agents == 0 && work && choice.is_none() && !ignore_team { jev_parts(jev) } else { None };
    let setting = if agents == 0 { jev_split.unwrap_or(1) } else { agents };
    let seats: Vec<&intent::RosterMember> = roster.iter().flat_map(|r| r.teams.iter().flatten()).collect();
    let want = if !seats.is_empty() { seats.len() } else if let Some(a) = &ask { a.count } else if picked_team >= 2 { picked_team } else { asked_agents.unwrap_or(if ignore_team { 1 } else { setting as usize }) }.min(intent::MAX_AGENTS);
    let team_from = reasons.len();
    let team = if want < 2 {
        None
    } else {
        let mut list: Vec<TeamAgent> = vec![];
        if !seats.is_empty() {
            list = seats.iter().map(|m| TeamAgent { engine: m.pick.engine, model: m.pick.model.clone() }).collect();
        } else if let Some(a) = &ask {
            list = vec![TeamAgent { engine: a.workers.engine, model: a.workers.model.clone() }; a.count];
        } else if picked_team >= 2 {
            for mp in &wanted.picks {
                list.push(TeamAgent { engine: mp.engine, model: mp.model.clone() });
            }
        } else {
            list.push(TeamAgent { engine: primary, model: models.iter().find(|(e, _)| *e == primary).map(|(_, m)| m.clone()) });
            // 다른 회사의 AI → 다른 엔진 → (모자라면) 같은 엔진을 새 세션으로 더 띄운다. 작업 모드는 도구가 많은 CLI가 먼저다.
            let mut pool: Vec<EngineId> = order.iter().copied().filter(usable).collect();
            // 작업 모드의 빈자리를 사용자가 고르지 않은 API로 채우지 않는다. CLI가 하나도 없을 때만 API로 돈다.
            if work && pool.iter().any(|e| !e.is_api()) {
                pool.retain(|e| !e.is_api());
            }
            for e in &pool {
                if list.len() < want && !list.iter().any(|a| a.engine.vendor() == e.vendor()) {
                    list.push(TeamAgent { engine: *e, model: None });
                }
            }
            for e in &pool {
                if list.len() < want && !list.iter().any(|a| a.engine == *e) {
                    list.push(TeamAgent { engine: *e, model: None });
                }
            }
            for e in pool.iter().cycle().take(want * pool.len().max(1)) {
                if list.len() >= want {
                    break;
                }
                list.push(TeamAgent { engine: *e, model: None });
            }
        }
        // 종합은 쓸 수 있는 AI 중 가장 강한 AI가(같으면 먼저 오는 쪽). 작업 모드의 통합은 되도록 CLI가 맡는다.
        let synth_pool: Vec<EngineId> = order.iter().copied().filter(usable).filter(|e| !work || !e.is_api() || !available.iter().any(|x| !x.is_api())).collect();
        let synth = lead.map(|l| l.engine).or_else(|| synth_pool.iter().copied().enumerate().max_by_key(|(i, e)| (strength(*e), std::cmp::Reverse(*i))).map(|(_, e)| e)).unwrap_or(primary);
        let n = list.len();
        let names = list.iter().map(|a| match &a.model { Some(m) => format!("{}({m})", a.engine.label()), None => a.engine.label().to_string() }).collect::<Vec<_>>().join(", ");
        let synth_label = synth.label();
        if seats.is_empty() && ask.is_none() && asked_agents.is_none() && picked_team < 2 {
            // AI가 정한 수(agents)는 그 이유를 부른 쪽이 붙인다.
            if jev_split.is_some() {
                reasons.push(format!("JEV: {n}개 에이전트로 나눠 동시 진행"));
            }
        }
        if work {
            reasons.push(format!("작업을 하위 작업으로 나누어 {n}개의 에이전트({names})가 별도 복사본에서 동시 작업한 후, 변경 사항을 병합하여 {synth_label}이 최종 검증 및 요약을 진행합니다."));
        } else {
            reasons.push(format!("{n}개 에이전트({names})의 개별 답변을 {synth_label}에서 하나로 종합하여 전달합니다."));
        }
        Some(Team { agents: list, synth, agent_effort: ask.as_ref().and_then(|a| a.effort).map(str::to_string), pinned: ask.is_some() || !seats.is_empty(), groups: roster.iter().flat_map(|r| r.teams.iter().map(Vec::len)).collect(), member_efforts: seats.iter().map(|m| m.effort.map(str::to_string)).collect(), depts: roster.iter().flat_map(|r| r.depts.iter().cloned()).collect() })
    };

    // 회의로 바뀌면 동시 에이전트 이유를 지울 수 있게 따로 둔다.
    let team_reasons: Vec<String> = reasons[team_from..].to_vec();

    // 3) 검토: 스킬 설정 > JEV 난이도 > 규칙 (토론으로 바뀌면 아래에서 검토 이유를 지운다)
    let review_reasons_from = reasons.len();
    let jev_hard = jev.and_then(|j| j.difficulty.filter(|_| j.difficulty_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE));
    let wants_review = team.is_none()
        && match review {
        // 토론 모드: 검토 대신 토론한다
        ReviewMode::Off | ReviewMode::Debate => false,
        ReviewMode::Always => true,
        ReviewMode::Auto => match (skill.and_then(|s| s.review), jev_hard) {
            (Some(v), _) => v,
            (None, Some(d)) => {
                reasons.push(format!("JEV 난이도 {d:.1}/3 → {}", if d >= 2.0 { "검토함" } else { "검토 안 함" }));
                d >= 2.0
            }
            (None, None) => complex(&prompt),
        },
    };
    let reviewer = if wants_review && !work {
        // 되도록 다른 회사의 AI가, 없으면 다른 엔진이, 그것도 없으면 같은 엔진이 새 세션에서 검토한다.
        let all_order = order_for(&kind);
        let other_vendor = all_order.iter().copied().find(|e| installed.contains(e) && e.vendor() != primary.vendor());
        let other_engine = all_order.iter().copied().find(|e| installed.contains(e) && *e != primary);
        let r = other_vendor.or(other_engine).unwrap_or(primary);
        reasons.push(if r != primary { format!("{}가 검토해 마무리합니다", r.label()) } else { format!("다른 AI가 없어 {}가 새 세션에서 검토합니다", r.label()) });
        Some(r)
    } else {
        if work && wants_review {
            reasons.push("작업 모드에서는 파일을 이미 고쳤으므로 검토 단계를 건너뜁니다".into());
        }
        None
    };
    // 4) 토론: 정확도가 중요하면 서로 다른 회사의 두 AI가 따로 답하고 판정 AI가 정리한다(검토 대신).
    let jev_stakes = jev.and_then(|j| j.stakes.filter(|_| j.stakes_confidence.unwrap_or(0.0) >= JEV_MIN_CONFIDENCE));
    let skill_debate = skill.and_then(|s| s.debate);
    let wants_debate = !work
        && team.is_none()
        && match review {
            ReviewMode::Debate => true,
            ReviewMode::Off => false,
            ReviewMode::Always => skill_debate.unwrap_or(false),
            ReviewMode::Auto => skill_debate.unwrap_or_else(|| match jev_stakes {
                Some(s) => s >= 2.0,
                None => needs_accuracy(&prompt),
            }),
        };
    let mut reviewer = reviewer;
    let review_reasons_to = reasons.len();
    let debate = if wants_debate {
        let all_order = order_for(&kind);
        let b = all_order.iter().copied().find(|e| installed.contains(e) && e.vendor() != primary.vendor()).or_else(|| all_order.iter().copied().find(|e| installed.contains(e) && *e != primary));
        match b {
            Some(b) => {
                // 판정은 후보(세 번째 회사 + 두 참가자) 중 가장 강한 AI가, 같으면 세 번째 회사가 맡는다.
                let third = all_order.iter().copied().filter(|e| installed.contains(e) && e.vendor() != primary.vendor() && e.vendor() != b.vendor());
                let judge = third.chain([primary, b]).enumerate().max_by_key(|(i, e)| (strength(*e), std::cmp::Reverse(*i))).map(|(_, e)| e).unwrap_or(primary);
                let tier_light = skill.and_then(|s| s.tier.as_deref()).map(|t| t == "light").unwrap_or_else(|| budget(jev, work).is_some_and(|b| b.tier == "light"));
                if let Some(s) = jev_stakes {
                    reasons.push(format!("JEV: 정확도 중요도 {s:.1}/3 → 토론"));
                }
                reasons.push(format!(
                    "{}와 {}가 따로 답하고{} {}가 판정해 하나로 정리합니다",
                    primary.label(),
                    b.label(),
                    if tier_light { "" } else { " 서로 반론한 뒤" },
                    judge.label()
                ));
                reviewer = None;
                reasons.drain(review_reasons_from..review_reasons_to);
                Some(Debate { a: primary, b, judge, rebuttal: !tier_light })
            }
            None => {
                reasons.push("토론할 다른 AI가 없어 한 AI가 답합니다".into());
                None
            }
        }
    } else {
        None
    };
    let skill_tier = skill.and_then(|s| s.tier.clone());
    let skill = skill.map(|s| {
        let dir = std::path::Path::new(&s.path).parent().filter(|d| d.join("references").is_dir()).map(|d| d.to_string_lossy().to_string());
        (SkillRef { id: s.id.clone(), name: s.name.clone(), how: if catalog.explicit(text).is_some() { "explicit" } else { "auto" }, dir }, skill_body(s))
    });
    let budget = match skill_tier.as_deref().and_then(budget_for) {
        Some(b) => {
            reasons.push(format!("스킬이 노력 단계 '{}'를 정했습니다", tier_label(b.tier)));
            Some(b)
        }
        None => budget(jev, work),
    };
    if let Some(b) = &budget {
        reasons.push(format!(
            "노력 단계 '{}': 노력치 {}, Claude Code {}, API 출력 최대 {}토큰",
            tier_label(b.tier),
            b.effort,
            b.claude_model.unwrap_or("기본 모델"),
            b.max_tokens
        ));
    }
    Ok(Plan { primary, fallbacks, reviewer, skill, prompt, work, reasons, jev: jev.cloned(), budget, debate, models, team, council: None, team_reasons, free_model: None })
}

/// How to run one request, chosen in the work setup panel before sending. Empty fields follow the settings and the
/// usual automatic choices. Saved with the user message so "continue" runs the same way.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkSetup {
    /// The team as built in the panel (work mode only)
    pub team: Option<SetupTeam>,
    /// Number of agents when no team is built (0: automatic)
    pub agents: u8,
    /// Effort tier: light · standard · deep
    pub tier: Option<String>,
    pub review: Option<ReviewMode>,
    pub lead_commit: Option<bool>,
    /// false: OpenCode does not assign AIs and effort per sub-task
    pub roadmap: Option<bool>,
    /// false: no JEV for this request (routing, memory filtering, command checks)
    pub jev: Option<bool>,
    /// Turns each worker may take before it has to stop
    pub max_turns: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupTeam {
    pub lead: Option<SetupSeat>,
    pub groups: Vec<SetupGroup>,
}

/// One team. dept: the departments above it ("개발/프론트" is two levels), empty when not grouped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupGroup {
    pub dept: String,
    pub members: Vec<SetupSeat>,
}

/// model: a family name from intent::model_choices ("Claude Opus"); count: same seat repeated.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SetupSeat {
    pub model: String,
    pub effort: Option<String>,
    pub count: u8,
}

/// Worker turns allowed in the panel
pub const SETUP_TURNS: std::ops::RangeInclusive<u32> = 5..=200;

impl SetupTeam {
    pub fn roster(&self, installed: &[EngineId], work: bool) -> Result<intent::Roster, String> {
        let missing = |n: String| SETUP_MISSING.replace("{model}", &n);
        let pick = |seat: &SetupSeat| intent::pick_named(&seat.model, installed, work).map_err(missing);
        let (mut teams, mut depts) = (vec![], vec![]);
        for g in self.groups.iter().filter(|g| !g.members.is_empty()) {
            let mut members = vec![];
            for seat in &g.members {
                let m = intent::RosterMember { pick: pick(seat)?, effort: seat.effort.as_deref().and_then(intent::effort_of) };
                members.extend(std::iter::repeat_n(m, usize::from(seat.count.max(1))));
            }
            teams.push(members);
            depts.push(g.dept.split('/').map(str::trim).filter(|d| !d.is_empty()).map(str::to_string).take(teamwork::MAX_ORG_DEPTH).collect());
        }
        let seats: usize = teams.iter().map(Vec::len).sum();
        if seats < 2 {
            return Err(SETUP_TOO_FEW.into());
        }
        if seats > intent::MAX_AGENTS {
            return Err(SETUP_TOO_MANY.replace("{n}", &seats.to_string()).replace("{max}", &intent::MAX_AGENTS.to_string()));
        }
        let lead = self.lead.as_ref().filter(|l| !l.model.trim().is_empty()).map(pick).transpose()?;
        Ok(intent::Roster { teams, depts, lead })
    }
}

const SETUP_MISSING: &str = "{model} 모델을 실행할 수 없으므로, 모델을 설치하거나 작업 설정에서 다른 모델을 선택해 주세요.";
const SETUP_TOO_FEW: &str = "직접 구성한 팀에는 최소 2명 이상의 워커가 필요합니다.";
const SETUP_TOO_MANY: &str = "워커 수({n}명)가 너무 많으니 최대 {max}명 이하로 줄여 주세요.";
const SETUP_CHAT_TEAM: &str = "직접 구성한 팀은 작업 모드 또는 만들기에서만 적용되므로 사용되지 않았습니다.";
const SETUP_AGENTS: &str = "작업 설정에 따라 에이전트 {n}개가 실행됩니다.";
const SETUP_TIER: &str = "작업 설정에 따라 {tier} 노력 등급이 적용됩니다.";

/// 요청 글에 적힌 팀을 어떻게 읽을지
#[derive(Debug, Clone, PartialEq)]
pub enum TeamText {
    /// 규칙으로 읽는다
    Parse,
    /// Built in the work setup panel: used as is, the request text is not read for a team
    Given(intent::Roster),
    /// AI가 뽑은 구성표를 읽는다(규칙이 못 읽은 글)
    Lines(String),
    /// 글의 내용으로 적은 팀이라 쓰지 않는다
    Ignore,
}

/// 여러 AI를 돌리는 일은 Elysium이 한다. AI가 셸로 다른 AI를 띄우면 시간 제한·중지·비용 집계에서 벗어난다.
const NO_NESTED_AI: &str = "다른 AI는 Elysium이 직접 실행합니다. 명령 도구로 AI CLI(agy, gemini, codex, opencode, claude, just-code-claude, agy_call)를 실행하거나 백그라운드 워커를 띄우지 마세요. 요청이 하위 에이전트·워커를 말해도 지금 당신에게 그 일이 맡겨졌다면 Elysium이 한 AI로 돌리기로 한 것이니, 직접 처리하고 답에 그렇게 했다고 밝히세요.\n";

pub fn system_prompt(work: bool, workspace: &str, skill: Option<&(SkillRef, String)>) -> String {
    let prefs = crate::settings::prefs();
    let mut s = format!(
        "당신은 데스크톱 AI 클라이언트 Elysium 안에서 일하는 AI입니다. {}\n\
         결과는 바로 쓸 수 있게 완성된 형태로 주고, 필요하면 마크다운 제목·목록·코드 블록으로 정리하세요. 모르는 것은 추측하지 말고 모른다고 밝히세요.\n",
        prefs.lang_line()
    );
    s.push_str(&format!("작업 폴더: {workspace}\n"));
    if work {
        s.push_str("작업 모드: 사용자가 이 작업 폴더 안의 파일 수정과 PowerShell 명령 실행을 허락했습니다(명령 도구가 있는 AI만). 요청한 변경만 하고, 명령은 작업 폴더 안에서만 쓰세요. 끝나면 바꾼 파일과 내용을 짧게 요약하세요.\n");
        s.push_str(LEAN_SYSTEM);
        s.push_str(OWNERSHIP_SYSTEM.trim());
        s.push('\n');
    } else {
        s.push_str("대화 모드: 파일을 읽을 수는 있지만 고치거나 명령을 실행하지 마세요. 파일을 바꿔야 하면 바꿀 내용을 보여 주고 '만들기' 버튼으로 다시 요청하라고 안내하세요.\n");
    }
    s.push_str(NO_NESTED_AI);
    s.push_str(ATTACH_SYSTEM);
    if let Some((r, body)) = skill {
        s.push_str(&format!("\n[스킬: {}]\n{}\n", r.name, body));
    }
    s.push_str(&prefs.tail());
    s
}

/// 작업 모드에서 대화가 불어나지 않게 하는 규칙. 도구 결과는 이후 모든 단계에서 다시 읽히므로 큰 출력 한 번이 수백만 토큰이 된다.
pub const LEAN_SYSTEM: &str = "[토큰 아끼기] 도구 결과는 이후 단계마다 다시 읽히므로 작게 가져오세요.\n\
- 폴더 전체를 재귀로 나열하지 마세요(Get-ChildItem -Recurse, tree, dir /s 금지). 파일은 Glob, 내용은 Grep으로 찾으세요.\n\
- 큰 파일은 필요한 부분만 읽으세요(Read의 offset·limit, Grep으로 줄 번호를 먼저 찾기). 이미 읽은 파일을 다시 통째로 읽지 마세요.\n\
- 명령 출력은 줄이세요: | Select-Object -First 40 또는 -Last 40, 빌드·테스트는 조용한 옵션(-q, --quiet, --console=plain)과 실패 부분만.\n";

/// Every AI that works on files owns its assignment: no handing it off, and unfinished work is listed under lead::LEFT_MARK
/// so Elysium can give it to someone (leadrun::run_worker, teamwork::integrate_prompt).
pub const OWNERSHIP_SYSTEM: &str = "[책임]
- 부여받은 작업은 스스로 끝까지 완수하세요.
- 다른 작업자, 총괄, 사용자에게 작업을 넘기거나 나중으로 미루지 말고 TODO, 스텁, 생략을 남기지 마세요.
- 보유한 도구로 실행, 빌드, 테스트, 확인이 가능한 일은 사용자에게 요청하지 말고 직접 수행하세요.
- 완료를 보고하기 전에 실제로 정상 동작하는지 반드시 검증하세요.
- 권한이나 계정 부재, 사람만 할 수 있는 작업, 범위 외 항목 등 불가피하게 불가능한 경우에만 나머지를 계속 진행하고, 답변 맨 끝에 [남은 일] 표기를 적은 후 미완료 항목과 사유를 명시하세요.
- 모든 작업이 완료되었을 때는 [남은 일] 표기를 절대 작성하지 마세요.";

/// Wait before the one retry of a failed integration run.
const INTEGRATE_RETRY_WAIT: Duration = Duration::from_secs(30);
/// Lead log line when every worker failed and the integrator takes over.
const ALL_FAILED_TAKEOVER: &str = "모든 작업자가 실패하여 통합 AI가 중단 없이 전체 작업을 직접 수행합니다.";
/// Lead log line when an integration run failed and is tried again (the error follows in parentheses).
const INTEGRATE_FAILED_NOTE: &str = "통합 단계가 실패하여 현재 폴더 상태를 이어받아 30초 후 1회 자동 재시도합니다";
/// Put in front of the integration prompt with window test results the integrator has not seen.
const TESTS_FOR_INTEGRATOR: &str = "[창 테스트 결과] 작업 중 큐를 통해 실행된 창 테스트 결과입니다. 이미 수정되었을 수 있으니 먼저 확인한 후, 아직 해결되지 않은 실패 항목은 남겨두지 말고 지금 모두 찾아 해결하세요.";
/// Heading for window test results added to the final answer.
const TESTS_FOR_REPORT: &str = "### 창 테스트 결과";

/// Adds the window test results (status and command line of each) to the final answer.
fn tests_tail(text: &mut String, results: &[String]) {
    if results.is_empty() {
        return;
    }
    let lines: String = results.iter().map(|r| format!("- {}\n", cut(r.lines().next().unwrap_or("").trim(), 300))).collect();
    text.push_str(&format!("\n\n{}\n{lines}", TESTS_FOR_REPORT.trim()));
}

/// Put in front of the integration prompt on the retry.
const INTEGRATE_RETRY: &str = "[다시 시도] 이전 통합 시도 중 오류가 발생했습니다. 폴더에 일부 작업물이 이미 반영되어 있을 수 있으니 처음부터 다시 시작하지 말고 현재 상태를 먼저 확인한 뒤 남은 작업만 완료하세요.";

/// 에이전트가 답에 파일을 붙이는 방법. attachments::extract가 답에서 표시를 떼어 첨부로 만든다.
pub const ATTACH_SYSTEM: &str = "\n[파일 전달] 사용자에게 파일을 건네려면 답에 아래 표시를 한 줄씩 쓰세요. 표시는 화면에서 첨부 파일로 바뀌므로 표시나 이 방법을 답에서 설명하지 마세요. 요청한 결과물이 파일일 때만 쓰세요.\n\
- 작업 폴더의 파일: [[attach: 작업 폴더 기준 경로]]\n\
- 글 파일을 새로 만들어 건네기(파일을 고칠 수 없는 대화 모드에서도 됩니다):\n[[file: 파일이름.확장자]]\n내용\n[[/file]]\n";

/// '만들기': 버튼 한 번으로 폴더 안에서 결과물을 끝까지 완성한다(작업 모드 지시 뒤에 붙는다).
pub const BUILD_SYSTEM: &str = "\n[만들기] 사용자가 '만들기'를 눌러 이 작업 폴더 안에서 요청을 끝까지 완성해 달라고 했습니다. 파일을 만들고 고치는 것은 이미 허락받았습니다.\n\
- 되묻지 말고 합리적으로 가정해 끝까지 진행하세요. 가정한 것은 마지막 요약에 적으세요.\n\
- 폴더에 기존 프로젝트가 있으면 그 구조와 스타일을 따르고, 비어 있으면 실행할 수 있는 프로젝트를 처음부터 만드세요.\n\
- TODO·생략·자리만 잡은 코드를 남기지 말고 바로 쓸 수 있는 완성본을 만드세요. 새 프로젝트면 README에 설치·실행 방법을 적으세요.\n\
- PowerShell 명령을 쓸 수 있으면 의존성 설치·빌드·테스트를 직접 돌려 실제로 동작하는지 확인하고 오류를 고치세요. 작업 폴더 밖을 지우거나 바꾸는 명령은 쓰지 마세요. 명령을 쓸 수 없으면 사용자가 그대로 따라 할 수 있게 적으세요.\n\
- 끝나면 만든·바꾼 파일, 실행 방법, 가정을 짧게 요약하세요.\n";

/// 토론 라운드 시간 한도: 한 AI가 너무 오래 걸리면 기다리지 않고 들어온 답으로 진행한다.
const DEBATE_ROUND1: Duration = Duration::from_secs(180);
const DEBATE_REBUTTAL: Duration = Duration::from_secs(120);

pub const DEBATE_SYSTEM: &str = "\n[토론] 정확도가 중요한 질문입니다. 다른 AI도 같은 질문에 따로 답하고 서로의 답을 검토합니다. 근거와 계산 과정을 짧게 보이고, 확실하지 않은 부분은 '확실하지 않음'이라고 표시하세요. 모르는 것을 지어내지 마세요.\n";

pub fn rebuttal_prompt(request: &str, own: &str, other_label: &str, other: &str) -> String {
    format!(
        "[질문]\n{request}\n\n[당신의 1차 답]\n{own}\n\n[다른 AI({other_label})의 답]\n{other}\n\n\
         다른 AI의 답에서 틀린 점, 빠진 점, 근거가 약한 점을 짚고, 당신의 답에서도 고칠 점이 있으면 인정하세요. 그 뒤 고친 최종 답을 쓰세요.\n\
         형식: '## 반론'(짧은 불릿) 다음에 '## 고친 답'(완성된 답)."
    )
}

/// 반론 라운드 출력에서 '## 고친 답' 부분만(없으면 전체)
pub fn revised_part(text: &str) -> &str {
    match text.find("## 고친 답") {
        Some(i) => text[i + "## 고친 답".len()..].trim(),
        None => text.trim(),
    }
}

pub fn judge_prompt(request: &str, a_label: &str, a: &str, b_label: &str, b: &str, rebuttals: Option<(&str, &str)>) -> String {
    let mut s = format!("[질문]\n{request}\n\n[답 1: {a_label}]\n{a}\n\n[답 2: {b_label}]\n{b}\n\n");
    if let Some((ra, rb)) = rebuttals {
        let cut = |t: &str| -> String { t.split("## 고친 답").next().unwrap_or("").trim().chars().take(1500).collect() };
        s.push_str(&format!("[{a_label}의 반론]\n{}\n\n[{b_label}의 반론]\n{}\n\n", cut(ra), cut(rb)));
    }
    s.push_str(
        "두 AI가 같은 질문에 따로 답하고 토론한 결과입니다. 판정자로서 사실, 숫자, 계산, 논리를 하나씩 확인해 맞는 쪽을 따르고, 둘 다 틀렸으면 바로잡아 하나의 완성된 최종 답을 쓰세요.\n\
         두 답이 갈린 핵심 쟁점이 있었다면 답 끝에 '### 토론에서 갈린 점' 짧은 목록으로 무엇이 왜 맞는지 적으세요(갈린 점이 없으면 생략). 확실하지 않은 부분은 그렇다고 밝히세요.\n\
         '답 1은', 'A는' 같은 토론 과정 서술로 시작하지 말고 바로 답하세요.",
    );
    s
}

/// 동시 에이전트에게 붙이는 지시
pub fn team_system(n: usize) -> String {
    format!("\n[동시 에이전트] 이 요청은 AI 에이전트 {n}개가 동시에 따로 답하고, 종합 AI가 각 답의 좋은 점을 모아 하나로 정리합니다. 다른 에이전트를 기다리지 말고 혼자서 완성된 답을 쓰세요. 확실하지 않은 부분은 '확실하지 않음'이라고 표시하고, 모르는 것을 지어내지 마세요.\n")
}

/// 종합 AI에게 줄 요청. answers: (에이전트 이름, 답)
pub fn team_prompt(request: &str, answers: &[(String, &str)]) -> String {
    let mut s = format!("[질문]\n{request}\n\n");
    for (i, (who, text)) in answers.iter().enumerate() {
        s.push_str(&format!("[에이전트 {}: {who}]\n{text}\n\n", i + 1));
    }
    s.push_str(
        "여러 AI 에이전트가 같은 질문에 동시에 따로 답했습니다. 종합자로서 각 답의 맞는 내용과 좋은 점을 모으고, 서로 다르거나 틀린 부분은 사실·숫자·논리를 확인해 바로잡아 하나의 완성된 최종 답을 쓰세요.\n\
         답들이 크게 갈린 점이 있었다면 답 끝에 '### 에이전트 사이에 갈린 점' 짧은 목록으로 무엇이 왜 맞는지 적으세요(없으면 생략).\n\
         '에이전트 1은' 같은 과정 서술로 시작하지 말고 바로 답하세요.",
    );
    s
}

// ---------------------------------------------------------------------------
// 회의: 여러 AI(되도록 CLI)가 라운드마다 서로의 발언을 읽고 대화하며 푼다.

/// 회의 한 라운드에서 한 AI가 기다리는 최대 시간
const COUNCIL_TURN: Duration = Duration::from_secs(240);
/// 다른 참가자에게 전하는 발언 하나의 최대 글자 수(토큰 절약)
const COUNCIL_MSG_CHARS: usize = 2500;
pub const COUNCIL_MAX_AGENTS: usize = 4;
/// 발언 끝의 합의 표시(JEV가 없을 때 회의를 끝낼지 정한다)
const AGREE_MARK: &str = "합의:";

/// 회의 참가자와 라운드 수를 정한다. 다른 회사의 CLI → 다른 CLI → API → (모자라면) 같은 AI를 새 세션으로.
pub fn council_plan(p: &Plan, installed: &[EngineId], want: usize) -> Option<Council> {
    let want = want.clamp(2, COUNCIL_MAX_AGENTS);
    let mut pool: Vec<EngineId> = order_for("general").into_iter().filter(|e| installed.contains(e)).collect();
    // 회의는 CLI 창끼리의 대화가 중심이다. 파일·웹 도구도 CLI에만 있다.
    pool.sort_by_key(|e| e.is_api());
    if pool.is_empty() {
        return None;
    }
    let mut list: Vec<TeamAgent> = vec![];
    let model = |e: EngineId| p.models.iter().find(|(x, _)| *x == e).map(|(_, m)| m.clone());
    for e in &pool {
        if list.len() < want && !list.iter().any(|a| a.engine.vendor() == e.vendor()) {
            list.push(TeamAgent { engine: *e, model: model(*e) });
        }
    }
    for e in &pool {
        if list.len() < want && !list.iter().any(|a| a.engine == *e) {
            list.push(TeamAgent { engine: *e, model: model(*e) });
        }
    }
    for e in pool.iter().cycle().take(want * pool.len()) {
        if list.len() >= want {
            break;
        }
        list.push(TeamAgent { engine: *e, model: model(*e) });
    }
    let synth = pool.iter().copied().enumerate().max_by_key(|(i, e)| (strength(*e), std::cmp::Reverse(*i))).map(|(_, e)| e)?;
    let rounds = if p.budget.as_ref().is_some_and(|b| b.tier == "light") { 2 } else { 3 };
    Some(Council { agents: list, rounds, synth })
}

/// 회의에서 부르는 이름: "참가자 2 · Codex" / "참가자 1 · Claude Code(opus)"
pub fn council_name(i: usize, a: &TeamAgent) -> String {
    match &a.model {
        Some(m) => format!("참가자 {} · {}({m})", i + 1, a.engine.label()),
        None => format!("참가자 {} · {}", i + 1, a.engine.label()),
    }
}

pub fn council_system(me: &str, others: &[String]) -> String {
    format!(
        "\n[회의] 이 요청은 AI 여러 명이 여러 라운드 동안 서로의 발언을 읽고 대화하며 함께 푸는 회의입니다. 당신은 '{me}'입니다. 다른 참가자: {}.\n\
         - 한 번에 핵심만 밀도 있게 말하세요(600단어 이내). 근거·계산·코드를 보이고, 다른 참가자가 틀렸으면 분명히 짚고, 설득되면 인정하세요.\n\
         - 다른 참가자의 말을 되풀이하지 말고 새로 보탤 것만 쓰세요. 모르는 것을 지어내지 마세요.\n\
         - 발언 앞에 자기 이름·라운드 같은 머리말을 붙이지 말고 바로 본론부터 쓰세요.\n",
        others.join(", ")
    )
}

/// 1라운드: 다른 참가자의 의견을 보기 전에 각자 해결안을 낸다.
pub fn council_open(request: &str) -> String {
    format!("[요청]\n{request}\n\n1라운드입니다. 다른 참가자의 의견을 보기 전에 당신의 해결안을 제시하세요.")
}

fn council_reply_rule(round: u8) -> String {
    format!(
        "{round}라운드입니다. 위 발언에 답하세요: 동의하는 점과 틀렸거나 빠진 점을 짚고, 당신의 해결안을 고쳐 다시 제시하세요.\n\
         마지막 줄에는 반드시 '{AGREE_MARK} 예'(모든 참가자의 결론이 사실상 같다고 볼 때) 또는 '{AGREE_MARK} 아니오' 중 하나만 쓰세요."
    )
}

/// 2라운드부터, 자기 세션을 이어 쓰는 AI에게: 마지막으로 말한 뒤 나온 다른 참가자 발언만 보낸다(이미 본 내용은 다시 보내지 않는다). others: ("2라운드 · 참가자 2 · Codex", 발언)
pub fn council_followup(round: u8, others: &[(String, &str)]) -> String {
    let mut s = String::from("[당신이 마지막으로 말한 뒤 다른 참가자의 발언]\n");
    for (who, text) in others {
        s.push_str(&format!("[{who}]\n{}\n\n", cut(strip_agree(text), COUNCIL_MSG_CHARS)));
    }
    s.push_str(&council_reply_rule(round));
    s
}

/// 2라운드부터, 세션을 이어 쓰지 못하는 AI에게: 요청과 지금까지의 회의 기록 전체.
pub fn council_full(request: &str, round: u8, me: &str, log: &[(u8, String, &str)]) -> String {
    let mut s = format!("[요청]\n{request}\n\n[지금까지의 회의]\n");
    for (r, who, text) in log {
        let mine = if who == me { "(당신)" } else { "" };
        s.push_str(&format!("[{r}라운드 · {who}{mine}]\n{}\n\n", cut(strip_agree(text), COUNCIL_MSG_CHARS)));
    }
    s.push_str(&council_reply_rule(round));
    s
}

/// 합의하지 못하고 끝났을 때 정리 AI에게: 마지막 라운드는 전부, 그 전 라운드는 줄여서 보낸다.
pub fn council_synth(request: &str, last_round: u8, log: &[(u8, String, &str)]) -> String {
    let mut s = format!("[요청]\n{request}\n\n[회의 기록]\n");
    for (r, who, text) in log {
        let n = if *r == last_round { JEV_ANSWER_CHARS } else { 1200 };
        s.push_str(&format!("[{r}라운드 · {who}]\n{}\n\n", cut(strip_agree(text), n)));
    }
    s.push_str(
        "여러 AI가 회의하며 이 요청을 함께 풀었습니다. 마지막 라운드의 의견을 중심으로 맞는 내용과 근거를 모으고, 남은 이견은 사실·숫자·논리를 확인해 바로잡아 하나의 완성된 최종 답을 쓰세요.\n\
         끝까지 갈린 점이 있으면 답 끝에 '### 회의에서 갈린 점' 짧은 목록으로 무엇이 왜 맞는지 적으세요(없으면 생략). 회의 과정 서술로 시작하지 말고 바로 답하세요.",
    );
    s
}

/// 발언 끝의 '합의: 예/아니오' 줄을 뗀다.
pub fn strip_agree(text: &str) -> &str {
    let t = text.trim_end();
    match t.rfind('\n') {
        Some(i) if t[i + 1..].trim_start().trim_start_matches(['*', '#', ' ']).starts_with(AGREE_MARK) => t[..i].trim_end(),
        None if t.trim_start_matches(['*', '#', ' ']).starts_with(AGREE_MARK) => "",
        _ => t,
    }
}

/// 마지막 줄이 '합의: 예'인지
pub fn says_agree(text: &str) -> bool {
    let last = text.trim_end().rsplit('\n').next().unwrap_or("").trim().trim_matches(['*', '#', ' ']);
    last.strip_prefix(AGREE_MARK).is_some_and(|r| r.trim().trim_matches(['*', '.']).starts_with('예'))
}

/// evidence: 초안을 쓴 AI가 쓴 도구(읽은 파일·검색 등). 검토자가 볼 수 없는 자료에 근거한 내용을 지우지 않게 알려 준다.
pub fn review_prompt(request: &str, draft: &str, evidence: &[String]) -> String {
    let mut s = format!(
        "아래 [요청]에 대해 다른 AI가 쓴 [초안]이 있습니다. 검토자로서 사실 오류, 빠진 내용, 모호하거나 실행할 수 없는 부분을 바로잡아 완성된 최종 답만 출력하세요.\n\
         - 초안이 이미 충분히 좋으면 거의 그대로 두세요. '초안을 고쳤습니다' 같은 검토 과정 설명은 쓰지 마세요.\n\
         - 초안은 당신이 볼 수 없는 파일·검색·실행 결과에 근거했을 수 있습니다. 확인할 수 없다는 이유만으로 내용을 지우거나 '모름'으로 바꾸지 말고, 분명히 틀린 부분만 고치세요.\n\
         - 초안의 [[attach: …]]·[[file: …]] 표시는 고치지 말고 그대로 두세요.\n\n[요청]\n{request}\n\n[초안]\n{draft}"
    );
    if !evidence.is_empty() {
        s.push_str("\n\n[초안을 쓴 AI가 확인한 자료]\n");
        for e in evidence.iter().take(20) {
            s.push_str(&format!("- {e}\n"));
        }
    }
    s
}

/// 검토 답이 초안을 망가뜨린 것 같으면(지나치게 짧거나 '모름'만) 버리고 초안을 쓴다.
pub fn review_is_degraded(draft: &str, review: &str) -> bool {
    let (d, r) = (draft.trim().chars().count(), review.trim().chars().count());
    let shrug = ["모름", "모르겠", "알 수 없", "확인할 수 없", "정보가 없"];
    d >= 40 && (r * 5 < d || (r < 40 && shrug.iter().any(|w| review.contains(w))))
}

// ---------------------------------------------------------------------------
// 실행

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub conversation_id: String,
    pub task_id: String,
    pub user_message_id: String,
}

/// 화면에 알리고, 원격 제어로 붙은 폰에도 같은 이벤트를 흘린다.
pub fn emit(app: &AppHandle, name: &str, payload: Value) {
    crate::remote::publish(name, &payload);
    let _ = app.emit(name, payload);
}

/// 작업 모드 허락/거절(PC 화면과 폰이 같이 쓴다). actor는 감사 기록에 남는다("user" 또는 "remote:기기").
pub fn decide_approval(app: &AppHandle, st: &AppState, approval_id: &str, approve: bool, actor: &str) -> crate::error::AppResult<store::Approval> {
    let a = store::decide_approval(&st.db, approval_id, approve)?;
    if let Some(tx) = st.approvals.lock().unwrap_or_else(|p| p.into_inner()).remove(approval_id) {
        let _ = tx.send(approve);
    }
    crate::audit::record(&st.db, actor, "approval.decided", if approve { "approved" } else { "rejected" }, json!({ "engine": a.engine }));
    emit(app, "approval:decided", json!(a));
    Ok(a)
}

/// build: '만들기' 버튼. 작업 모드로 돌고, 누른 것이 곧 허락이라 승인을 묻지 않는다.
/// council: '회의' 버튼. 여러 AI가 서로 대화하며 푼다(대화 모드에서만).
#[allow(clippy::too_many_arguments)]
pub fn send(app: &AppHandle, st: &std::sync::Arc<AppState>, conversation_id: Option<String>, text: String, choice: Option<EngineId>, work: bool, build: bool, workspace: Option<String>, council: bool, attachment_ids: Vec<String>, setup: WorkSetup) -> Result<SendResult, String> {
    if attachment_ids.len() > attachments::MAX_PER_MESSAGE {
        return Err("메시지당 파일은 최대 10개까지 첨부할 수 있습니다.".to_string());
    }
    let files: Vec<Attachment> = attachment_ids.iter().map(|id| attachments::load(&st.data_dir, id).ok_or_else(|| "첨부 파일을 찾을 수 없으니 다시 첨부해 주세요.".to_string())).collect::<Result<_, _>>()?;
    let text = text.trim().to_string();
    let text = if text.is_empty() && !files.is_empty() { "첨부된 파일을 확인해 주세요.".to_string() } else { text };
    if text.is_empty() {
        return Err("보낼 내용이 없습니다".into());
    }
    // 회의는 여러 AI가 같은 폴더를 동시에 고치지 않도록 대화 모드로만 돈다.
    let council = council && !build;
    let work = !council && (work || build);
    let conv = match conversation_id {
        Some(c) if store::conversation_exists(&st.db, &c).map_err(|e| e.to_string())? => c,
        // 새 대화는 입력창에서 고른 폴더(없으면 기본 작업 폴더)로 시작한다.
        // 폴더를 고르지 않고 '만들기'로 시작하면 기본 작업 폴더 안에 새 프로젝트 폴더를 만들어 다른 결과물과 섞이지 않게 한다.
        _ => {
            let workspace = match workspace {
                None if build => Some(new_project_dir(&st.settings().workspace())?),
                w => w,
            };
            store::create_conversation(&st.db, &text, workspace.as_deref()).map_err(|e| e.to_string())?
        }
    };
    let user_message_id = store::insert_message(&st.db, &conv, "user", &text, Some(&json!({ "work": work, "build": build, "askedCouncil": council, "engine": choice.map(|c| c.as_str()), "attachments": files, "setup": (setup != WorkSetup::default()).then_some(&setup) }))).map_err(|e| e.to_string())?;
    let task_id = new_id();
    crate::afterwork::note(&task_id, &text);
    // Sync: every open screen (PC window, paired phones) reloads so a question sent from another device shows up at once.
    emit(app, "message:created", json!({ "conversationId": conv, "messageId": user_message_id, "taskId": task_id, "role": "user" }));
    let cancel = CancellationToken::new();
    st.tasks.lock().unwrap_or_else(|p| p.into_inner()).insert(task_id.clone(), cancel.clone());
    st.steers.lock().unwrap_or_else(|p| p.into_inner()).insert(task_id.clone(), Steer::default());
    // Kept until the answer is saved; if the app closes first, the next start offers to continue.
    let keeper = crate::resume::begin(st, &task_id, &conv, &user_message_id);
    let (app2, st2, conv2, task2) = (app.clone(), st.clone(), conv.clone(), task_id.clone());
    tauri::async_runtime::spawn(async move {
        let since = std::time::SystemTime::now() - Duration::from_secs(2);
        run_task(&app2, &st2, &conv2, &task2, &text, choice, work, build, council, &setup, cancel.clone()).await;
        crate::resume::end(&st2, &task2, keeper);
        if work && !cancel.is_cancelled() {
            attach_deliverables(&app2, &st2, &conv2, &task2, &text, since).await;
        }
        st2.tasks.lock().unwrap_or_else(|p| p.into_inner()).remove(&task2);
        st2.steers.lock().unwrap_or_else(|p| p.into_inner()).remove(&task2);
        let prefix = format!("{task2}/");
        st2.agent_runs.lock().unwrap_or_else(|p| p.into_inner()).retain(|k, _| !k.starts_with(&prefix));
        emit(&app2, "task:done", json!({ "taskId": task2, "conversationId": conv2 }));
        refresh_memory(&app2, &st2, &conv2).await;
        crate::afterwork::after_task(&app2, &st2, &task2, &conv2, cancel.is_cancelled()).await;
    });
    Ok(SendResult { conversation_id: conv, task_id, user_message_id })
}

#[derive(Debug, Clone, Deserialize)]
pub struct SideTurn {
    pub role: String,
    pub text: String,
}

/// 곁질문에 다시 보내는 앞선 곁질문 차례 수·글자 수, 진행 상황 글자 수
const SIDE_TURNS: usize = 6;
const SIDE_TURN_CHARS: usize = 700;
const SIDE_PROGRESS_CHARS: usize = 16000;
const SIDE_TIMEOUT: Duration = Duration::from_secs(300);

const SIDE_SYSTEM: &str = "\n[곁질문] 사용자가 진행 중인 작업과 별개로 짧게 묻는 곁질문입니다. 이 문답은 대화 기록에 남지 않고 다른 AI에게도 전해지지 않습니다.\n\
- 작업의 진행 상황이나 프로젝트에 대한 질문에 간결하게 답하세요. 프로젝트 내용은 필요할 때만 파일을 읽어 확인하세요.\n\
- 파일을 고치거나 명령을 실행하지 말고, 진행 중인 작업을 이어받아 하지도 마세요.\n\
- [지금 진행 상황]에 없는 것은 추측하지 말고 모른다고 말하세요.\n";

fn side_prompt(question: &str, progress: Option<&str>, thread: &[SideTurn]) -> String {
    let mut s = String::new();
    match progress.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => s.push_str(&format!("[지금 진행 상황]\n{}\n\n", cut(p, SIDE_PROGRESS_CHARS))),
        None => s.push_str("[지금 진행 상황]\n이 세션에서 실행 중인 작업이 없습니다.\n\n"),
    }
    let recent = &thread[thread.len().saturating_sub(SIDE_TURNS)..];
    if !recent.is_empty() {
        s.push_str("[앞선 곁질문]\n");
        for t in recent {
            s.push_str(&format!("{}: {}\n", if t.role == "user" { "사용자" } else { "AI" }, cut(&t.text, SIDE_TURN_CHARS)));
        }
        s.push('\n');
    }
    s.push_str(&format!("[곁질문]\n{question}"));
    s
}

/// Marks the lead's side-question summary run: Claude Code gets no tools for it (engines::args).
pub const SIDE_LEAD_SYSTEM: &str = "\n[곁질문 총괄] 당신은 이 작업의 총괄입니다. 사용자가 진행 중인 작업과 별개로 곁질문을 했고, 실행 중인 워커마다 /btw로 같은 질문을 해 답을 받았습니다. 답을 받지 못한 워커는 화면에 남은 실행 기록을 대신 붙였습니다.\n\
- 프로젝트 파일을 읽거나 고치지 말고, 명령을 실행하거나 워커에게 지시하지도 마세요. 아래 자료만으로 답하세요.\n\
- 먼저 전체 진행 정도를 요약하세요(워커마다 한 줄: 끝낸 것 · 하는 중 · 남은 것). 이어서 사용자의 질문에 답하세요.\n\
- 워커의 답과 기록에 없는 것은 추측하지 말고 모른다고 하세요. 워커끼리 말이 다르면 그대로 알려 주세요.\n";
/// Workers asked at once, how long each may take, and how much of each answer or record the lead reads
const SIDE_MAX_FORKS: usize = 8;
const SIDE_FORK_TIMEOUT: Duration = Duration::from_secs(180);
const SIDE_ANSWER_CHARS: usize = 3000;
const SIDE_RECORD_TAIL: usize = 1500;

const SIDE_ASKING: &str = "에이전트 응답 수집 중 ({done}/{total})";
const SIDE_SUMMING: &str = "에이전트 응답 요약 중";

/// What a forked worker is asked: its progress first, then the user's question, from its own context only.
fn btw_prompt(question: &str) -> String {
    format!(
        // Must not start with "/": headless Claude Code would read it as the /btw command and refuse.
        "[곁질문 /btw] 사용자가 작업과 별개로 묻습니다. 이 문답은 작업 기록에 남지 않습니다.\n\
         - 도구를 쓰지 말고 지금까지의 맥락만으로 답하세요. 하던 작업을 이어 하거나 계획을 세우지 마세요.\n\
         - 첫 줄은 '진행:'으로 시작해 맡은 일의 진행 정도를 쓰세요(끝낸 것 · 하는 중 · 남은 것, 대략 몇 %).\n\
         - 이어서 아래 질문에 짧게 답하세요. 모르는 것은 모른다고 쓰세요.\n\n[질문]\n{question}"
    )
}

/// One agent the side question goes to.
struct SideAsk {
    key: String,
    title: String,
    label: String,
    /// The worker's run with its session to fork, when it can be asked
    fork: Option<RunSpec>,
    /// What the screen shows of it (used when it cannot be asked or does not answer)
    record: String,
    /// ask (waiting) · ok (answered) · fail (no answer: record used) · log (cannot be asked: record used) · done (already finished)
    state: &'static str,
    answer: String,
}

fn agent_record(a: &crate::livesnap::AgentSnap) -> String {
    let state = match (a.state.as_str(), a.wait.as_deref()) {
        ("run", Some(w)) => format!("대기({w})"),
        ("run", None) => "작업 중".into(),
        ("ok", _) => "완료".into(),
        _ if a.stopped => "중지".into(),
        _ => "실패".into(),
    };
    let n = a.tail.chars().count();
    let tail: String = a.tail.chars().skip(n.saturating_sub(SIDE_RECORD_TAIL)).collect();
    let mut s = format!("상태: {state} · 도구 {}회{}", a.tools, if a.bg > 0 { format!(" · 백그라운드 명령 {}개", a.bg) } else { String::new() });
    if let Some(l) = &a.last {
        s.push_str(&format!("\n최근 활동: {l}"));
    }
    if !tail.trim().is_empty() {
        s.push_str(&format!("\n출력 끝부분: …{}", tail.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    s
}

/// Agents of the conversation's running tasks (the lead's own calls and removed workers left out), workers first.
fn side_asks(st: &AppState, conv: &str, question: &str) -> Vec<SideAsk> {
    let runs = st.agent_runs.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let mut out = vec![];
    for t in crate::remote::conv_snapshots(conv) {
        let removed = st.leads.lock().unwrap_or_else(|p| p.into_inner()).get(&t.task_id).map(|l| l.removed.lock().unwrap_or_else(|p| p.into_inner()).clone()).unwrap_or_default();
        for (key, a) in &t.agents {
            if key.starts_with("lead") || removed.contains(key) {
                continue;
            }
            let run = runs.get(&agent_slot(&t.task_id, key));
            let live = a.state == "run";
            let fork = run.filter(|r| live && r.spec.engine == EngineId::Claude).and_then(|r| Some(RunSpec { prompt: btw_prompt(question), resume: Some(r.session.clone()?), ..r.spec.clone() }));
            let title = t.parts.get(key).filter(|s| !s.trim().is_empty()).cloned().unwrap_or_else(|| a.label.clone());
            let state = if !live { "done" } else if fork.is_some() { "ask" } else { "log" };
            out.push(SideAsk { key: key.clone(), title, label: a.label.clone(), fork, record: agent_record(a), state, answer: String::new() });
        }
    }
    out.sort_by_key(|x| !x.key.starts_with("agent:"));
    for a in out.iter_mut().filter(|a| a.fork.is_some()).skip(SIDE_MAX_FORKS) {
        a.fork = None;
        a.state = "log";
    }
    out
}

fn emit_asks(app: &AppHandle, side_id: &str, asks: &[SideAsk]) {
    let list: Vec<Value> = asks.iter().map(|a| json!({ "key": a.key, "title": a.title, "label": a.label, "state": a.state, "text": a.answer })).collect();
    emit(app, "side:asks", json!({ "sideId": side_id, "asks": list }));
}

/// The fork leaves its own transcript in Claude Code's project folder; it is only noise in the resume list.
fn drop_fork_transcript(sid: &str) {
    if sid.is_empty() || !sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return;
    }
    let base = std::env::var("CLAUDE_CONFIG_DIR").map(PathBuf::from).ok().or_else(|| std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok().map(|h| PathBuf::from(h).join(".claude")));
    let Some(Ok(dirs)) = base.map(|b| std::fs::read_dir(b.join("projects"))) else { return };
    for d in dirs.flatten() {
        let f = d.path().join(format!("{sid}.jsonl"));
        if f.is_file() {
            let _ = std::fs::remove_file(f);
            return;
        }
    }
}

/// Asks one worker through a fork of its session. Stops after SIDE_FORK_TIMEOUT even while it keeps writing.
async fn ask_fork(spec: RunSpec, stop: CancellationToken) -> Outcome {
    let t = stop.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(SIDE_FORK_TIMEOUT).await;
        t.cancel();
    });
    let o = engines::run_fork(&spec, SIDE_FORK_TIMEOUT, &stop, |_| {}).await;
    timer.abort();
    if let Some(sid) = &o.session_id {
        drop_fork_transcript(sid);
    }
    o
}

fn side_lead_prompt(question: &str, progress: Option<&str>, asks: &[SideAsk], thread: &[SideTurn]) -> String {
    let mut s = String::new();
    if let Some(p) = progress.map(str::trim).filter(|p| !p.is_empty()) {
        s.push_str(&format!("[지금 진행 상황]\n{}\n\n", cut(p, SIDE_PROGRESS_CHARS / 2)));
    }
    s.push_str("[워커에게 물어 받은 답]\n");
    for a in asks {
        let how = match a.state {
            "ok" => "/btw 답",
            "done" => "이미 끝남, 실행 기록",
            "fail" => "답 없음, 실행 기록",
            _ => "물을 수 없는 AI, 실행 기록",
        };
        let body = if a.state == "ok" { cut(a.answer.trim(), SIDE_ANSWER_CHARS) } else { a.record.clone() };
        s.push_str(&format!("### {} [{}] ({how})\n{body}\n\n", a.title, a.label));
    }
    let recent = &thread[thread.len().saturating_sub(SIDE_TURNS)..];
    if !recent.is_empty() {
        s.push_str("[앞선 곁질문]\n");
        for t in recent {
            s.push_str(&format!("{}: {}\n", if t.role == "user" { "사용자" } else { "AI" }, cut(&t.text, SIDE_TURN_CHARS)));
        }
        s.push('\n');
    }
    s.push_str(&format!("[곁질문]\n{question}"));
    s
}

/// 작업이 끝난 뒤 바뀐 파일 중 사용자에게 건넬 결과물(보고서·문서·이미지·내보낸 데이터 등)을 JEV가 골라 답에 붙인다.
async fn attach_deliverables(app: &AppHandle, st: &AppState, conv: &str, task: &str, text: &str, since: std::time::SystemTime) {
    let s = st.settings();
    let Some(key) = secrets::get(SecretName::TypeSafe).filter(|_| s.jev_enabled) else { return };
    let Some(answer) = store::list_messages(&st.db, conv).ok().and_then(|m| m.into_iter().rev().find(|m| m.role == "assistant")) else { return };
    let mut data = answer.data.clone().unwrap_or_else(|| json!({}));
    if data.get("error").is_some() || data.get("rejected").is_some() {
        return;
    }
    let workspace = store::get_workspace(&st.db, conv).ok().flatten().map(PathBuf::from).unwrap_or_else(|| s.workspace());
    let mut have = attachments::of_data(Some(&data));
    let files: Vec<PathBuf> = attachments::changed_since(&workspace, since, JEV_MAX_ATTACH_CANDIDATES).into_iter().filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() <= attachments::MAX_FILE_BYTES)).collect();
    if files.is_empty() {
        return;
    }
    let mut list = serde_json::Map::new();
    let mut q = serde_json::Map::new();
    for (i, f) in files.iter().enumerate() {
        let k = format!("f{i}");
        let rel = f.strip_prefix(&workspace).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let head = std::fs::read(f).ok().map(|b| attachments::classify(&rel, &b[..b.len().min(8192)]).0).unwrap_or("file");
        list.insert(k.clone(), json!({ "path": rel, "kind": head, "size_kb": std::fs::metadata(f).map(|m| m.len().div_ceil(1024)).unwrap_or(0), "excerpt": attachments::excerpt(f, head, 300) }));
        q.insert(k.clone(), json!({"type": "noul", "instructions": format!("An AI just carried out `request` and created or changed `files.{k}`. Is this file a finished result the user asked to receive, such as a report, document, image, spreadsheet, data export or installer? Answer no for source code, project configuration, lock files, logs, caches and anything edited only as part of building software.")}));
    }
    let state = json!({ "request": cut(text, memory::FILTER_REQUEST_CHARS), "summary": cut(&answer.content, 1500), "files": list });
    let Ok(r) = jev::ask(&st.http, &key, "jev-latest", state, q, 1, Duration::from_secs(12)).await else { return };
    let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "attach", ok: true, latency_ms: r.latency_ms, input_tokens: None, output_tokens: None, cost_usd: Some(r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6) });
    let a = &r.response.answers;
    let mut picked: Vec<(f64, usize)> = (0..files.len()).filter_map(|i| jev::noul(a, &format!("f{i}")).filter(|p| *p >= JEV_DELIVERABLE).map(|p| (p, i))).collect();
    picked.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut added = 0;
    for (_, i) in picked.into_iter().take(JEV_MAX_AUTO_ATTACH) {
        let name = files[i].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if have.iter().any(|h| h.name == name) {
            continue;
        }
        if let Ok(f) = attachments::add_path(&st.data_dir, &files[i], "agent") {
            have.push(f);
            added += 1;
        }
    }
    if added > 0 {
        if let Some(o) = data.as_object_mut() {
            o.insert("attachments".into(), json!(have));
        }
        let _ = store::set_message_data(&st.db, &answer.id, &data);
        emit(app, "message:created", json!({ "conversationId": conv, "messageId": answer.id, "taskId": task }));
    }
}

/// 곁질문: messages·세션 메모리·CLI 세션을 읽기만 하고 쓰지 않으므로 대화에 영향을 주지 않는다. 사용량만 기록한다.
#[allow(clippy::too_many_arguments)]
pub fn side_ask(app: &AppHandle, st: &std::sync::Arc<AppState>, side_id: String, conversation_id: Option<String>, workspace: Option<String>, question: String, progress: Option<String>, thread: Vec<SideTurn>) -> Result<(), String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("보낼 내용이 없습니다".into());
    }
    let cancel = CancellationToken::new();
    st.tasks.lock().unwrap_or_else(|p| p.into_inner()).insert(side_id.clone(), cancel.clone());
    let (app2, st2) = (app.clone(), st.clone());
    tauri::async_runtime::spawn(async move {
        let (engine, o) = run_side(&app2, &st2, &side_id, conversation_id.as_deref(), workspace, &question, progress.as_deref(), &thread, &cancel).await;
        st2.tasks.lock().unwrap_or_else(|p| p.into_inner()).remove(&side_id);
        emit(&app2, "side:done", json!({ "sideId": side_id, "ok": o.ok && !o.text.trim().is_empty(), "text": o.text, "error": o.error, "engine": engine, "stopped": cancel.is_cancelled() }));
    });
    Ok(())
}

/// With agents running in this conversation the lead asks each of them (a fork of its session, like /btw), waits for
/// every reply and sums them up, without touching the project or the agents' work. Otherwise one AI answers.
#[allow(clippy::too_many_arguments)]
async fn run_side(app: &AppHandle, st: &AppState, side_id: &str, conv: Option<&str>, workspace: Option<String>, question: &str, progress: Option<&str>, thread: &[SideTurn], cancel: &CancellationToken) -> (Option<EngineId>, Outcome) {
    let mut asks = conv.map(|c| side_asks(st, c, question)).unwrap_or_default();
    if asks.is_empty() {
        return run_side_plain(app, st, side_id, conv, workspace, question, progress, thread, cancel).await;
    }
    let conv = conv.unwrap_or_default();
    emit_asks(app, side_id, &asks);
    let total = asks.iter().filter(|a| a.fork.is_some()).count();
    let asking = |done: usize| emit(app, "side:activity", json!({ "sideId": side_id, "text": SIDE_ASKING.replace("{done}", &done.to_string()).replace("{total}", &total.to_string()) }));
    let mut jobs = futures_util::stream::FuturesUnordered::new();
    for (i, a) in asks.iter().enumerate() {
        if let Some(spec) = a.fork.clone() {
            let stop = cancel.child_token();
            jobs.push(async move { (i, ask_fork(spec, stop).await) });
        }
    }
    if total > 0 {
        asking(0);
    }
    let mut done = 0;
    while let Some((i, o)) = futures_util::StreamExt::next(&mut jobs).await {
        done += 1;
        let _ = store::record_usage(&st.db, &store::Usage { engine: EngineId::Claude.as_str(), role: "side", ok: o.ok, latency_ms: o.latency_ms, input_tokens: o.input_tokens, output_tokens: o.output_tokens, cost_usd: o.cost_usd });
        let a = &mut asks[i];
        let good = o.ok && !o.text.trim().is_empty();
        a.state = if good { "ok" } else { "fail" };
        a.answer = if good { o.text } else { o.error.unwrap_or_default() };
        emit_asks(app, side_id, &asks);
        // On stop every fork sees the same token and ends its own process; keep draining so none is left running.
        if !cancel.is_cancelled() {
            asking(done);
        }
    }
    if cancel.is_cancelled() {
        return (None, Outcome { error: Some(AGENT_STOPPED.into()), ..Default::default() });
    }
    emit(app, "side:activity", json!({ "sideId": side_id, "text": SIDE_SUMMING }));
    // The task's lead sums up; without a lead the usual pick of AI does.
    let s = st.settings();
    let lead = st.leads.lock().unwrap_or_else(|p| p.into_inner()).values().find(|l| l.conv == conv).cloned();
    let (engines_to_try, cwd): (Vec<(EngineId, Option<String>)>, PathBuf) = match &lead {
        Some(l) => (vec![(l.engine, l.model.clone())], l.cwd.clone()),
        None => {
            let p = match plan_with(question, None, false, &st.installed(), &Catalog::default(), ReviewMode::Off, None, 1) {
                Ok(p) => p,
                Err(e) => return (None, Outcome { error: Some(e), ..Default::default() }),
            };
            let cwd = store::get_workspace(&st.db, conv).ok().flatten().map(PathBuf::from).unwrap_or_else(|| s.workspace());
            (std::iter::once(p.primary).chain(p.fallbacks.iter().copied()).map(|e| (e, model_for(&p, e, &s))).collect(), cwd)
        }
    };
    let prompt = side_lead_prompt(question, progress, &asks, thread);
    let system = format!("{}{SIDE_LEAD_SYSTEM}", system_prompt(false, &cwd.to_string_lossy(), None));
    let mut last = (None, Outcome::default());
    for (engine, model) in engines_to_try {
        if cancel.is_cancelled() {
            break;
        }
        let spec = RunSpec { engine, prompt: prompt.clone(), system: system.clone(), cwd: cwd.clone(), work: false, resume: None, plugin_dirs: vec![], model, effort: Some("low".into()), max_tokens: Some(4000), extra_dirs: vec![], steer: None, attachments: vec![] };
        emit(app, "side:started", json!({ "sideId": side_id, "engine": engine, "label": engine.label() }));
        let o = engines::run(&st.http, &spec, SIDE_TIMEOUT, cancel, |ev| {
            if let Event::Text(t) = ev {
                emit(app, "side:delta", json!({ "sideId": side_id, "delta": t }));
            }
        })
        .await;
        let _ = store::record_usage(&st.db, &store::Usage { engine: engine.as_str(), role: "side", ok: o.ok, latency_ms: o.latency_ms, input_tokens: o.input_tokens, output_tokens: o.output_tokens, cost_usd: o.cost_usd });
        let good = o.ok && !o.text.trim().is_empty();
        last = (Some(engine), o);
        if good {
            break;
        }
    }
    audit::record(&st.db, "user", "side.asked", if last.1.ok { "ok" } else { "error" }, json!({ "engine": last.0, "asked": total }));
    last
}

/// One AI answers from the conversation and progress shown on screen (nothing is running in this conversation).
#[allow(clippy::too_many_arguments)]
async fn run_side_plain(app: &AppHandle, st: &AppState, side_id: &str, conv: Option<&str>, workspace: Option<String>, question: &str, progress: Option<&str>, thread: &[SideTurn], cancel: &CancellationToken) -> (Option<EngineId>, Outcome) {
    let s = st.settings();
    let fail = |e: String| (None, Outcome { error: Some(e), ..Default::default() });
    // JEV가 있으면 AI와 노력 단계를 고르게 한다(파일·웹이 필요한 질문은 도구 있는 CLI로).
    let mut judgment = None;
    if let Some(key) = secrets::get(SecretName::TypeSafe).filter(|_| s.jev_enabled) {
        emit(app, "side:activity", json!({ "sideId": side_id, "text": "JEV가 요청을 판단하는 중" }));
        if let Ok(j) = route(&st.http, &key, question, &RouteAsk { core: true, engines: st.installed(), ..Default::default() }).await {
            let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "side", ok: true, latency_ms: j.ms, input_tokens: None, output_tokens: None, cost_usd: Some(j.cost_usd) });
            judgment = Some(j);
        }
    }
    let mut p = match plan_with(question, None, false, &st.installed(), &Catalog::default(), ReviewMode::Off, judgment.as_ref(), 1) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    if p.budget.is_none() {
        p.budget = budget_for("standard");
    }
    let stored = conv.and_then(|c| store::get_workspace(&st.db, c).ok().flatten());
    let cwd = workspace.or(stored).map(PathBuf::from).unwrap_or_else(|| s.workspace());
    let ws = cwd.to_string_lossy().to_string();
    let msgs = conv.map(|c| store::list_messages(&st.db, c).unwrap_or_default()).unwrap_or_default();
    let history = memory::turns(&msgs);
    let (summary, upto) = conv.map(|c| store::get_memory(&st.db, c).unwrap_or((None, None))).unwrap_or((None, None));
    // 곁질문도 지금 질문에 필요한 기억만 보낸다.
    let mut selection = None;
    if let Some(key) = secrets::get(SecretName::TypeSafe).filter(|_| s.jev_enabled) {
        if let Ok(Some(f)) = filter_memory(&st.http, &key, question, summary.as_deref(), &history).await {
            let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "memory", ok: true, latency_ms: f.ms, input_tokens: None, output_tokens: None, cost_usd: Some(f.cost_usd) });
            selection = Some(f.selection);
        }
    }
    let prompt = memory::context(summary.as_deref(), upto.as_deref(), &history, None, &side_prompt(&p.prompt, progress, thread), selection.as_ref());
    let system = format!("{}{SIDE_SYSTEM}", system_prompt(false, &ws, None));
    let mut last = (None, Outcome::default());
    for engine in std::iter::once(p.primary).chain(p.fallbacks.iter().copied()) {
        if cancel.is_cancelled() {
            break;
        }
        let spec = RunSpec {
            engine,
            prompt: prompt.clone(),
            system: system.clone(),
            cwd: cwd.clone(),
            work: false,
            resume: None,
            plugin_dirs: vec![],
            model: model_for(&p, engine, &s),
            effort: spec_effort(&p),
            max_tokens: p.budget.as_ref().map(|b| b.max_tokens),
            extra_dirs: vec![],
            steer: None,
            attachments: vec![],
        };
        emit(app, "side:started", json!({ "sideId": side_id, "engine": engine, "label": engine.label() }));
        let o = engines::run(&st.http, &spec, SIDE_TIMEOUT, cancel, |ev| match ev {
            Event::Text(t) => emit(app, "side:delta", json!({ "sideId": side_id, "delta": t })),
            Event::Activity(a) => emit(app, "side:activity", json!({ "sideId": side_id, "text": a })),
            _ => {}
        })
        .await;
        let _ = store::record_usage(&st.db, &store::Usage { engine: engine.as_str(), role: "side", ok: o.ok, latency_ms: o.latency_ms, input_tokens: o.input_tokens, output_tokens: o.output_tokens, cost_usd: o.cost_usd });
        let good = o.ok && !o.text.trim().is_empty();
        last = (Some(engine), o);
        if good {
            break;
        }
    }
    audit::record(&st.db, "user", "side.asked", if last.1.ok { "ok" } else { "error" }, json!({ "engine": last.0 }));
    last
}

/// 기본 작업 폴더 안의 새 프로젝트 폴더(project-20261030-143015). 같은 이름이 있으면 뒤에 수를 붙인다.
fn new_project_dir(base: &std::path::Path) -> Result<String, String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut dir = base.join(format!("project-{stamp}"));
    let mut n = 2;
    while dir.exists() {
        dir = base.join(format!("project-{stamp}-{n}"));
        n += 1;
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("프로젝트 폴더를 만들 수 없습니다({}): {e}", dir.display()))?;
    Ok(dir.to_string_lossy().to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    engine: EngineId,
    label: &'static str,
    role: &'static str,
    effort: Option<String>,
    model: Option<String>,
    ok: bool,
    ms: u64,
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    activities: Vec<String>,
    error: Option<String>,
}

/// The lead's own calls (judging, direct work) as one step, so the session cost includes them.
fn push_lead(steps: &mut Vec<Step>, lead: &crate::leadrun::TaskLead) {
    let o = lead.spent.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if o.cost_usd.is_some() || o.input_tokens.is_some() || o.output_tokens.is_some() {
        steps.push(step(lead.engine, "lead", &Outcome { ok: true, ..o }, &Some("medium".into()), lead.model.clone()));
    }
}

fn spec_effort(p: &Plan) -> Option<String> {
    p.budget.as_ref().map(|b| b.effort.to_string())
}

/// 이 엔진에 줄 모델: 요청 글에서 고른 모델 > (Claude Code) JEV 단계의 모델 > 설정의 모델. None이면 엔진 기본값.
fn model_for(p: &Plan, engine: EngineId, s: &Settings) -> Option<String> {
    if let Some((_, m)) = p.models.iter().find(|(e, _)| *e == engine) {
        return Some(m.clone());
    }
    if engine == EngineId::OpenCode {
        return p.free_model.clone().or_else(|| s.models.get(engine.as_str()).cloned());
    }
    if engine == EngineId::Claude {
        p.budget.as_ref().and_then(|b| b.claude_model).map(str::to_string)
    } else {
        s.models.get(engine.as_str()).cloned()
    }
}

/// 기록에 남길 모델 이름(API는 기본 모델 이름까지)
fn spec_model(p: &Plan, engine: EngineId, s: &Settings) -> Option<String> {
    if let Some((_, m)) = p.models.iter().find(|(e, _)| *e == engine) {
        return Some(m.clone());
    }
    if engine == EngineId::OpenCode {
        return Some(engines::opencode_model(p.free_model.as_deref().or(s.models.get(engine.as_str()).map(String::as_str))));
    }
    if engine == EngineId::Claude {
        p.budget.as_ref().and_then(|b| b.claude_model).map(str::to_string)
    } else if engine.is_api() {
        Some(s.models.get(engine.as_str()).cloned().unwrap_or_else(|| crate::apis::api(engine).map(|a| a.default_model.to_string()).unwrap_or_default()))
    } else {
        None
    }
}

fn step(engine: EngineId, role: &'static str, o: &Outcome, effort: &Option<String>, model: Option<String>) -> Step {
    Step {
        engine,
        label: engine.label(),
        role,
        effort: effort.clone(),
        model,
        ok: o.ok,
        ms: o.latency_ms,
        cost_usd: o.cost_usd,
        input_tokens: o.input_tokens,
        output_tokens: o.output_tokens,
        activities: o.activities.iter().take(30).cloned().collect(),
        error: o.error.clone(),
    }
}

/// 답 메시지에 함께 저장하는 진행 기록(어떤 AI가 무엇을 했는지).
fn message_data(steps: &[Step], p: &Plan, extra: Value) -> Value {
    let mut d = json!({ "steps": steps, "work": p.work, "reasons": p.reasons, "skill": p.skill.as_ref().map(|s| &s.0), "primary": p.primary, "reviewer": p.reviewer, "jev": p.jev, "budget": p.budget });
    if let (Some(o), Value::Object(e)) = (d.as_object_mut(), extra) {
        o.extend(e);
    }
    d
}

/// 에이전트 하나만 멈췄을 때 그 실행의 오류(같은 작업의 다른 에이전트는 계속 돈다)
pub const AGENT_STOPPED: &str = "사용자에 의해 중지되었습니다.";
/// 응답 없이 멈춘 작업 실행을 같은 대화로 다시 이어 주는 횟수
const STALL_RESUMES: u32 = 2;
/// 세션을 이어 쓸 수 없는 엔진에 다시 보내는 앞선 글의 최대 글자 수
const FOLLOW_UP_PARTIAL: usize = 6000;

pub(crate) fn agent_slot(task: &str, key: &str) -> String {
    format!("{task}/{key}")
}

/// 에이전트 하나만 멈춘다. 없거나 이미 끝났으면 false.
pub fn cancel_agent(st: &AppState, task: &str, key: &str) -> bool {
    // 차례 사이(검토·대기열·질문 대기)에도 멈추도록 워커 토큰을 먼저 본다.
    if let Some(t) = st.agent_stops.lock().unwrap_or_else(|p| p.into_inner()).get(&agent_slot(task, key)) {
        t.cancel();
        return true;
    }
    match st.agents.lock().unwrap_or_else(|p| p.into_inner()).get(&agent_slot(task, key)) {
        Some(l) => {
            l.stop.cancel();
            true
        }
        None => false,
    }
}

fn agent_said(app: &AppHandle, task: &str, key: &str, text: &str) {
    emit(app, "task:agent-message", json!({ "taskId": task, "key": key, "text": text }));
}

/// 에이전트 하나에게 말을 건다. 없거나 이미 끝났으면 false.
pub fn message_agent(app: &AppHandle, st: &AppState, task: &str, key: &str, text: &str) -> bool {
    crate::afterwork::note(task, text);
    let delivered = match st.agents.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&agent_slot(task, key)) {
        Some(l) => {
            l.deliver(text.to_string());
            true
        }
        // 차례 사이면 다음 차례에 전한다.
        None => match st.agent_inboxes.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&agent_slot(task, key)) {
            Some(mail) => {
                mail.push(text.to_string());
                true
            }
            None => false,
        },
    };
    if delivered {
        agent_said(app, task, key, text);
    }
    delivered
}

/// 이 작업에서 지금 실행 중인 에이전트 모두에게 말을 건다. 받은 에이전트 수를 돌려준다.
pub fn message_task(app: &AppHandle, st: &AppState, task: &str, text: &str) -> usize {
    crate::afterwork::note(task, text);
    let prefix = format!("{task}/");
    let mut keys: Vec<String> = {
        let mut g = st.agents.lock().unwrap_or_else(|p| p.into_inner());
        g.iter_mut()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, l)| {
                l.deliver(text.to_string());
                k[prefix.len()..].to_string()
            })
            .collect()
    };
    // 차례 사이의 워커는 다음 차례에 받는다.
    for (k, mail) in st.agent_inboxes.lock().unwrap_or_else(|p| p.into_inner()).iter_mut().filter(|(k, _)| k.starts_with(&prefix)) {
        let key = k[prefix.len()..].to_string();
        if !keys.contains(&key) {
            mail.push(text.to_string());
            keys.push(key);
        }
    }
    for k in &keys {
        agent_said(app, task, k, text);
    }
    keys.len()
}

/// 말을 건 뒤의 다음 차례. 자기 세션을 이어 쓸 수 있으면 메시지만, 아니면 원래 요청과 지금까지 쓴 글을 함께 보낸다.
fn follow_up(spec: &RunSpec, original: &str, o: &Outcome, msgs: &[String]) -> RunSpec {
    let said = msgs.join("\n");
    let mut next = spec.clone();
    let session = o.session_id.clone().or_else(|| spec.resume.clone());
    match session.filter(|_| spec.engine.resumable()) {
        Some(s) => {
            next.resume = Some(s);
            next.prompt = format!("[사용자가 작업 중에 보낸 메시지]\n{said}\n\n이 메시지를 반영해 원래 요청을 이어서 끝내세요. 마지막에는 앞서 쓴 부분까지 포함한 완성된 답을 쓰세요.");
        }
        None => {
            let n = o.text.chars().count();
            let partial: String = o.text.chars().skip(n.saturating_sub(FOLLOW_UP_PARTIAL)).collect();
            let files = if spec.work { " 앞선 시도에서 이미 고친 파일이 있을 수 있으니 폴더의 현재 상태를 확인하고 이어 가세요." } else { "" };
            next.prompt = format!("{original}\n\n[중단되기 전까지 쓴 내용]\n{partial}\n\n[사용자가 작업 중에 보낸 메시지]\n{said}\n\n이 메시지를 반영해 요청을 끝까지 완성하세요.{files} 마지막에는 앞서 쓴 부분까지 포함한 완성된 답을 쓰세요.");
        }
    }
    next
}

/// 응답 없이 멈춘 뒤 다시 이어 가는 차례. 세션을 이을 수 있으면 같은 대화로, 아니면 원래 요청을 다시 보낸다.
fn stall_resume(spec: &RunSpec, session: Option<String>) -> RunSpec {
    let mut next = spec.clone();
    let note = "이전 작업이 장시간 응답하지 않아 중단되었습니다. 현재 작업 디렉터리 상태를 확인하고, 멈출 우려가 있는 명령은 재실행을 피하거나 타임아웃 및 백그라운드로 실행하세요. 원래 진행 중이던 작업을 끝까지 완수해 주세요.";
    match session.or_else(|| spec.resume.clone()).filter(|_| spec.engine.resumable()) {
        Some(s) => {
            next.resume = Some(s);
            next.prompt = note.to_string();
        }
        None => next.prompt = format!("{}\n\n{note}", spec.prompt),
    }
    next
}

pub(crate) fn add_opt<T: std::ops::Add<Output = T> + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x + y),
        (x, y) => x.or(y),
    }
}

/// 끊긴 차례의 사용량·활동·글을 앞 차례 몫에 더한다. 글이 없던 차례는 앞의 글을 지우지 않는다.
fn absorb(earlier: &mut Outcome, cur: Outcome) {
    earlier.cost_usd = add_opt(earlier.cost_usd, cur.cost_usd);
    earlier.input_tokens = add_opt(earlier.input_tokens, cur.input_tokens);
    earlier.output_tokens = add_opt(earlier.output_tokens, cur.output_tokens);
    earlier.latency_ms += cur.latency_ms;
    earlier.activities.extend(cur.activities);
    if !cur.text.trim().is_empty() {
        earlier.text = cur.text;
    }
    if cur.session_id.is_some() {
        earlier.session_id = cur.session_id;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_engine(app: &AppHandle, st: &AppState, conv: &str, task: &str, mid: &str, role: &'static str, spec: RunSpec, timeout: Duration, cancel: &CancellationToken) -> Outcome {
    let key = format!("{role}:{}", spec.engine.as_str());
    run_engine_as(app, st, conv, task, mid, role, spec, timeout, cancel, true, &key).await
}

/// stream=false: 토론·동시 에이전트처럼 여러 AI가 동시에 쓸 때. 글자를 흘리지 않고 시작·끝만 알린다(task:debate).
/// key: 이 실행을 가리키는 이름(오른쪽 "에이전트" 탭이 실행마다 상태·활동을 보여 준다). 같은 엔진이 여럿이면 key로 구분한다.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_engine_as(app: &AppHandle, st: &AppState, conv: &str, task: &str, mid: &str, role: &'static str, spec: RunSpec, timeout: Duration, cancel: &CancellationToken, stream: bool, key: &str) -> Outcome {
    emit(app, "task:agent", json!({ "taskId": task, "conversationId": conv, "key": key, "engine": spec.engine, "label": spec.engine.label(), "role": role, "model": spec.model, "effort": spec.effort, "work": spec.work, "state": "run", "at": crate::db::now() }));
    if stream {
        emit(app, "task:phase", json!({ "taskId": task, "conversationId": conv, "messageId": mid, "phase": role, "engine": spec.engine, "label": spec.engine.label(), "effort": spec.effort, "model": spec.model }));
    } else {
        emit(app, "task:debate", json!({ "taskId": task, "conversationId": conv, "engine": spec.engine, "role": role, "state": "run" }));
    }
    // 에이전트 하나만 멈출 수 있게 작업 토큰의 자식 토큰으로 돌린다(작업을 멈추면 같이 멈춘다).
    let own = cancel.child_token();
    let slot = agent_slot(task, key);
    let mut turn = own.child_token();
    st.agents.lock().unwrap_or_else(|p| p.into_inner()).insert(slot.clone(), AgentLine { stop: own.clone(), turn: turn.clone(), inbox: vec![], steer: spec.steer.clone() });
    // The lead's own calls start a new session each time: nothing to ask there.
    if !key.starts_with("lead") {
        let run = AgentRun { spec: RunSpec { steer: None, attachments: vec![], ..spec.clone() }, session: spec.resume.clone() };
        st.agent_runs.lock().unwrap_or_else(|p| p.into_inner()).insert(slot.clone(), run);
    }
    let original = spec.prompt.clone();
    let mut spec = spec;
    let mut earlier = Outcome::default();
    let mut stalls = 0;
    let mut turn_no: u32 = 0;
    let mut o = loop {
        turn_no += 1;
        // Last live usage of this turn: kept when the process ends without a final usage report (stall, kill).
        let mut seen = (0u64, 0u64);
        // 차례마다 같은 무응답 한도를 준다(일하는 동안은 전체 시간으로 자르지 않는다).
        let mut cur = engines::run(&st.http, &spec, timeout, &turn, |ev| match ev {
            Event::Text(t) if stream => emit(app, "task:delta", json!({ "taskId": task, "conversationId": conv, "messageId": mid, "phase": role, "delta": t })),
            // 여러 AI가 동시에 쓸 때는 실행(key)별로 흘려 각 창이 따로 보여 준다(회의 창).
            Event::Text(t) => emit(app, "task:agent-delta", json!({ "taskId": task, "conversationId": conv, "key": key, "delta": t })),
            // 실시간 사용량: 이번 차례에서 지금까지 쓴 토큰과 추정 비용
            // `turn` lets screens add the previous turn to the total instead of overwriting it.
            Event::Usage { input, output } => {
                seen = (*input, *output);
                emit(app, "task:agent-usage", json!({ "taskId": task, "conversationId": conv, "key": key, "engine": spec.engine, "turn": turn_no, "input": input, "output": output, "costUsd": crate::pricing::estimate_cost(spec.engine, spec.model.as_deref(), Some(*input), Some(*output)) }))
            }
            Event::Activity(a) => {
                emit(app, "task:agent-activity", json!({ "taskId": task, "conversationId": conv, "key": key, "text": a }));
                if stream {
                    emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": role, "text": a }));
                }
            }
            Event::Command(c) => crate::leadrun::worker_ran(app, st, task, key, c),
            Event::Session(s) => {
                if let Some(r) = st.agent_runs.lock().unwrap_or_else(|p| p.into_inner()).get_mut(&slot) {
                    r.session = Some(s.clone());
                }
            }
            Event::Bg(engines::Bg::Started { id, command, path }) => st.bg.start_claude(app, task, key, id, command, path.clone()),
            Event::Bg(engines::Bg::Ended { id, status }) => st.bg.finish_claude(app, id, status),
            _ => {}
        })
        .await;
        if cur.input_tokens.is_none() && cur.output_tokens.is_none() && seen != (0, 0) {
            cur.input_tokens = Some(seen.0);
            cur.output_tokens = Some(seen.1);
            cur.cost_usd = cur.cost_usd.or_else(|| crate::pricing::estimate_cost(spec.engine, spec.model.as_deref(), Some(seen.0), Some(seen.1)));
        }
        // 프로세스가 끝나면 그 안의 백그라운드 셸도 함께 끝난다.
        st.bg.end_run(app, task, key);
        // 멈춘 프로세스만 끝내고 일은 버리지 않는다: 같은 대화(또는 작업 공간)에서 이어 가게 한다.
        if cur.stalled && spec.work && !own.is_cancelled() && stalls < STALL_RESUMES {
            stalls += 1;
            emit(app, "task:agent-activity", json!({ "taskId": task, "conversationId": conv, "key": key, "text": "무응답으로 재시작되어 기존 작업 재개" }));
            spec = stall_resume(&spec, cur.session_id.clone());
            absorb(&mut earlier, cur);
            continue;
        }
        // 잠금 안에서 함께 처리해 메시지가 끝난 실행으로 사라지지 않게 한다: 받은 메시지가 없으면 이 자리에서 선을 닫는다.
        let msgs = {
            let mut g = st.agents.lock().unwrap_or_else(|p| p.into_inner());
            match g.get_mut(&slot) {
                Some(l) if !own.is_cancelled() && !l.inbox.is_empty() => {
                    turn = own.child_token();
                    l.turn = turn.clone();
                    std::mem::take(&mut l.inbox)
                }
                _ => {
                    g.remove(&slot);
                    vec![]
                }
            }
        };
        if msgs.is_empty() {
            break cur;
        }
        spec = follow_up(&spec, &original, &cur, &msgs);
        emit(app, "task:agent-activity", json!({ "taskId": task, "conversationId": conv, "key": key, "text": format!("메시지 전달: {}", cut(&msgs.join(" "), 60)) }));
        if stream {
            emit(app, "task:delta", json!({ "taskId": task, "conversationId": conv, "messageId": mid, "phase": role, "delta": "\n\n" }));
        } else {
            emit(app, "task:agent-delta", json!({ "taskId": task, "conversationId": conv, "key": key, "delta": "\n\n" }));
        }
        absorb(&mut earlier, cur);
    };
    // 끊긴 차례의 사용량도 합치고, 마지막 차례가 글을 못 냈으면 그 앞의 글을 살린다.
    o.cost_usd = add_opt(earlier.cost_usd, o.cost_usd);
    o.input_tokens = add_opt(earlier.input_tokens, o.input_tokens);
    o.output_tokens = add_opt(earlier.output_tokens, o.output_tokens);
    o.latency_ms += earlier.latency_ms;
    if !earlier.activities.is_empty() {
        earlier.activities.append(&mut o.activities);
        o.activities = earlier.activities;
    }
    if o.text.trim().is_empty() {
        o.text = earlier.text;
    }
    o.session_id = o.session_id.or(earlier.session_id);
    let stopped = own.is_cancelled() && !cancel.is_cancelled();
    if stopped {
        o.ok = false;
        o.error = Some(AGENT_STOPPED.into());
    }
    let done = o.ok && !o.text.trim().is_empty();
    emit(app, "task:agent", json!({ "taskId": task, "conversationId": conv, "key": key, "engine": spec.engine, "label": spec.engine.label(), "role": role, "state": if done { "ok" } else { "fail" }, "stopped": stopped, "ms": o.latency_ms, "error": o.error, "chars": o.text.chars().count(), "inputTokens": o.input_tokens, "outputTokens": o.output_tokens, "costUsd": o.cost_usd }));
    if !stream {
        emit(app, "task:debate", json!({ "taskId": task, "conversationId": conv, "engine": spec.engine, "role": role, "state": if o.ok && !o.text.trim().is_empty() { "ok" } else { "fail" }, "ms": o.latency_ms }));
    }
    let _ = store::record_usage(&st.db, &store::Usage {
        engine: spec.engine.as_str(),
        role,
        ok: o.ok,
        latency_ms: o.latency_ms,
        input_tokens: o.input_tokens,
        output_tokens: o.output_tokens,
        cost_usd: o.cost_usd,
    });
    audit::record(&st.db, "elysium", &format!("engine.{role}"), if o.ok { "ok" } else { "error" }, json!({ "engine": spec.engine, "ms": o.latency_ms, "work": spec.work }));
    o
}

#[allow(clippy::too_many_arguments)]
async fn run_task(app: &AppHandle, st: &AppState, conv: &str, task: &str, text: &str, choice: Option<EngineId>, work_req: bool, build: bool, council_req: bool, setup: &WorkSetup, cancel: CancellationToken) {
    let mid = new_id();
    // The work setup panel overrides these settings for this request only.
    let mut s = st.settings();
    if let Some(r) = setup.review {
        s.review = r;
    }
    if let Some(c) = setup.lead_commit {
        s.lead_commit = c;
    }
    if let Some(j) = setup.jev {
        s.jev_enabled = s.jev_enabled && j;
    }
    let s = s;
    let workspace = store::get_workspace(&st.db, conv).ok().flatten().map(PathBuf::from).unwrap_or_else(|| s.workspace());
    let ws = workspace.to_string_lossy().to_string();
    let catalog = st.catalog();
    // 세션 메모리 원본: 이 세션의 모든 AI가 같은 요약·최근 대화를 받는다(이번 요청은 뺀다).
    let msgs = store::list_messages(&st.db, conv).unwrap_or_default();
    let history = memory::turns(&msgs[..msgs.len().saturating_sub(1)]);
    let (summary, upto) = store::get_memory(&st.db, conv).unwrap_or((None, None));
    // JEV(켜져 있고 키가 있을 때만): 요청 판단과 기억 거르기를 동시에 한 번씩 묻는다. 실패하면 규칙·전체 기억으로 돌아간다.
    let mut judgment: Option<Judgment> = None;
    let mut jev_error: Option<String> = None;
    let mut selection: Option<memory::Selection> = None;
    let mut memory_note: Option<String> = None;
    let want_core = s.jev_enabled;
    // 이번 요청에 붙은 파일과 앞선 차례의 파일
    let current_files = msgs.last().map(|m| attachments::of_data(m.data.as_ref())).unwrap_or_default();
    let mut earlier_files: Vec<Attachment> = msgs[..msgs.len().saturating_sub(1)].iter().flat_map(|m| attachments::of_data(m.data.as_ref())).collect();
    earlier_files.drain(..earlier_files.len().saturating_sub(JEV_MAX_EARLIER));
    let mut picked_earlier = earlier_by_name(text, &earlier_files);
    let free_models: Vec<String> = st.engines.read().unwrap_or_else(|p| p.into_inner()).iter().find(|e| e.id == EngineId::OpenCode && e.installed).map(|e| e.models.clone()).unwrap_or_default();
    let free_choices: Vec<String> = if choice.is_none() || choice == Some(EngineId::OpenCode) { free_models.clone() } else { vec![] };
    let file_briefs: Vec<Value> = current_files.iter().map(|f| file_brief(&st.data_dir, f)).collect();
    // JEV가 고를 것: /이름으로 직접 부르지 않았으면 켜진 스킬 중에서, 직접 고르지 않았으면 AI와 작업 분할까지.
    let route_skills: Vec<&Skill> = if s.jev_enabled && catalog.explicit(text).is_none() { catalog.enabled_skills().collect() } else { vec![] };
    let route_engines: Vec<EngineId> = if s.jev_enabled && choice.is_none() {
        let all = st.installed();
        let clis: Vec<EngineId> = all.iter().copied().filter(|e| !e.is_api()).collect();
        if work_req && !clis.is_empty() { clis } else { all }
    } else {
        vec![]
    };
    let want_parts = s.jev_enabled && work_req && choice.is_none() && intent::requested_agents(text).is_none();
    let want_judge = want_core || !route_skills.is_empty();
    let jev_key = if s.jev_enabled { secrets::get(SecretName::TypeSafe) } else { None };
    if want_judge && jev_key.is_none() {
        jev_error = Some("TypeSafe API 키가 없습니다".into());
    }
    if let Some(key) = &jev_key {
        let want_filter = memory::candidates(summary.as_deref(), &history).is_some();
        if want_judge || want_filter {
            let what = match (want_judge, want_filter) {
                (true, true) => "JEV가 요청을 판단하고 필요한 기억을 고르는 중",
                (true, false) => "JEV가 요청을 판단하는 중",
                _ => "JEV가 필요한 기억을 고르는 중",
            };
            emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "plan", "text": what }));
        }
        let (jr, fr, er) = tokio::join!(
            async {
                if want_judge {
                    Some(route(&st.http, key, text, &RouteAsk { core: want_core, skills: route_skills, engines: route_engines, parts: want_parts, files: file_briefs, free_models: free_choices, work: work_req }).await)
                } else {
                    None
                }
            },
            async {
                if want_filter {
                    filter_memory(&st.http, key, text, summary.as_deref(), &history).await
                } else {
                    Ok(None)
                }
            },
            async {
                if earlier_files.is_empty() {
                    None
                } else {
                    Some(pick_earlier_files(&st.http, key, &st.data_dir, text, &earlier_files).await)
                }
            },
        );
        if let Some(Ok((keep, ms, cost))) = er {
            let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "attach", ok: true, latency_ms: ms, input_tokens: None, output_tokens: None, cost_usd: Some(cost) });
            picked_earlier = keep;
        }
        match jr {
            Some(Ok(j)) => {
                let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "judge", ok: true, latency_ms: j.ms, input_tokens: None, output_tokens: None, cost_usd: Some(j.cost_usd) });
                judgment = Some(j);
            }
            Some(Err(e)) => jev_error = Some(e),
            None => {}
        }
        match fr {
            Ok(Some(f)) => {
                let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "memory", ok: true, latency_ms: f.ms, input_tokens: None, output_tokens: None, cost_usd: Some(f.cost_usd) });
                memory_note = Some(f.selection.reason());
                selection = Some(f.selection);
            }
            Ok(None) => {}
            Err(e) => memory_note = Some(format!("JEV로 기억을 거르지 못해 기억을 그대로 보냅니다: {e}")),
        }
    }
    let checks: Mutex<Vec<Value>> = Mutex::new(vec![]);
    let finish = |content: String, mut data: Value| {
        // 답 안의 파일 표시를 첨부로 바꾼다.
        let (content, sent) = attachments::extract(&st.data_dir, &workspace, &content);
        let content = if content.trim().is_empty() && !sent.is_empty() { "파일을 첨부했습니다.".to_string() } else { content };
        if let (false, Some(o)) = (sent.is_empty(), data.as_object_mut()) {
            o.insert("attachments".into(), json!(sent));
        }
        let c = checks.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let (false, Some(o)) = (c.is_empty(), data.as_object_mut()) {
            o.insert("jevChecks".into(), json!(c));
        }
        let _ = store::insert_message_as(&st.db, &mid, conv, "assistant", &content, Some(&data));
        emit(app, "message:created", json!({ "conversationId": conv, "messageId": mid, "taskId": task }));
    };
    // 팀·여러 모델이 적힌 글이면 OpenCode가 먼저 '지금 돌릴 팀인지, 만들어 달라는 글의 내용인지'를 판단한다.
    // 지금 돌릴 팀이면 규칙으로 읽고, 규칙이 못 읽으면 OpenCode가 뽑은 구성표를 쓴다. OpenCode가 없거나 답을 못 읽으면 예전처럼 한다.
    let installed = st.installed();
    // A team built in the work setup panel is used as is; a count set there replaces the AI's count.
    let given = setup.team.as_ref().filter(|_| !council_req);
    let (mut team, mut team_unread, mut team_note) = (TeamText::Parse, false, None::<String>);
    match given {
        Some(t) if work_req => match t.roster(&installed, true) {
            Ok(r) => team = TeamText::Given(r),
            Err(e) => return finish(format!("⚠ {e}"), json!({ "error": e })),
        },
        Some(_) => team_note = Some(SETUP_CHAT_TEAM.to_string()),
        None => {}
    }
    let fixed = given.is_some() || setup.agents > 0;
    let team_written = !fixed && work_req && intent::team_signals(text, &installed, true);
    if team_written {
        let mut decided = None;
        if installed.contains(&EngineId::OpenCode) {
            emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "plan", "text": "OpenCode가 작성된 팀 확인 중" }));
            let spec = RunSpec { engine: EngineId::OpenCode, prompt: intent::team_intent_prompt(text), system: system_prompt(false, &ws, None), cwd: workspace.clone(), work: false, resume: None, plugin_dirs: vec![], model: pick_free_model(&free_models, s.models.get("opencode").map(String::as_str), judgment.as_ref(), false), effort: None, max_tokens: None, extra_dirs: vec![], steer: None, attachments: vec![] };
            let o = run_engine_as(app, st, conv, task, &mid, "roadmap", spec, TEAM_READ_TIMEOUT, &cancel, false, "team-intent").await;
            decided = o.ok.then(|| intent::parse_team_intent(&o.text)).flatten();
        }
        match decided {
            Some(d) if !d.run_now => {
                team = TeamText::Ignore;
                team_note = Some(format!("OpenCode는 작성된 팀을 지금 실행하지 않습니다 ({why})", why = d.why));
            }
            Some(d) => {
                // 지금 돌릴 팀이면 OpenCode가 뽑은 구성표를 먼저 쓴다(규칙은 "소네트를 3대" 같은 말을 놓칠 수 있다). 읽을 수 없으면 규칙으로 읽는다.
                if matches!(intent::roster_from_answer(&d.roster, text, &installed, true), Ok(Some(_)) | Err(_)) {
                    team = TeamText::Lines(d.roster);
                } else if intent::needs_team_reading(text, &installed, true) {
                    team_unread = true;
                }
            }
            None if intent::needs_team_reading(text, &installed, true) => {
                // OpenCode를 쓸 수 없으면 빠른 CLI가 구성표만 뽑는다.
                team_unread = true;
                if let Some((engine, model)) = team_reader(&installed) {
                    emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "plan", "text": "작성된 팀 구성 확인 중" }));
                    let spec = RunSpec { engine, prompt: intent::team_reading_prompt(text), system: system_prompt(false, &ws, None), cwd: workspace.clone(), work: false, resume: None, plugin_dirs: vec![], model, effort: Some("low".into()), max_tokens: Some(1000), extra_dirs: vec![], steer: None, attachments: vec![] };
                    let o = run_engine_as(app, st, conv, task, &mid, "plan", spec, TEAM_READ_TIMEOUT, &cancel, false, "plan").await;
                    if o.ok {
                        // "없음"이면 팀을 적은 글이 아니다. 답을 읽을 수 없을 때만 알린다.
                        team_unread = !o.text.contains("없음") && matches!(intent::roster_from_answer(&o.text, text, &installed, true), Ok(None));
                        team = TeamText::Lines(o.text);
                    }
                }
            }
            None => {}
        }
    }
    // 동시 에이전트 수: 요청 글이 팀·수·AI를 정하지 않은 파일 작업이면 AI가 정한다.
    // JEV가 1~3개로 봤으면 그 판단을 쓰고(plan_read가 읽는다), 4개 이상으로 봤거나 JEV를 쓰지 못했으면 빠른 CLI가 앞선 대화까지 읽고 정한다.
    let mut count_reason = None::<String>;
    let mut agents = 0u8;
    let jev_said = judgment.as_ref().and_then(|j| j.parts.map(|_| jev_parts(Some(j))));
    let ask_count = !fixed && work_req && choice.is_none() && !team_written && !matches!(jev_said, Some(None)) && !matches!(jev_said, Some(Some(n)) if n < 4);
    if let Some((engine, model)) = team_reader(&installed).filter(|_| ask_count) {
        emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "plan", "text": "에이전트 수 결정 중" }));
        let recent = history.iter().rev().take(AGENT_COUNT_TURNS).rev().map(|t| format!("{}: {}", t.role, cut(&t.content, AGENT_COUNT_TURN_CHARS))).collect::<Vec<_>>().join("\n");
        let at_least = jev_said.flatten().unwrap_or(1) as usize;
        let spec = RunSpec { engine, prompt: intent::agent_count_prompt(text, &recent, at_least), system: system_prompt(false, &ws, None), cwd: workspace.clone(), work: false, resume: None, plugin_dirs: vec![], model, effort: Some("low".into()), max_tokens: Some(400), extra_dirs: vec![], steer: None, attachments: vec![] };
        let o = run_engine_as(app, st, conv, task, &mid, "plan", spec, TEAM_READ_TIMEOUT, &cancel, false, "plan").await;
        if let Some((n, why)) = o.ok.then(|| intent::parse_agent_count(&o.text)).flatten() {
            let n = n.max(at_least);
            agents = n as u8;
            count_reason = Some(if why.is_empty() { format!("{n}개의 에이전트를 실행하기로 결정했습니다.") } else { format!("{n}개의 에이전트 실행 결정: {why}") });
        }
    }
    if setup.agents > 0 && given.is_none() {
        agents = setup.agents.min(intent::MAX_AGENTS as u8);
        count_reason = Some(SETUP_AGENTS.replace("{n}", &agents.to_string()));
    }
    let mut p = match plan_read(text, choice, work_req, &installed, &catalog, s.review, judgment.as_ref(), agents, team) {
        Ok(p) => p,
        Err(e) => return finish(format!("⚠ {e}"), json!({ "error": e })),
    };
    if team_unread {
        p.reasons.insert(0, "작성된 팀 구성을 확인할 수 없어 에이전트를 자동으로 배정했습니다.".to_string());
    }
    if let Some(n) = team_note {
        p.reasons.insert(0, n);
    }
    if let Some(e) = &jev_error {
        p.reasons.insert(0, format!("JEV를 쓰지 못해 규칙으로 정했습니다: {e}"));
    }
    if let Some(r) = count_reason {
        p.reasons.push(r);
    }
    if let Some(n) = memory_note {
        p.reasons.push(n);
    }
    // 이번 요청의 파일 + JEV가 고른 앞선 파일. CLI는 이 폴더들을 읽을 수 있어야 한다.
    let mut all_files = current_files.clone();
    for i in &picked_earlier {
        if let Some(f) = earlier_files.get(*i) {
            all_files.push(f.clone());
        }
    }
    if !picked_earlier.is_empty() {
        p.reasons.push(format!("이전 첨부 파일 {}개를 이 요청에 함께 전달합니다", picked_earlier.len()));
    }
    let files: Vec<AttachedFile> = all_files.iter().map(|f| attachments::to_file(&st.data_dir, f)).collect();
    p.free_model = pick_free_model(&free_models, s.models.get("opencode").map(String::as_str), judgment.as_ref(), files.iter().any(|f| f.kind == "image"));
    prefer_file_readers(&mut p, &files, choice);
    if let Some(r) = judgment.as_ref().and_then(|j| j.risk).filter(|_| p.work) {
        p.reasons.push(format!("JEV: 이 작업의 위험도 {r:.1}/3"));
    }
    if judgment.as_ref().and_then(|j| j.sensitive).is_some_and(|x| x >= JEV_SENSITIVE) && (p.primary == EngineId::OpenCode || p.fallbacks.contains(&EngineId::OpenCode)) {
        p.reasons.push("JEV: 민감한 내용이 있어 OpenCode는 입력을 보관하지 않는 무료 모델만 씁니다".into());
    }
    if build {
        // 끝까지 완성해야 하므로 가장 깊은 노력 단계로 돈다(요청 글에서 고른 모델은 그대로 따른다).
        p.budget = budget_for("deep");
        p.reasons.push("만들기: 승인 없이 이 폴더 안에서 끝까지 완성하도록 노력 단계를 '깊게'로 정했습니다".into());
    }
    if let Some(b) = setup.tier.as_deref().and_then(budget_for) {
        p.reasons.push(SETUP_TIER.replace("{tier}", tier_label(b.tier)));
        p.budget = Some(b);
    }
    // 회의: 버튼 또는 요청 글("서로 대화하며", "회의해서")로 켠다. 동시 에이전트·토론·검토를 대신한다.
    if !p.work && (council_req || intent::requested_council(text)) {
        let want = intent::requested_agents(text).unwrap_or(3);
        // 회의는 AI 수 × 라운드만큼 호출이 늘어난다: 노력 단계가 정해지지 않았으면 기본(가장 비싼) 모델 대신 '보통'으로 돈다.
        if p.budget.is_none() {
            p.budget = budget_for("standard");
            if let Some(b) = &p.budget {
                p.reasons.push(format!("노력 단계 '{}': 노력치 {}, Claude Code {}, API 출력 최대 {}토큰", tier_label(b.tier), b.effort, b.claude_model.unwrap_or("기본 모델"), b.max_tokens));
            }
        }
        match council_plan(&p, &st.installed(), want) {
            Some(c) => {
                let names = c.agents.iter().enumerate().map(|(i, a)| council_name(i, a)).collect::<Vec<_>>().join(", ");
                p.reasons.push(format!(
                    "{names} 참여로 최대 {rounds}라운드 동안 의견을 조율하며 합의 도달 시 조기 종료합니다.",
                    names = names,
                    rounds = c.rounds
                ));
                let stale = std::mem::take(&mut p.team_reasons);
                p.reasons.retain(|r| !stale.contains(r));
                p.team = None;
                p.debate = None;
                p.reviewer = None;
                p.council = Some(c);
            }
            None => p.reasons.push("회의에 참여 가능한 AI가 없어 단일 AI가 단독으로 답변합니다.".into()),
        }
    }
    emit(app, "task:started", json!({
        "taskId": task, "conversationId": conv, "messageId": mid,
        "primary": p.primary, "reviewer": p.reviewer, "work": p.work, "skill": p.skill.as_ref().map(|s| &s.0), "reasons": p.reasons, "jev": judgment, "budget": p.budget, "debate": p.debate, "team": p.team, "council": p.council, "model": model_for(&p, p.primary, &s),
    }));

    // 작업 모드: 파일을 고치기 전에 사용자의 허락을 받는다. '만들기'는 버튼을 누른 것이 허락이다.
    if p.work && build {
        audit::record(&st.db, "user", "build.started", "ok", json!({ "engine": p.primary, "workspace": ws }));
    } else if p.work {
        let preview: String = p.prompt.chars().take(160).collect();
        let a = match store::create_approval(&st.db, conv, task, p.primary.as_str(), &preview, &ws) {
            Ok(a) => a,
            Err(e) => return finish(format!("⚠ 승인을 만들 수 없습니다: {e}"), json!({ "error": e.to_string() })),
        };
        let (tx, rx) = oneshot::channel();
        st.approvals.lock().unwrap_or_else(|p| p.into_inner()).insert(a.id.clone(), tx);
        emit(app, "task:approval", json!(a));
        let approved = tokio::select! {
            r = rx => r.unwrap_or(false),
            _ = cancel.cancelled() => false,
            _ = tokio::time::sleep(Duration::from_secs(600)) => false,
        };
        st.approvals.lock().unwrap_or_else(|p| p.into_inner()).remove(&a.id);
        if !approved {
            let _ = store::decide_approval(&st.db, &a.id, false);
            return finish("작업을 하지 않았습니다(허락하지 않음·취소·10분 초과).".into(), json!({ "work": true, "rejected": true, "reasons": p.reasons }));
        }
    }

    // 스킬 참고 파일을 CLI가 읽을 수 있게 그 폴더를 넘긴다.
    let mut skill_dirs: Vec<String> = p.skill.as_ref().and_then(|s| s.0.dir.clone()).into_iter().collect();
    skill_dirs.extend(files.iter().filter_map(|f| f.path.parent().map(|d| d.to_string_lossy().to_string())));
    // 세션 메모리: JEV가 걸렀으면 남긴 것만, 아니면 요약 + 최근 대화를 붙인다.
    let ctx = |seen: Option<&str>, prompt: &str| memory::context(summary.as_deref(), upto.as_deref(), &history, seen, prompt, selection.as_ref());
    let system = if build { format!("{}{BUILD_SYSTEM}", system_prompt(true, &ws, p.skill.as_ref())) } else { system_prompt(p.work, &ws, p.skill.as_ref()) };
    let plugin_dirs = catalog.enabled_plugin_dirs();
    let timeout = Duration::from_secs(if build { 1800 } else if p.work { 1200 } else { 600 });
    let mut steps: Vec<Step> = vec![];
    // 병렬 작업: 작업(만들기)을 하위 작업으로 나눠 에이전트마다 폴더 사본에서 동시에 하고 → 바뀐 파일을 합치고 → 통합 AI가 마무리한다.
    if p.work {
        if let Some(team) = p.team.clone() {
            let tw = TeamWork { app, st, conv, task, mid: &mid, workspace: &workspace, settings: &s, system: &system, timeout, cancel: &cancel, extra_dirs: &skill_dirs, plugin_dirs: &plugin_dirs, roadmap: choice.is_none() && setup.roadmap != Some(false) && st.installed().contains(&EngineId::OpenCode), max_turns: setup.max_turns.map(|n| n.clamp(*SETUP_TURNS.start(), *SETUP_TURNS.end())), files: &files };
            if let Some((content, data)) = run_team_work(&tw, &mut p, &team, &ctx, &mut steps).await {
                return finish(content, data);
            }
        }
    }
    let spec_for = |engine: EngineId, prompt: String, system: String, work: bool| RunSpec {
        engine,
        prompt,
        system,
        cwd: workspace.clone(),
        work,
        resume: None,
        plugin_dirs: vec![],
        model: model_for(&p, engine, &s),
        effort: p.budget.as_ref().map(|b| b.effort.to_string()),
        max_tokens: p.budget.as_ref().map(|b| b.max_tokens),
        extra_dirs: skill_dirs.clone(),
        steer: None,
        attachments: files.clone(),
    };
    // JEV로 확인해 건너뛴 단계(답 비교·검토 생략)를 사용량과 답 기록에 남긴다.
    let jev_note = |kind: &'static str, value: f64, ms: u64, cost: f64| {
        let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: kind, ok: true, latency_ms: ms, input_tokens: None, output_tokens: None, cost_usd: Some(cost) });
        checks.lock().unwrap_or_else(|p| p.into_inner()).push(json!({ "kind": kind, "value": value, "ms": ms, "costUsd": cost }));
    };
    let pct = |x: f64| (x * 100.0).round() as u32;

    // 회의: 여러 AI가 라운드마다 서로의 발언을 읽고 대화한다 → 합의하면 가장 좋은 발언을, 아니면 정리 AI가 하나로 정리한다.
    if let Some(cn) = p.council.clone() {
        let n = cn.agents.len();
        let names: Vec<String> = cn.agents.iter().enumerate().map(|(i, a)| council_name(i, a)).collect();
        let base_sys = system_prompt(false, &ws, p.skill.as_ref());
        let systems: Vec<String> = (0..n)
            .map(|i| {
                let others: Vec<String> = names.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, x)| x.clone()).collect();
                format!("{base_sys}{}", council_system(&names[i], &others))
            })
            .collect();
        let good = |o: &Outcome| o.ok && !o.text.trim().is_empty();
        // 자기 세션을 이어 쓰는 CLI(Claude Code·Antigravity)는 2라운드부터 새 발언만 받는다.
        let mut sessions: Vec<Option<String>> = vec![None; n];
        // (라운드, 참가자 위치, 발언): 답한 발언만
        let mut log: Vec<(u8, usize, String)> = vec![];
        let mut rounds_json: Vec<Value> = vec![];
        // (합의 확률, 가장 좋은 발언을 한 참가자)
        let mut agreed: Option<(f64, usize)> = None;
        let opening = ctx(None, &council_open(&p.prompt));
        // 참가자마다 회의 기록(log) 중 어디까지 들었는지. 2라운드부터 세션을 이어 쓰는 AI는 그 뒤의 다른 참가자 발언만 받는다.
        let mut heard: Vec<usize> = vec![0; n];
        let record = |i: usize, round: u8, o: &Outcome, log: &mut Vec<(u8, usize, String)>, sessions: &mut Vec<Option<String>>, steps: &mut Vec<Step>, rounds_json: &mut Vec<Value>| {
            let a = &cn.agents[i];
            steps.push(step(a.engine, "council", o, &spec_effort(&p), a.model.clone().or_else(|| spec_model(&p, a.engine, &s))));
            rounds_json.push(json!({ "round": round, "idx": i, "engine": a.engine, "ok": good(o), "text": strip_agree(&o.text) }));
            if good(o) {
                if let (true, Some(sid)) = (a.engine.resumable(), &o.session_id) {
                    sessions[i] = Some(sid.clone());
                }
                log.push((round, i, o.text.clone()));
            }
        };
        let turn = |i: usize, round: u8, prompt: String, resume: Option<String>| {
            let a = &cn.agents[i];
            let mut spec = spec_for(a.engine, prompt, systems[i].clone(), false);
            if a.model.is_some() {
                spec.model = a.model.clone();
            }
            spec.resume = resume;
            let (key, mid, cancel) = (format!("council:{i}:{round}"), &mid, &cancel);
            async move { run_engine_as(app, st, conv, task, mid, "council", spec, COUNCIL_TURN, cancel, false, &key).await }
        };
        for round in 1..=cn.rounds {
            if cancel.is_cancelled() {
                break;
            }
            emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "council", "text": format!("{round}라운드 진행 중: AI {n}개가 발언하는 중", round = round, n = n) }));
            if round == 1 {
                // 1라운드: 서로 보기 전에 각자 해결안을 동시에 낸다.
                let outs: Vec<Outcome> = futures_util::future::join_all((0..n).map(|i| turn(i, round, opening.clone(), None))).await;
                for (i, o) in outs.iter().enumerate() {
                    record(i, round, o, &mut log, &mut sessions, &mut steps, &mut rounds_json);
                }
            } else {
                // 2라운드부터는 차례로 말한다: 동시에 말하면 서로의 지난 입장에 답하느라 입장이 엇갈리기만 한다.
                for i in 0..n {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let prompt = if sessions[i].is_some() {
                        let others: Vec<(String, &str)> = log[heard[i]..].iter().filter(|l| l.1 != i).map(|(r, j, t)| (format!("{r}라운드 · {}", names[*j]), t.as_str())).collect();
                        council_followup(round, &others)
                    } else {
                        let full: Vec<(u8, String, &str)> = log.iter().map(|(r, j, t)| (*r, names[*j].clone(), t.as_str())).collect();
                        council_full(&p.prompt, round, &names[i], &full)
                    };
                    let o = turn(i, round, prompt, sessions[i].clone()).await;
                    record(i, round, &o, &mut log, &mut sessions, &mut steps, &mut rounds_json);
                    heard[i] = log.len();
                }
            }
            // 각 참가자의 가장 최근 발언(이번 라운드에 실패했으면 그 전 발언)
            let mut latest: Vec<(usize, &str)> = (0..n).filter_map(|i| log.iter().rev().find(|l| l.1 == i).map(|l| (i, l.2.as_str()))).collect();
            latest.retain(|(i, _)| log.iter().any(|l| l.1 == *i && l.0 == round));
            if latest.len() < 2 {
                break;
            }
            // 합의 확인: JEV가 있으면 JEV가(1라운드부터), 없으면 2라운드부터 모든 발언의 '합의: 예'로 정한다.
            if let Some(key) = jev_key.as_ref().filter(|_| !cancel.is_cancelled()) {
                let texts: Vec<&str> = latest.iter().map(|(_, t)| strip_agree(t)).collect();
                if let Ok(c) = compare_answers(&st.http, key, &p.prompt, &texts).await {
                    jev_note("council", c.agree, c.ms, c.cost_usd);
                    if c.agree >= JEV_AGREE {
                        agreed = Some((c.agree, latest[c.best].0));
                        break;
                    }
                }
            } else if round >= 2 && latest.iter().all(|(_, t)| says_agree(t)) {
                agreed = Some((1.0, latest[latest.len() - 1].0));
                break;
            }
        }
        let agents_json: Vec<Value> = cn.agents.iter().map(|a| json!({ "engine": a.engine, "model": a.model })).collect();
        let council_json = |synth: Option<EngineId>, agree: Option<f64>| json!({ "agents": agents_json, "rounds": rounds_json, "synth": synth, "agreed": agree.is_some(), "agree": agree, "maxRounds": cn.rounds });
        let Some(last_round) = log.last().map(|l| l.0) else {
            let why = if cancel.is_cancelled() { "중지했습니다.".to_string() } else { steps.iter().rev().find_map(|x| x.error.clone()).unwrap_or_else(|| "답을 받지 못했습니다".into()) };
            return finish(format!("⚠ {why}"), message_data(&steps, &p, json!({ "error": why, "council": council_json(None, None) })));
        };
        let last: Vec<&(u8, usize, String)> = log.iter().filter(|l| l.0 == last_round).collect();
        if let Some((agree, best)) = agreed {
            let text = log.iter().rev().find(|l| l.1 == best).map(|l| strip_agree(&l.2).to_string()).unwrap_or_default();
            let mut p2 = p.clone();
            p2.reasons.push(format!(
                "{round}라운드에서 조기 합의(일치율 {pct}%)되어 요약 단계를 생략하고 {who}의 답변을 표시합니다.",
                round = last_round,
                pct = pct(agree),
                who = names[best]
            ));
            return finish(text, message_data(&steps, &p2, json!({ "draftedBy": cn.agents[best].engine, "council": council_json(None, Some(agree)) })));
        }
        if last.len() == 1 || cancel.is_cancelled() {
            let (_, i, t) = last[0];
            let mut p2 = p.clone();
            p2.reasons.push(format!(
                "회의가 조기 종료되어 추가 요약 없이 {who}의 마지막 답변을 표시합니다.",
                who = names[*i]
            ));
            return finish(strip_agree(t).to_string(), message_data(&steps, &p2, json!({ "draftedBy": cn.agents[*i].engine, "council": council_json(None, None) })));
        }
        let full: Vec<(u8, String, &str)> = log.iter().map(|(r, i, t)| (*r, names[*i].clone(), t.as_str())).collect();
        let sp = council_synth(&p.prompt, last_round, &full);
        let j = run_engine(app, st, conv, task, &mid, "synth", spec_for(cn.synth, ctx(None, &sp), base_sys.clone(), false), timeout, &cancel).await;
        steps.push(step(cn.synth, "synth", &j, &spec_effort(&p), spec_model(&p, cn.synth, &s)));
        let first = last[0];
        if good(&j) {
            return finish(j.text.clone(), message_data(&steps, &p, json!({ "synthBy": cn.synth, "draftedBy": cn.agents[first.1].engine, "council": council_json(Some(cn.synth), None) })));
        }
        let mut p2 = p.clone();
        p2.reasons.push(format!(
            "{synth}의 회의 요약 생성에 실패하여 {who}의 마지막 답변을 대신 표시합니다.",
            synth = cn.synth.label(),
            who = names[first.1]
        ));
        return finish(strip_agree(&first.2).to_string(), message_data(&steps, &p2, json!({ "draftedBy": cn.agents[first.1].engine, "council": council_json(None, None) })));
    }

    // 동시 에이전트: 여러 AI가 동시에 따로 답하고 → 종합 AI가 하나로 정리한다.
    if let Some(team) = p.team.clone().filter(|_| !p.work) {
        let n = team.agents.len();
        let tsys = format!("{}{}", system_prompt(false, &ws, p.skill.as_ref()), team_system(n));
        emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "agents", "text": format!("{n}개의 에이전트가 동시에 답변을 생성하는 중") }));
        let q = ctx(None, &p.prompt);
        let runs = team.agents.iter().enumerate().map(|(i, a)| {
            let mut spec = spec_for(a.engine, q.clone(), tsys.clone(), false);
            if a.model.is_some() {
                spec.model = a.model.clone();
            }
            if team.agent_effort.is_some() {
                spec.effort = team.agent_effort.clone();
            }
            let (key, mid, cancel) = (format!("agent:{i}"), &mid, &cancel);
            async move { run_engine_as(app, st, conv, task, mid, "agent", spec, timeout, cancel, false, &key).await }
        });
        let outs: Vec<Outcome> = futures_util::future::join_all(runs).await;
        let good = |o: &Outcome| o.ok && !o.text.trim().is_empty();
        let who = |a: &TeamAgent| match &a.model {
            Some(m) => format!("{} · {m}", a.engine.label()),
            None => a.engine.label().to_string(),
        };
        for (a, o) in team.agents.iter().zip(&outs) {
            steps.push(step(a.engine, "agent", o, &spec_effort(&p), a.model.clone().or_else(|| spec_model(&p, a.engine, &s))));
        }
        let agents_json: Vec<Value> = team.agents.iter().zip(&outs).map(|(a, o)| json!({ "engine": a.engine, "model": a.model, "ok": good(o), "text": o.text })).collect();
        let ok: Vec<(&TeamAgent, &Outcome)> = team.agents.iter().zip(&outs).filter(|(_, o)| good(o)).collect();
        if ok.is_empty() {
            let why = if cancel.is_cancelled() { "중지했습니다.".to_string() } else { outs.iter().find_map(|o| o.error.clone()).unwrap_or_else(|| "답을 받지 못했습니다".into()) };
            return finish(format!("⚠ {why}"), message_data(&steps, &p, json!({ "error": why, "team": { "agents": agents_json, "synth": null } })));
        }
        if ok.len() == 1 || cancel.is_cancelled() {
            let (a, o) = ok[0];
            let label = who(a);
            let mut p2 = p.clone();
            p2.reasons.push(format!("단일 답변만 완료되어 종합 과정 없이 {label}의 결과를 그대로 표시합니다."));
            return finish(o.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": a.engine, "team": { "agents": agents_json, "synth": null } })));
        }
        // 답들이 같은 결론이면 종합 AI를 부르지 않고 JEV가 고른 가장 좋은 답을 쓴다.
        if let Some(key) = &jev_key {
            let texts: Vec<&str> = ok.iter().map(|(_, o)| o.text.as_str()).collect();
            if let Ok(c) = compare_answers(&st.http, key, &p.prompt, &texts).await {
                jev_note("compare", c.agree, c.ms, c.cost_usd);
                if c.agree >= JEV_AGREE {
                    let (a, o) = ok[c.best];
                    let mut p2 = p.clone();
                    p2.reasons.push(format!(
                        "JEV: 에이전트 간 결론이 일치(합의율 {pct}%)하여 추가 종합을 생략하고 {who}의 답변을 표시합니다.",
                        pct = pct(c.agree),
                        who = who(a)
                    ));
                    return finish(o.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": a.engine, "team": { "agents": agents_json, "synth": null } })));
                }
            }
        }
        let answers: Vec<(String, &str)> = ok.iter().map(|(a, o)| (who(a), o.text.as_str())).collect();
        let sp = team_prompt(&p.prompt, &answers);
        let j = run_engine(app, st, conv, task, &mid, "synth", spec_for(team.synth, ctx(None, &sp), system_prompt(false, &ws, p.skill.as_ref()), false), timeout, &cancel).await;
        steps.push(step(team.synth, "synth", &j, &spec_effort(&p), spec_model(&p, team.synth, &s)));
        let team_json = json!({ "agents": agents_json, "synth": team.synth });
        if good(&j) {
            return finish(j.text.clone(), message_data(&steps, &p, json!({ "synthBy": team.synth, "draftedBy": ok[0].0.engine, "team": team_json })));
        }
        let (synth_label, first) = (team.synth.label(), who(ok[0].0));
        let mut p2 = p.clone();
        p2.reasons.push(format!("{synth_label}의 답변 종합에 실패하여 {first}의 결과를 대신 표시합니다."));
        return finish(ok[0].1.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": ok[0].0.engine, "team": team_json })));
    }

    // 토론: 두 AI가 동시에 따로 답하고 → (보통·깊게면) 서로 반론하고 → 판정 AI가 하나로 정리한다.
    if let Some(dbt) = p.debate.clone() {
        let dsys = format!("{}{DEBATE_SYSTEM}", system_prompt(false, &ws, p.skill.as_ref()));
        emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "debate", "text": format!("{}와 {}가 따로 답하는 중", dbt.a.label(), dbt.b.label()) }));
        let q = ctx(None, &p.prompt);
        let (ra, rb) = tokio::join!(
            run_engine_as(app, st, conv, task, &mid, "debate", spec_for(dbt.a, q.clone(), dsys.clone(), false), DEBATE_ROUND1, &cancel, false, "debate:a"),
            run_engine_as(app, st, conv, task, &mid, "debate", spec_for(dbt.b, q.clone(), dsys.clone(), false), DEBATE_ROUND1, &cancel, false, "debate:b"),
        );
        steps.push(step(dbt.a, "debate", &ra, &spec_effort(&p), spec_model(&p, dbt.a, &s)));
        steps.push(step(dbt.b, "debate", &rb, &spec_effort(&p), spec_model(&p, dbt.b, &s)));
        let good = |o: &Outcome| o.ok && !o.text.trim().is_empty();
        let mut rounds = vec![json!({ "round": 1, "engine": dbt.a, "text": ra.text }), json!({ "round": 1, "engine": dbt.b, "text": rb.text })];
        match (good(&ra), good(&rb)) {
            (false, false) => {
                let why = if cancel.is_cancelled() { "중지했습니다.".to_string() } else { ra.error.clone().or(rb.error.clone()).unwrap_or_else(|| "답을 받지 못했습니다".into()) };
                return finish(format!("⚠ {why}"), message_data(&steps, &p, json!({ "error": why })));
            }
            (true, false) | (false, true) => {
                // 한쪽만 답했으면 토론 없이 그 답을 쓴다.
                let (e, o) = if good(&ra) { (dbt.a, &ra) } else { (dbt.b, &rb) };
                let note = format!("{}가 답하지 못해 토론 없이 {}의 답을 씁니다", if good(&ra) { dbt.b.label() } else { dbt.a.label() }, e.label());
                let mut p2 = p.clone();
                p2.reasons.push(note);
                return finish(o.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": e, "debate": { "participants": [dbt.a, dbt.b], "judge": null, "rounds": rounds } })));
            }
            (true, true) => {}
        }
        // 두 답이 이미 같은 결론이면 반론·판정(AI 호출 3번)을 건너뛰고 JEV가 고른 더 좋은 답을 쓴다.
        if let Some(key) = jev_key.as_ref().filter(|_| !cancel.is_cancelled()) {
            if let Ok(c) = compare_answers(&st.http, key, &p.prompt, &[&ra.text, &rb.text]).await {
                jev_note("compare", c.agree, c.ms, c.cost_usd);
                if c.agree >= JEV_AGREE {
                    let (e, o) = if c.best == 1 { (dbt.b, &rb) } else { (dbt.a, &ra) };
                    let mut p2 = p.clone();
                    p2.reasons.push(format!(
                        "JEV: 초기 답변의 결론이 일치(합의율 {pct}%)하여 반론 및 판정을 생략하고 {who}의 답변을 채택합니다.",
                        pct = pct(c.agree),
                        who = e.label()
                    ));
                    return finish(o.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": e, "debate": { "participants": [dbt.a, dbt.b], "judge": null, "rounds": rounds } })));
                }
            }
        }
        let (mut fa, mut fb) = (ra.text.clone(), rb.text.clone());
        let mut rebuttals: Option<(String, String)> = None;
        if dbt.rebuttal && !cancel.is_cancelled() {
            emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "rebuttal", "text": "서로의 답을 읽고 반론하는 중" }));
            let (xa, xb) = tokio::join!(
                run_engine_as(app, st, conv, task, &mid, "rebuttal", spec_for(dbt.a, ctx(None, &rebuttal_prompt(&p.prompt, &ra.text, dbt.b.label(), &rb.text)), dsys.clone(), false), DEBATE_REBUTTAL, &cancel, false, "rebuttal:a"),
                run_engine_as(app, st, conv, task, &mid, "rebuttal", spec_for(dbt.b, ctx(None, &rebuttal_prompt(&p.prompt, &rb.text, dbt.a.label(), &ra.text)), dsys.clone(), false), DEBATE_REBUTTAL, &cancel, false, "rebuttal:b"),
            );
            steps.push(step(dbt.a, "rebuttal", &xa, &spec_effort(&p), spec_model(&p, dbt.a, &s)));
            steps.push(step(dbt.b, "rebuttal", &xb, &spec_effort(&p), spec_model(&p, dbt.b, &s)));
            rounds.push(json!({ "round": 2, "engine": dbt.a, "text": xa.text }));
            rounds.push(json!({ "round": 2, "engine": dbt.b, "text": xb.text }));
            if good(&xa) {
                fa = revised_part(&xa.text).to_string();
            }
            if good(&xb) {
                fb = revised_part(&xb.text).to_string();
            }
            if good(&xa) && good(&xb) {
                rebuttals = Some((xa.text.clone(), xb.text.clone()));
            }
        }
        let jp = judge_prompt(&p.prompt, dbt.a.label(), &fa, dbt.b.label(), &fb, rebuttals.as_ref().map(|(x, y)| (x.as_str(), y.as_str())));
        let j = if cancel.is_cancelled() { Outcome::default() } else { run_engine(app, st, conv, task, &mid, "judge", spec_for(dbt.judge, ctx(None, &jp), system_prompt(false, &ws, p.skill.as_ref()), false), timeout, &cancel).await };
        steps.push(step(dbt.judge, "judge", &j, &spec_effort(&p), spec_model(&p, dbt.judge, &s)));
        let debate = json!({ "participants": [dbt.a, dbt.b], "judge": dbt.judge, "rounds": rounds });
        if good(&j) {
            return finish(j.text.clone(), message_data(&steps, &p, json!({ "judgedBy": dbt.judge, "draftedBy": dbt.a, "debate": debate })));
        }
        let mut p2 = p.clone();
        p2.reasons.push(format!("{}가 판정하지 못해 {}의 답을 씁니다", dbt.judge.label(), dbt.a.label()));
        return finish(fa, message_data(&steps, &p2, json!({ "draftedBy": dbt.a, "debate": debate })));
    }

    let steer: Steer = st.steers.lock().unwrap_or_else(|p| p.into_inner()).get(task).cloned().unwrap_or_default();
    let mut draft: Option<(EngineId, Outcome)> = None;
    let mut last_spec: Option<RunSpec> = None;
    'engines: for engine in std::iter::once(p.primary).chain(p.fallbacks.iter().copied()) {
        let mut retried = false;
        loop {
            if cancel.is_cancelled() {
                break 'engines;
            }
            // Claude Code·Antigravity·OpenCode는 같은 대화의 세션을 이어 쓰고, 나머지는 최근 대화를 요청에 붙인다.
            let resume = if engine.resumable() { store::get_session(&st.db, conv, engine.as_str()).ok().flatten() } else { None };
            let model = model_for(&p, engine, &s);
            // 자기 세션을 이어 쓰면 못 본 차례만, 아니면 세션 요약 + 최근 대화를 붙인다.
            let seen = if resume.is_some() { store::get_seen(&st.db, conv, engine.as_str()).ok().flatten() } else { None };
            let prompt = ctx(seen.as_deref(), &p.prompt);
            // 작업 중 메시지는 API 작업 모드에서만 받는다(CLI는 중간에 끼어들 수 없다).
            *steer.lock().unwrap_or_else(|p| p.into_inner()) = (engine.is_api() && p.work).then(Vec::new);
            let spec = RunSpec {
                engine,
                prompt,
                system: system.clone(),
                cwd: workspace.clone(),
                work: p.work,
                resume,
                plugin_dirs: if engine == EngineId::Claude { plugin_dirs.clone() } else { vec![] },
                model,
                effort: p.budget.as_ref().map(|b| b.effort.to_string()),
                max_tokens: p.budget.as_ref().map(|b| b.max_tokens),
                extra_dirs: skill_dirs.clone(),
                steer: Some(steer.clone()),
                attachments: files.clone(),
            };
            let o = run_engine(app, st, conv, task, &mid, "draft", spec.clone(), timeout, &cancel).await;
            // 에이전트 탭에서 이 AI만 멈췄으면 다음 AI로 넘어가지 않는다.
            let stopped = o.error.as_deref() == Some(AGENT_STOPPED);
            let left = steer.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or_default();
            if !left.is_empty() {
                emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "draft", "text": format!("작업이 끝나 전달하지 못한 메시지 {}개가 있습니다. 다시 보내 주세요", left.len()) }));
            }
            steps.push(step(engine, "draft", &o, &spec_effort(&p), spec_model(&p, engine, &s)));
            if let Some(sid) = &o.session_id {
                if o.ok && engine.resumable() {
                    let _ = store::set_session(&st.db, conv, engine.as_str(), sid);
                }
            }
            if o.ok && !o.text.trim().is_empty() {
                last_spec = Some(spec);
                draft = Some((engine, o));
                break 'engines;
            }
            if stopped || cancel.is_cancelled() {
                break 'engines;
            }
            // 일시적인 오류면 같은 AI로 한 번 더 시도한다. 이미 도구를 쓴 작업은 파일을 두 번 고칠 수 있어 다시 돌리지 않는다.
            let error = o.error.clone().unwrap_or_default();
            // 응답 없이 멈췄던 AI는 다시 돌려도 또 멈추기 쉬우므로 바로 다음 AI로 넘어간다.
            if !retried && !error.is_empty() && !o.stalled && (!p.work || o.activities.is_empty()) {
                retried = true;
                let worth = match &jev_key {
                    Some(key) => match failure_transient(&st.http, key, engine.label(), &error).await {
                        Ok((x, ms, cost)) => {
                            jev_note("triage", x, ms, cost);
                            x >= JEV_RETRY
                        }
                        Err(_) => crate::apis::transient(&error),
                    },
                    None => crate::apis::transient(&error),
                };
                if worth {
                    emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "draft", "text": format!("{} 일시적인 오류로 다시 시도합니다", engine.label()) }));
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    continue;
                }
            }
            // 키가 거절된 API는 키를 다시 넣을 때까지 자동 선택에서 뺀다(매번 실패를 거치지 않도록).
            if engine.is_api() && o.error.as_deref().is_some_and(|e| e.contains("(401)") || e.contains("(403)")) {
                if let Some(e) = st.engines.write().unwrap_or_else(|p| p.into_inner()).iter_mut().find(|e| e.id == engine) {
                    e.installed = false;
                }
                emit(app, "engines:changed", json!({}));
            }
            if !p.fallbacks.is_empty() {
                emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "draft", "text": format!("{} 실패, 다음 AI로 넘어갑니다", engine.label()) }));
            }
            break;
        }
    }
    let Some((drafted_by, mut d)) = draft else {
        let why = if cancel.is_cancelled() { "중지했습니다.".to_string() } else { steps.last().and_then(|s| s.error.clone()).unwrap_or_else(|| "답을 받지 못했습니다".into()) };
        return finish(format!("⚠ {why}"), message_data(&steps, &p, json!({ "error": why })));
    };

    // 작업: JEV가 보기에 끝나지 않았으면 같은 AI에게 한 차례 더 맡긴다.
    let mut notes: Vec<String> = vec![];
    if let (true, Some(key), Some(spec)) = (p.work && !cancel.is_cancelled(), &jev_key, &last_spec) {
        emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "draft", "text": "JEV가 작업이 끝났는지 확인하는 중" }));
        if let Ok((x, ms, cost)) = work_unfinished(&st.http, key, &p.prompt, &d.text).await {
            jev_note("verify", x, ms, cost);
            if x >= JEV_UNFINISHED {
                notes.push(format!("JEV: 작업이 끝나지 않았다고 판단({pct}%)해 {who}에게 이어서 맡겼습니다", pct = pct(x), who = drafted_by.label()));
                let msg = "작업이 아직 끝나지 않은 것으로 보입니다. 남은 부분을 끝내고 실제로 동작하는지 확인하세요.".to_string();
                let next = follow_up(spec, &p.prompt, &d, &[msg]);
                let r = run_engine(app, st, conv, task, &mid, "finish", next, timeout, &cancel).await;
                steps.push(step(drafted_by, "finish", &r, &spec_effort(&p), spec_model(&p, drafted_by, &s)));
                if let (true, Some(sid)) = (r.ok && drafted_by.resumable(), &r.session_id) {
                    let _ = store::set_session(&st.db, conv, drafted_by.as_str(), sid);
                }
                if r.ok && !r.text.trim().is_empty() {
                    d.text = r.text;
                }
            }
        }
    }
    // 대화: JEV로 초안을 확인한다. 이미 충분하면 검토를 건너뛰고, 검토 계획이 없어도 답이 약해 보이면 다른 AI에게 맡긴다.
    let mut reviewer = p.reviewer;
    let may_escalate = reviewer.is_none() && !p.work && s.review == ReviewMode::Auto && choice.is_none();
    let mut ready: Option<f64> = None;
    if let (true, Some(key)) = ((reviewer.is_some() || may_escalate) && !cancel.is_cancelled(), &jev_key) {
        emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "review", "text": "JEV가 초안 검토 생략 여부를 판단하는 중" }));
        if let Ok((x, ms, cost)) = draft_ready(&st.http, key, &p.prompt, &d.text).await {
            jev_note("draft", x, ms, cost);
            ready = Some(x);
            if may_escalate && x < JEV_ESCALATE {
                reviewer = stronger_reviewer(drafted_by, &st.installed());
                if let Some(r) = reviewer {
                    notes.push(format!("JEV: 초안이 충분하지 않다고 판단(완성도 {pct}%)해 {who}에게 검토를 맡겼습니다", pct = pct(x), who = r.label()));
                }
            }
        }
    }
    let pn = {
        let mut x = p.clone();
        x.reasons.extend(notes.iter().cloned());
        x
    };
    // 검토: 다른 AI가 초안을 다듬는다(실패하면 초안을 그대로 쓴다).
    if let Some(rev) = reviewer.filter(|_| !cancel.is_cancelled()) {
        // 초안이 이미 완성됐다고 JEV가 확신하면 검토 AI를 부르지 않는다.
        if let Some(x) = ready.filter(|x| *x >= JEV_DRAFT_OK) {
            let mut p2 = pn.clone();
            p2.reasons.push(format!(
                "JEV: 초안의 완성도가 충분하다고 판단(신뢰도 {pct}%)하여 {rev}의 검토를 생략합니다.",
                rev = rev.label(),
                pct = pct(x)
            ));
            finish(d.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": drafted_by, "skippedReview": rev })));
            mark_seen(st, conv, drafted_by);
            return;
        }
        let spec = RunSpec {
            engine: rev,
            prompt: ctx(None, &review_prompt(&p.prompt, &d.text, &d.activities)),
            system: system_prompt(false, &ws, p.skill.as_ref()),
            cwd: workspace.clone(),
            work: false,
            resume: None,
            plugin_dirs: vec![],
            model: model_for(&p, rev, &s),
            effort: p.budget.as_ref().map(|b| b.effort.to_string()),
            max_tokens: p.budget.as_ref().map(|b| b.max_tokens),
            extra_dirs: skill_dirs.clone(),
            steer: None,
            attachments: files.clone(),
        };
        let r = run_engine(app, st, conv, task, &mid, "review", spec, timeout, &cancel).await;
        steps.push(step(rev, "review", &r, &spec_effort(&p), spec_model(&p, rev, &s)));
        if r.ok && !r.text.trim().is_empty() && !review_is_degraded(&d.text, &r.text) {
            // 검토가 오히려 답을 망쳤는지 JEV로 한 번 더 본다.
            if let Some(key) = jev_key.as_ref().filter(|_| !cancel.is_cancelled()) {
                if let Ok((x, ms, cost)) = review_worse(&st.http, key, &p.prompt, &d.text, &r.text).await {
                    jev_note("reviewCheck", x, ms, cost);
                    if x >= JEV_KEEP_DRAFT {
                        let mut p2 = pn.clone();
                        p2.reasons.push(format!("JEV: {who}의 검토가 초안보다 못하다고 판단({pct}%)해 초안을 씁니다", who = rev.label(), pct = pct(x)));
                        finish(d.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": drafted_by, "rejectedReview": r.text })));
                        mark_seen(st, conv, drafted_by);
                        return;
                    }
                }
            }
            mark_seen(st, conv, drafted_by);
            return finish(r.text.clone(), message_data(&steps, &pn, json!({ "draft": d.text, "draftedBy": drafted_by, "reviewedBy": rev })));
        }
        if r.ok && review_is_degraded(&d.text, &r.text) {
            let mut p2 = pn.clone();
            p2.reasons.push(format!("{}의 검토가 초안보다 지나치게 짧거나 내용을 지워 초안을 그대로 씁니다", rev.label()));
            finish(d.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": drafted_by, "rejectedReview": r.text })));
            mark_seen(st, conv, drafted_by);
            return;
        }
    }
    finish(d.text.clone(), message_data(&steps, &pn, json!({ "draftedBy": drafted_by })));
    mark_seen(st, conv, drafted_by);
}

/// 첨부를 읽지 못하는 API가 먼저 답하게 되면 파일을 읽을 수 있는 CLI를 앞세운다(직접 고른 AI는 그대로).
fn prefer_file_readers(p: &mut Plan, files: &[AttachedFile], choice: Option<EngineId>) {
    if choice.is_some() || !p.primary.is_api() {
        return;
    }
    let ok = crate::apis::can_send_binary(p.primary);
    if files.iter().all(|f| f.kind == "text" || ok(f)) {
        return;
    }
    if let Some(i) = p.fallbacks.iter().position(|e| !e.is_api()) {
        let cli = p.fallbacks.remove(i);
        p.fallbacks.insert(0, p.primary);
        p.primary = cli;
        p.reasons.push(format!("첨부 파일을 읽을 수 있는 {}가 먼저 답합니다", cli.label()));
    }
}

/// 병렬 작업에 필요한 작업 정보
struct TeamWork<'a> {
    app: &'a AppHandle,
    st: &'a AppState,
    conv: &'a str,
    task: &'a str,
    mid: &'a str,
    workspace: &'a std::path::Path,
    settings: &'a Settings,
    /// 작업(만들기) 시스템 지시
    system: &'a str,
    timeout: Duration,
    cancel: &'a CancellationToken,
    extra_dirs: &'a [String],
    plugin_dirs: &'a [String],
    /// true면 OpenCode가 로드맵(하위 작업마다 맡을 AI와 노력 단계)을 짠다
    roadmap: bool,
    /// Worker turn limit from the work setup panel (None: the default)
    max_turns: Option<u32>,
    files: &'a [AttachedFile],
}

impl TeamWork<'_> {
    fn act(&self, text: &str) {
        emit(self.app, "task:activity", json!({ "taskId": self.task, "conversationId": self.conv, "phase": "agents", "text": text }));
    }
}

/// 병렬 워커의 세션 메모리: 최근 대화는 빼고 요약만 준다.
/// 워커는 전체 요청과 자기 하위 작업을 따로 받으므로 지난 대화 전문이 필요 없고, 첫 글은 모든 단계에서 다시 읽힌다.
const WORKER_MEMORY: &str = memory::SUMMARY_ONLY;

/// 나누는 AI 한 번에 기다리는 최대 시간
const SPLIT_TIMEOUT: Duration = Duration::from_secs(300);
const TEAM_READ_TIMEOUT: Duration = Duration::from_secs(90);
/// 에이전트 수를 정할 때 함께 읽는 앞선 대화 차례 수와 차례마다의 최대 글자 수
const AGENT_COUNT_TURNS: usize = 6;
const AGENT_COUNT_TURN_CHARS: usize = 600;
/// 로드맵을 짜는 OpenCode 한 번에 기다리는 최대 시간
const ROADMAP_TIMEOUT: Duration = Duration::from_secs(120);

fn remove_agent_dirs(root: PathBuf, tmp: PathBuf) {
    tokio::task::spawn_blocking(move || teamwork::remove_dirs(&root, &tmp));
}

/// 요청문에 적힌 팀을 나눈 결과가 지켰을 확률
async fn groups_kept(http: &reqwest::Client, key: &str, request: &str, parts: &[teamwork::Part]) -> Result<(f64, u64, f64), String> {
    let plan: Vec<Value> = parts.iter().map(|x| json!({ "title": x.title, "members": x.members })).collect();
    let state = json!({ "request": cut(request, 6000), "plan": plan });
    let mut q = serde_json::Map::new();
    q.insert("groups".into(), json!({"type": "choice", "instructions": "`request` may name groups of workers, for example \"workers 1 and 2 are one group\". `plan` is how the work was split; members above 1 means a group of that size. Did the plan keep the groups the user wrote?", "criteria": {
        "kept": "The user wrote no groups, left grouping to the lead, or the plan has the group sizes the user wrote.",
        "broken": "The user wrote specific groups and the plan has different group sizes or none."
    }}));
    let r = jev::ask(http, key, "jev-latest", state, q, 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let p = jev::prob(&r.response.answers, "groups", "kept").ok_or("JEV returned no groups")?;
    Ok((p, r.latency_ms, r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6))
}

/// 총괄 등록. 작업이 끝나면 지우고 남은 질문과 대기를 끝낸다.
struct LeadEntry {
    st: std::sync::Arc<AppState>,
    lead: std::sync::Arc<crate::leadrun::TaskLead>,
}

impl Drop for LeadEntry {
    fn drop(&mut self) {
        self.lead.cancel.cancel();
        self.st.leads.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.lead.task);
    }
}

/// 팀 구성표를 뽑을 AI. 빠른 CLI만 쓴다(사용자가 고르지 않은 API를 부르지 않는다).
fn team_reader(installed: &[EngineId]) -> Option<(EngineId, Option<String>)> {
    if installed.contains(&EngineId::Claude) {
        return Some((EngineId::Claude, Some("haiku".to_string())));
    }
    if installed.contains(&EngineId::Antigravity) {
        return Some((EngineId::Antigravity, Some("gemini-3.8-flash".to_string())));
    }
    installed.iter().find(|e| !e.is_api()).map(|e| (*e, None))
}

/// 적힌 팀 크기대로 자리를 앞에서부터 묶는다. 하위 작업이 팀보다 적으면 그만큼만 돈다.
fn written_units(agents: &[TeamAgent], sizes: &[usize], parts: usize) -> Vec<Vec<TeamAgent>> {
    let mut seats = agents.iter().cloned();
    sizes.iter().take(parts).map(|n| seats.by_ref().take(*n).collect()).collect()
}

/// 하위 작업을 맡길 수 있는 AI: CLI가 있으면 CLI만 쓴다(사용자가 고르지 않은 API를 부르지 않는다).
fn worker_pool(installed: Vec<EngineId>) -> Vec<EngineId> {
    if installed.iter().any(|e| !e.is_api()) {
        installed.into_iter().filter(|e| !e.is_api()).collect()
    } else {
        installed
    }
}

fn seat_label(a: &TeamAgent) -> String {
    match &a.model {
        Some(m) => format!("{} · {m}", a.engine.label()),
        None => a.engine.label().to_string(),
    }
}

/// 계획 안내에 쓰는 배정 한 줄: "화면 → Claude Code(깊게)"
fn roadmap_line(title: &str, engine: EngineId, tier: Option<&Budget>) -> String {
    match tier {
        Some(b) => format!("{title} → {}({})", engine.label(), tier_label(b.tier)),
        None => format!("{title} → {}", engine.label()),
    }
}

/// OpenCode에게 로드맵(하위 작업마다 맡을 AI와 노력 단계)을 묻는다. 읽을 수 없으면 None(처음 계획대로 배정한다).
async fn plan_roadmap(tw: &TeamWork<'_>, p: &Plan, key: &str, parts: &[teamwork::Part], pool: &[EngineId], steps: &mut Vec<Step>) -> Option<Vec<roadmap::Pick>> {
    let s = tw.settings;
    tw.act("OpenCode가 로드맵을 작성하는 중");
    let briefs: Vec<(EngineId, &str)> = pool.iter().map(|e| (*e, engine_brief(*e))).collect();
    let spec = RunSpec {
        engine: EngineId::OpenCode,
        prompt: roadmap::assign_prompt(&p.prompt, parts, &briefs),
        system: system_prompt(false, &tw.workspace.to_string_lossy(), None),
        cwd: tw.workspace.to_path_buf(),
        work: false,
        resume: None,
        plugin_dirs: vec![],
        model: model_for(p, EngineId::OpenCode, s),
        effort: None,
        max_tokens: None,
        extra_dirs: vec![],
        steer: None,
        attachments: vec![],
    };
    let o = run_engine_as(tw.app, tw.st, tw.conv, tw.task, tw.mid, "roadmap", spec, ROADMAP_TIMEOUT, tw.cancel, false, key).await;
    steps.push(step(EngineId::OpenCode, "roadmap", &o, &None, spec_model(p, EngineId::OpenCode, s)));
    let picks = o.ok.then(|| roadmap::parse_assign(&o.text, parts.len(), pool)).flatten();
    if picks.is_none() && !tw.cancel.is_cancelled() {
        tw.act("⚠️ OpenCode 응답을 읽지 못해 기존 계획대로 에이전트를 할당합니다.");
    }
    picks
}

/// 로드맵 한 단계: 하위 작업마다 일하는 자리와 노력 단계
struct Stage {
    parts: Vec<teamwork::Part>,
    /// 하위 작업마다 일하는 자리(둘 이상이면 팀)
    units: Vec<Vec<TeamAgent>>,
    /// 하위 작업마다의 노력 단계(None이면 처음 계획)
    tiers: Vec<Option<Budget>>,
    /// 자리마다 요청 글에 적힌 노력치
    efforts: Vec<Vec<Option<String>>>,
    /// 실행 key 앞부분: 첫 단계 "agent:", 다음 단계 "agent:s2-"
    prefix: String,
}

impl Stage {
    fn key(&self, i: usize, m: usize) -> String {
        if self.units[i].len() > 1 {
            format!("{}{i}.{m}", self.prefix)
        } else {
            format!("{}{i}", self.prefix)
        }
    }

    /// 오른쪽 창에 보이는 하위 작업 카드
    fn views(&self) -> Vec<Value> {
        self.parts.iter().enumerate().flat_map(|(i, x)| (0..self.units[i].len()).map(move |m| (i, m, x))).map(|(i, m, x)| json!({ "key": self.key(i, m), "title": x.title, "files": x.files })).collect()
    }
}

/// 하위 작업 하나의 결과(모든 단계에 걸쳐 쌓는다)
struct PartRecord {
    title: String,
    agent: TeamAgent,
    out: Outcome,
    files: Vec<String>,
}

impl PartRecord {
    fn ok(&self) -> bool {
        self.out.ok && !self.out.text.trim().is_empty()
    }
}

fn team_record(records: &[PartRecord], synth: Option<EngineId>, conflicts: &[String], lead: &crate::leadrun::TaskLead) -> Value {
    // Reviews and the commands agents ran themselves, in time order
    let mut decisions = lead.decisions.lock().unwrap_or_else(|e| e.into_inner()).clone();
    decisions.extend(lead.ran.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned());
    decisions.sort_by(|a, b| a["at"].as_str().unwrap_or("").cmp(b["at"].as_str().unwrap_or("")));
    json!({
        "agents": records.iter().map(|r| json!({ "engine": r.agent.engine, "model": r.agent.model, "ok": r.ok(), "text": r.out.text, "title": r.title, "files": r.files })).collect::<Vec<_>>(),
        "synth": synth,
        "work": true,
        "conflicts": conflicts,
        "decisions": decisions,
        "groupLog": *lead.group_log.lock().unwrap_or_else(|e| e.into_inner()),
        "leadLog": *lead.timeline.lock().unwrap_or_else(|e| e.into_inner()),
        "orgLog": *lead.org_log.lock().unwrap_or_else(|e| e.into_inner()),
    })
}

/// 지금 폴더의 파일 목록을 읽는다(사본·변경 비교의 기준).
async fn snapshot_of(root: &std::path::Path) -> Result<std::sync::Arc<teamwork::Snapshot>, String> {
    let r = root.to_path_buf();
    tokio::task::spawn_blocking(move || teamwork::snapshot(&r)).await.map_err(|e| e.to_string()).and_then(|r| r).map(std::sync::Arc::new)
}

/// 단계의 작업 공간을 만든다. (폴더들, 작업 공간 안내)
async fn prepare_stage(root: &std::path::Path, tmp: &std::path::Path, n: usize, base: &std::sync::Arc<teamwork::Snapshot>) -> Result<(Vec<PathBuf>, String), String> {
    let (r, t, b) = (root.to_path_buf(), tmp.to_path_buf(), base.clone());
    let w = tokio::task::spawn_blocking(move || teamwork::prepare_dirs(&r, &t, n, &b)).await.map_err(|e| e.to_string()).and_then(|r| r)?;
    let note = format!("{}{}", if base.scope == teamwork::Scope::GitIgnore { teamwork::GITIGNORE_NOTE } else { "" }, if w.how == teamwork::Prepared::Worktree { teamwork::WORKTREE_NOTE } else { teamwork::COPY_NOTE });
    Ok((w.dirs, note))
}

/// 단계의 하위 작업을 동시에 돌리고 바뀐 파일을 원래 폴더에 합친다. 실패·중지한 에이전트는 합치지 않고, 중지하면 합치지 않는다.
/// 충돌 파일은 첫 판을 쓰고, 다른 판은 통합자가 읽도록 돌려준다.
#[allow(clippy::too_many_arguments)]
async fn run_stage(tw: &TeamWork<'_>, p: &Plan, stage: &Stage, dirs: &[PathBuf], tmp: &std::path::Path, space_note: &str, base: &std::sync::Arc<teamwork::Snapshot>, lead_ctx: &crate::leadrun::Ctx, ctx: &(dyn Fn(Option<&str>, &str) -> String + Sync), steps: &mut Vec<Step>) -> (Vec<PartRecord>, Vec<teamwork::ConflictView>) {
    let (s, parts) = (tw.settings, &stage.parts);
    tw.act(&format!("{n}개의 에이전트가 개별 작업 공간에서 동시 작업 중", n = parts.len()));
    // 워커가 쉴 때마다 바꾼 파일을 메인 폴더에 반영하고 총괄이 검토해 커밋한다.
    let units: Vec<crate::leadrun::UnitSpec> = parts.iter().enumerate().map(|(i, x)| ((0..stage.units[i].len()).map(|m| stage.key(i, m)).collect(), x.title.clone(), seat_label(&stage.units[i][0]), dirs[i].clone())).collect();
    let gate = std::sync::Arc::new(crate::leadrun::StageGate::new(tw.workspace.to_path_buf(), base.clone(), units));
    *lead_ctx.lead.stage.lock().unwrap_or_else(|e| e.into_inner()) = Some(gate.clone());
    let effort = spec_effort(p);
    let max_tokens = p.budget.as_ref().map(|b| b.max_tokens);
    // 무응답 한도. 큰 저장소의 하위 작업은 2026-11-02 실측 17~49분, 이후 1시간을 넘기도 해 전체 시간으로는 자르지 않는다.
    let budget = tw.timeout;
    let crew = crate::leadrun::Crew::new(parts.len());
    let crew = &crew;
    // 조직: 부서가 있으면 팀장(하위 작업의 첫 워커)끼리 짧은 보고를 주고받는다.
    let paths: Vec<Vec<String>> = parts.iter().map(|x| x.org.clone()).collect();
    let peers = teamwork::org_peers(&paths);
    let org_on = teamwork::org_active(&paths);
    *lead_ctx.lead.org.lock().unwrap_or_else(|e| e.into_inner()) = org_on.then(|| {
        let leaders = (0..parts.len()).map(|i| (stage.key(i, 0), i)).collect();
        std::sync::Arc::new(crate::leadrun::OrgBoard::new(parts.iter().map(|x| x.title.clone()).collect(), peers.clone(), leaders))
    });
    if org_on {
        let units: Vec<Value> = parts.iter().enumerate().map(|(i, x)| json!({ "index": i, "title": x.title, "org": x.org, "keys": (0..stage.units[i].len()).map(|m| stage.key(i, m)).collect::<Vec<_>>(), "peers": peers[i] })).collect();
        emit(tw.app, "task:org", json!({ "taskId": tw.task, "conversationId": tw.conv, "units": units }));
    }
    let runs = parts.iter().enumerate().map(|(i, part)| {
        let (unit, tier) = (&stage.units[i], &stage.tiers[i]);
        let specs: Vec<(String, RunSpec)> = unit
            .iter()
            .enumerate()
            .map(|(m, a)| {
                let spec = RunSpec {
                    engine: a.engine,
                    prompt: ctx(Some(WORKER_MEMORY), &teamwork::part_prompt(&p.prompt, part)),
                    system: format!(
                        "{}{}{space_note}{}{}{}{}",
                        tw.system,
                        teamwork::part_system(i, parts),
                        crate::leadrun::worker_note(a.engine),
                        if unit.len() > 1 { crate::lead::GROUP_SYSTEM } else { "" },
                        if org_on { teamwork::org_note(i, parts, &peers, unit.len() > 1) } else { String::new() },
                        if org_on && m == 0 && unit.len() > 1 && !peers[i].is_empty() { crate::lead::LEADER_REPORT_SYSTEM } else { "" },
                    ),
                    cwd: dirs[i].clone(),
                    work: true,
                    resume: None,
                    plugin_dirs: if a.engine == EngineId::Claude { tw.plugin_dirs.to_vec() } else { vec![] },
                    model: a.model.clone().or_else(|| match (tier, a.engine) {
                        (Some(b), EngineId::Claude) => b.claude_model.map(str::to_string),
                        _ => model_for(p, a.engine, s),
                    }),
                    effort: stage.efforts[i][m].clone().or_else(|| tier.as_ref().map(|b| b.effort.to_string())).or_else(|| effort.clone()),
                    max_tokens: tier.as_ref().map(|b| b.max_tokens).or(max_tokens),
                    extra_dirs: tw.extra_dirs.to_vec(),
                    steer: None,
                    attachments: tw.files.to_vec(),
                };
                (stage.key(i, m), spec)
            })
            .collect();
        let title = part.title.as_str();
        async move {
            if specs.len() > 1 {
                let o = crate::leadrun::run_group(lead_ctx, i, title, specs, budget).await;
                crew.rest();
                o
            } else {
                let (key, spec) = specs.into_iter().next().expect("하위 작업에는 워커가 있다");
                crate::leadrun::run_part_worker(lead_ctx, spec, &key, title, budget, true, Some(crew)).await
            }
        }
    });
    // 총괄이 단계 중에 워커를 더할 수 있게, 끝나는 대로 받으며 새 워커 요청도 함께 기다린다.
    use futures_util::StreamExt as _;
    type Run<'a> = futures_util::future::BoxFuture<'a, (usize, Outcome)>;
    let lead = &lead_ctx.lead;
    let (spawn_tx, mut spawn_rx) = tokio::sync::mpsc::unbounded_channel::<crate::leadrun::SpawnReq>();
    *lead.spawner.lock().unwrap_or_else(|e| e.into_inner()) = Some(spawn_tx);
    let mut running: futures_util::stream::FuturesUnordered<Run<'_>> = runs.enumerate().map(|(i, f)| -> Run<'_> { Box::pin(async move { (i, f.await) }) }).collect();
    let mut outs: Vec<Option<Outcome>> = parts.iter().map(|_| None).collect();
    let mut extras: Vec<(String, String, TeamAgent)> = vec![];
    let mut all_parts: Vec<teamwork::Part> = parts.clone();
    let worktree = tmp.join("agent-1").join(".git").is_file();
    let installed = tw.st.installed();
    loop {
        let ev = tokio::select! {
            d = running.next() => Ok(d),
            Some(r) = spawn_rx.recv() => Err(r),
        };
        let req = match ev {
            Ok(Some((i, o))) => {
                outs[i] = Some(o);
                continue;
            }
            Ok(None) => break,
            Err(req) => req,
        };
        if tw.cancel.is_cancelled() {
            crate::leadrun::spawn_refused(lead_ctx, &req.title);
            continue;
        }
        let n = extras.len() + 1;
        let key = format!("{}x{n}", stage.prefix);
        let seat = &stage.units[(n - 1) % stage.units.len()][0];
        let agent = match req.engine.filter(|e| installed.contains(e)) {
            Some(e) if e != seat.engine => TeamAgent { engine: e, model: None },
            _ => seat.clone(),
        };
        let part = teamwork::Part { title: req.title.clone(), task: req.task.clone(), files: vec![], members: 1, org: vec![] };
        all_parts.push(part.clone());
        let idx = outs.len();
        outs.push(None);
        crew.join();
        lead.workers.lock().unwrap_or_else(|e| e.into_inner()).push(crate::leadrun::WorkerInfo { key: key.clone(), title: req.title.clone(), group: None });
        emit(tw.app, "task:part-added", json!({ "taskId": tw.task, "conversationId": tw.conv, "key": key, "title": req.title, "files": [] }));
        crate::leadrun::note(lead_ctx, "consult", format!("새 워커 {t}을(를) 생성하여 작업 공간을 준비하고 작업을 시작합니다.", t = req.title));
        let mut spec = RunSpec {
            engine: agent.engine,
            prompt: ctx(Some(WORKER_MEMORY), &teamwork::part_prompt(&p.prompt, &part)),
            system: format!("{}{}{space_note}{}", tw.system, teamwork::part_system(all_parts.len() - 1, &all_parts), crate::leadrun::worker_note(agent.engine)),
            cwd: PathBuf::new(),
            work: true,
            resume: None,
            plugin_dirs: if agent.engine == EngineId::Claude { tw.plugin_dirs.to_vec() } else { vec![] },
            model: agent.model.clone().or_else(|| model_for(p, agent.engine, s)),
            effort: effort.clone(),
            max_tokens,
            extra_dirs: tw.extra_dirs.to_vec(),
            steer: None,
            attachments: tw.files.to_vec(),
        };
        let (gate, who, title, k) = (gate.clone(), seat_label(&agent), req.title.clone(), key.clone());
        let root = tw.workspace.to_path_buf();
        let tmp = tmp.to_path_buf();
        running.push(Box::pin(async move {
            // 새 공간은 지금 메인 폴더에서 시작한다(반영 중에 바뀌지 않게 메인 폴더 잠금을 잡는다).
            let made = {
                let _main = lead.merge.lock().await;
                match snapshot_of(&root).await {
                    Ok(snap) => {
                        let (r, t, name, b) = (root.clone(), tmp.clone(), format!("agent-x{n}"), snap.clone());
                        tokio::task::spawn_blocking(move || teamwork::prepare_extra(&r, &t, &name, &b, worktree)).await.map_err(|e| e.to_string()).and_then(|r| r).map(|dir| (dir, snap))
                    }
                    Err(e) => Err(e),
                }
            };
            let o = match made {
                Ok((dir, snap)) => {
                    gate.add_unit(&k, title.clone(), who, dir.clone(), snap);
                    spec.cwd = dir;
                    crate::leadrun::run_part_worker(lead_ctx, spec, &k, &title, budget, true, Some(crew)).await
                }
                Err(e) => {
                    crew.rest();
                    let error = format!("새 워커의 작업 공간을 준비하지 못했습니다 ({e})", e = e);
                    emit(tw.app, "task:agent", json!({ "taskId": tw.task, "conversationId": tw.conv, "key": k, "engine": spec.engine, "label": spec.engine.label(), "role": "agent", "state": "fail", "error": error }));
                    Outcome { ok: false, error: Some(error), ..Default::default() }
                }
            };
            (idx, o)
        }));
        extras.push((key, req.title, agent));
    }
    // 단계가 끝난 뒤 온 요청은 만들지 못했다고 알린다.
    lead.spawner.lock().unwrap_or_else(|e| e.into_inner()).take();
    lead.org.lock().unwrap_or_else(|e| e.into_inner()).take();
    while let Ok(r) = spawn_rx.try_recv() {
        crate::leadrun::spawn_refused(lead_ctx, &r.title);
    }
    let mut outs: Vec<Outcome> = outs.into_iter().map(Option::unwrap_or_default).collect();
    let extra_outs = outs.split_off(parts.len());
    // 총괄이 지운 워커는 끝까지 갔어도 합치지 않는다.
    let removed = lead.removed.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let drop_removed = |key: &str, mut o: Outcome| {
        if removed.contains(key) {
            o.ok = false;
            o.error.get_or_insert_with(|| AGENT_STOPPED.to_string());
        }
        o
    };
    let outs: Vec<Outcome> = outs.into_iter().enumerate().map(|(i, o)| drop_removed(&stage.key(i, 0), o)).collect();
    let extra_outs: Vec<Outcome> = extras.iter().zip(extra_outs).map(|((k, _, _), o)| drop_removed(k, o)).collect();
    for (i, o) in outs.iter().enumerate() {
        let a = &stage.units[i][0];
        let (effort, model) = match (&stage.tiers[i], a.engine) {
            (Some(b), EngineId::Claude) => (Some(b.effort.to_string()), b.claude_model.map(str::to_string)),
            (Some(b), _) => (Some(b.effort.to_string()), spec_model(p, a.engine, s)),
            (None, _) => (spec_effort(p), spec_model(p, a.engine, s)),
        };
        steps.push(step(a.engine, "agent", o, &stage.efforts[i][0].clone().or(effort), a.model.clone().or(model)));
    }
    for ((_, _, a), o) in extras.iter().zip(&extra_outs) {
        steps.push(step(a.engine, "agent", o, &effort, a.model.clone().or_else(|| spec_model(p, a.engine, s))));
    }
    let mut records: Vec<PartRecord> = parts.iter().zip(outs).enumerate().map(|(i, (x, out))| PartRecord { title: x.title.clone(), agent: stage.units[i][0].clone(), out, files: vec![] }).collect();
    records.extend(extras.iter().zip(extra_outs).map(|((_, title, agent), out)| PartRecord { title: title.clone(), agent: agent.clone(), out, files: vec![] }));
    let keys: Vec<String> = (0..parts.len()).map(|i| stage.key(i, 0)).chain(extras.iter().map(|(k, _, _)| k.clone())).collect();
    if tw.cancel.is_cancelled() {
        return (records, vec![]);
    }

    // 마지막 반영: 쉬는 중에 반영하지 못한 변경(팀, 마지막 차례)을 하위 작업마다 차례로 반영한다.
    for (r, k) in records.iter().zip(&keys) {
        if r.ok() && !tw.cancel.is_cancelled() {
            crate::leadrun::checkpoint(lead_ctx, k, &r.out.text).await;
        }
    }
    *lead_ctx.lead.stage.lock().unwrap_or_else(|e| e.into_inner()) = None;
    for (r, k) in records.iter_mut().zip(&keys) {
        r.files = gate.files_of(k);
    }
    let views = gate
        .take_open()
        .into_iter()
        .map(|c| teamwork::ConflictView {
            path: c.path.clone(),
            kept: "메인 폴더(먼저 반영된 판)".to_string(),
            others: vec![(c.who.clone(), teamwork::version_path(&c.copy, &c.path), if c.deleted { Some("(deleted)".to_string()) } else { teamwork::read_version(&c.copy, &c.path) })],
        })
        .collect();
    (records, views)
}

/// 병렬 작업. Some이면 끝난 답(내용, 기록), None이면 나눌 수 없어 한 에이전트로 돌아간다(이유는 p에 남기고 p.team을 지운다).
async fn run_team_work(tw: &TeamWork<'_>, p: &mut Plan, team: &Team, ctx: &(dyn Fn(Option<&str>, &str) -> String + Sync), steps: &mut Vec<Step>) -> Option<(String, Value)> {
    let (app, st, conv, task, mid, s) = (tw.app, tw.st, tw.conv, tw.task, tw.mid, tw.settings);
    let single = |p: &mut Plan, why: String| {
        p.reasons.push(why);
        p.team = None;
        emit(app, "task:plan", json!({ "taskId": task, "conversationId": conv, "team": null, "reasons": p.reasons }));
    };
    let no_copy = |p: &mut Plan, e: String| single(p, format!("프로젝트 폴더를 복사할 수 없어({e}) 단일 에이전트가 파일 수정 작업을 직접 진행합니다."));
    let stopped = |steps: &[Step], p: &Plan, team_json: Value| {
        let why = "중지했습니다.".to_string();
        Some((format!("⚠ {why}"), message_data(steps, p, json!({ "error": why, "team": team_json }))))
    };
    let root = tw.workspace.to_path_buf();
    let ws = root.to_string_lossy().to_string();
    let n = team.agents.len();

    // 1) 지금 폴더의 파일 목록(사본·변경 비교의 기준)
    tw.act("작업 분할을 위해 프로젝트 폴더 검사 중");
    let base = match snapshot_of(&root).await {
        Ok(b) => b,
        Err(e) => {
            no_copy(p, e);
            return None;
        }
    };

    // 2) 나누기: 가장 강한 AI가 대화 모드로 하위 작업 목록(JSON)을 만든다.
    tw.act(&format!("{who}이(가) 요청을 하위 작업으로 분할 중", who = team.synth.label()));
    let mut split = RunSpec {
        engine: team.synth,
        prompt: ctx(None, &teamwork::split_prompt(&p.prompt, &teamwork::listing(&base), n)),
        system: system_prompt(false, &ws, p.skill.as_ref()),
        cwd: root.clone(),
        work: false,
        resume: None,
        plugin_dirs: vec![],
        model: model_for(p, team.synth, s),
        effort: Some("medium".into()),
        // 하위 작업 하나에 150토큰쯤: 에이전트가 많으면 목록이 길어진다.
        max_tokens: Some((n as u32 * 150).max(4000)),
        extra_dirs: tw.extra_dirs.to_vec(),
        steer: None,
        attachments: tw.files.to_vec(),
    };
    if !team.groups.is_empty() {
        split.prompt = format!("{}\n\n{}", split.prompt, teamwork::teams_note(&team.groups, &team.depts));
    }
    let jev_on_groups = if s.jev_enabled { secrets::get(SecretName::TypeSafe) } else { None };
    let (mut o, mut parts) = (Outcome::default(), None);
    for attempt in 0..2 {
        o = run_engine_as(app, st, conv, task, mid, "plan", split.clone(), SPLIT_TIMEOUT, tw.cancel, false, "plan").await;
        steps.push(step(team.synth, "plan", &o, &Some("medium".into()), spec_model(p, team.synth, s)));
        if tw.cancel.is_cancelled() {
            return stopped(steps, p, Value::Null);
        }
        parts = o.ok.then(|| teamwork::parse_parts(&o.text, n)).flatten();
        // 적힌 팀은 JEV에게 묻지 않고 수만 맞춘다. 다르면 한 번 더 나누게 하고, 그래도 다르면 아래에서 맞춘다.
        if !team.groups.is_empty() {
            if attempt == 0 && parts.as_ref().is_some_and(|f: &Vec<teamwork::Part>| f.len() != team.groups.len()) {
                continue;
            }
            break;
        }
        let (Some(found), Some(key), 0) = (&parts, &jev_on_groups, attempt) else { break };
        match groups_kept(&st.http, key, &p.prompt, found).await {
            Ok((kept, ms, cost)) => {
                let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "groups", ok: true, latency_ms: ms, input_tokens: None, output_tokens: None, cost_usd: Some(cost) });
                if kept >= 0.4 {
                    break;
                }
                split.prompt = format!("{}\n\n앞서 나눈 결과가 요청문에 적힌 팀과 다릅니다. 적힌 팀 그대로 다시 나누세요.", split.prompt);
            }
            Err(_) => break,
        }
    }
    let Some(parts) = parts else {
        single(p, "독립된 하위 작업으로 분할할 수 없어 단일 에이전트가 전체 파일 수정을 진행합니다.".to_string());
        return None;
    };
    let parts = if team.groups.is_empty() { parts } else { teamwork::fit_to_org(parts, &team.groups, &team.depts) };
    if !team.groups.is_empty() && parts.len() < team.groups.len() {
        p.reasons.push(format!("작업이 {got}개로 분할되어 작성된 {want}개 팀 중 {got}개 팀만 실행됩니다.", got = parts.len(), want = team.groups.len()));
    }

    // 3) 로드맵과 작업 공간을 함께 준비한다: OpenCode가 하위 작업마다 맡을 AI와 노력 단계를 정하는 동안 에이전트마다 폴더 사본을 만든다.
    // 못 정한 칸은 처음 계획을 따르고, 요청 글에서 정한 팀이나 모델은 그대로 둔다.
    let pool = worker_pool(st.installed());
    let mut assigned: Vec<TeamAgent> = team.agents.iter().take(parts.len()).cloned().collect();
    let want_roadmap = tw.roadmap && !team.pinned && assigned.iter().all(|a| a.model.is_none());
    // 작업 공간은 프로젝트의 .elysium/worktrees 아래에 둔다(저장소에서는 변경으로 보이지 않게 제외한다).
    let home = teamwork::workspace_home(&root);
    let r2 = root.clone();
    let _ = tokio::task::spawn_blocking(move || crate::gitops::exclude_workspace_dir(&r2)).await;
    let tmp = home.join(task);
    let (picks, prepared) = {
        let pr: &Plan = p;
        tokio::join!(
            async {
                if want_roadmap {
                    plan_roadmap(tw, pr, "roadmap", &parts, &pool, steps).await
                } else {
                    None
                }
            },
            prepare_stage(&root, &tmp, parts.len(), &base),
        )
    };
    let (dirs, space_note) = match prepared {
        Ok(x) => x,
        Err(e) => {
            remove_agent_dirs(root.clone(), tmp);
            no_copy(p, e);
            return None;
        }
    };
    if tw.cancel.is_cancelled() {
        remove_agent_dirs(root, tmp);
        return stopped(steps, p, Value::Null);
    }
    let mut tiers: Vec<Option<Budget>> = vec![None; parts.len()];
    if let Some(picks) = picks {
        for (slot, pick) in assigned.iter_mut().zip(&picks) {
            if let Some(e) = pick.engine {
                slot.engine = e;
            }
        }
        // 스킬이 노력 단계를 정했거나 요청 글에서 모델을 고른 경우 그 뜻을 따른다.
        if p.skill.is_none() && p.models.is_empty() {
            tiers = picks.iter().map(|x| x.tier.and_then(budget_for)).collect();
        }
        let list = parts.iter().zip(&assigned).zip(&tiers).map(|((x, a), t)| roadmap_line(&x.title, a.engine, t.as_ref())).collect::<Vec<_>>().join(", ");
        p.reasons.push(format!("OpenCode 작업 할당: {list}", list = list));
        let mut t = team.clone();
        t.agents = assigned.clone();
        p.team = Some(t.clone());
        emit(app, "task:plan", json!({ "taskId": task, "conversationId": conv, "team": t, "reasons": p.reasons }));
    }

    // 4) 총괄: 요청 검토·사용자 소통을 맡는다. 첫 팀원은 배정된 AI, 나머지는 팀 목록에서 차례로 쓴다.
    let shared: std::sync::Arc<AppState> = app.state::<std::sync::Arc<AppState>>().inner().clone();
    let lead = std::sync::Arc::new(crate::leadrun::TaskLead::new(conv, task, mid, &p.prompt, team.synth, model_for(p, team.synth, s), root.clone(), jev_on_groups.clone(), tw.cancel.child_token(), o.session_id.clone()));
    let mut spare = team.agents.iter().cycle().skip(parts.len());
    let units: Vec<Vec<TeamAgent>> = if team.groups.is_empty() {
        parts.iter().zip(&assigned).map(|(x, first)| std::iter::once(first.clone()).chain(spare.by_ref().take(x.members.saturating_sub(1)).cloned()).collect()).collect()
    } else {
        written_units(&team.agents, &team.groups, parts.len())
    };
    // 적힌 자리별 노력치: 팀마다 앞에서부터 자리를 센다.
    let seat_from = |i: usize| team.groups.iter().take(i).sum::<usize>();
    let efforts: Vec<Vec<Option<String>>> = units.iter().enumerate().map(|(i, u)| (0..u.len()).map(|m| team.member_efforts.get(seat_from(i) + m).cloned().flatten().or_else(|| team.agent_effort.clone())).collect()).collect();
    let first = Stage { parts, units, tiers, efforts, prefix: "agent:".to_string() };
    lead.workers.lock().unwrap_or_else(|e| e.into_inner()).extend(first.parts.iter().enumerate().flat_map(|(i, x)| {
        let first = &first;
        (0..first.units[i].len()).map(move |m| crate::leadrun::WorkerInfo { key: first.key(i, m), title: x.title.clone(), group: (first.units[i].len() > 1).then_some(i) })
    }));
    *lead.work_system.lock().unwrap_or_else(|e| e.into_inner()) = tw.system.to_string();
    if s.lead_commit {
        let r2 = root.clone();
        let repo = tokio::task::spawn_blocking(move || crate::gitops::open(&r2)).await.ok().flatten();
        if repo.is_none() {
            p.reasons.push("git 저장소가 아니거나 커밋할 수 없는 상태라 총괄 커밋 검토 없이 진행합니다.".to_string());
        }
        *lead.repo.lock().unwrap_or_else(|e| e.into_inner()) = repo;
    }
    if let Some(n) = tw.max_turns {
        lead.max_turns.store(n, std::sync::atomic::Ordering::Relaxed);
    }
    st.leads.lock().unwrap_or_else(|e| e.into_inner()).insert(task.to_string(), lead.clone());
    let _lead_entry = LeadEntry { st: shared.clone(), lead: lead.clone() };
    let lead_ctx = crate::leadrun::Ctx { app: app.clone(), st: shared, lead: lead.clone() };
    emit(app, "lead:started", json!({ "taskId": task, "conversationId": conv, "engine": team.synth, "label": team.synth.label(), "model": lead.model }));
    let groups: Vec<Value> = first.units.iter().enumerate().filter(|(_, u)| u.len() > 1).map(|(i, u)| json!({ "index": i, "title": first.parts[i].title, "keys": (0..u.len()).map(|m| first.key(i, m)).collect::<Vec<_>>() })).collect();
    if !groups.is_empty() {
        emit(app, "task:groups", json!({ "taskId": task, "conversationId": conv, "groups": groups }));
    }
    let mut part_views = first.views();
    emit(app, "task:parts", json!({ "taskId": task, "conversationId": conv, "parts": part_views }));

    // 5) 하위 작업을 동시에 하고 바뀐 파일을 합친다.
    let (mut records, mut views) = run_stage(tw, p, &first, &dirs, &tmp, &space_note, &base, &lead_ctx, ctx, steps).await;
    let mut conflicts: Vec<String> = views.iter().map(|v| v.path.clone()).collect();
    if tw.cancel.is_cancelled() {
        remove_agent_dirs(root, tmp);
        push_lead(steps, &lead);
        return stopped(steps, p, team_record(&records, None, &conflicts, &lead));
    }
    // Every worker failed: the integrator does the whole job instead of the task ending here (integrate_prompt marks each failed part).
    if records.iter().all(|r| !r.ok()) {
        crate::leadrun::note(&lead_ctx, "warn", ALL_FAILED_TAKEOVER);
    }

    // 6) 통합: 원래 폴더에서 맞물림·충돌·실패한 부분을 마무리한다. 남은 일이 크면 통합자가 로드맵에 다음 단계를 덧붙이고,
    // 그 하위 작업을 다시 동시에 돌려 합친 뒤 통합자에게 돌려준다(최대 roadmap::MAX_EXTRA_STAGES번).
    let (effort, max_tokens) = (spec_effort(p), p.budget.as_ref().map(|b| b.max_tokens));
    let (mut tmp, mut from, mut added, mut resume) = (tmp, 0usize, 0usize, None::<String>);
    let commits = lead.repo.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    // A failed integration run is tried once more before falling back to the workers' summaries.
    let mut integrate_retried = false;
    let mut retrying = false;
    // Window test results the integrator has not seen yet, and whether it already had its one round to fix its own failed tests.
    let mut tested: Vec<String> = vec![];
    let mut tests_fixed = false;
    let mut j = loop {
        tested.extend(crate::leadrun::settle_tests(&lead_ctx).await);
        let results: Vec<teamwork::PartResult> = records[from..].iter().map(|r| teamwork::PartResult { title: &r.title, who: seat_label(&r.agent), ok: r.ok(), summary: &r.out.text, files: r.files.clone(), left: r.out.ok.then(|| crate::lead::left_over(&r.out.text)).flatten() }).collect();
        let mut prompt = teamwork::integrate_prompt(&p.prompt, &results, &views);
        if added > 0 {
            prompt = format!("[로드맵 {stage}단계 결과] 당신이 덧붙인 단계를 워커들이 끝냈고 바뀐 파일은 이미 합쳤습니다.\n\n{prompt}", stage = added + 1);
        }
        if !tested.is_empty() {
            prompt = format!("{TESTS_FOR_INTEGRATOR}\n{}\n\n{prompt}", tested.join("\n\n"));
        }
        if std::mem::take(&mut retrying) {
            prompt = format!("{INTEGRATE_RETRY}\n\n{prompt}");
        }
        let left = roadmap::MAX_EXTRA_STAGES - added;
        let mut extra = tw.extra_dirs.to_vec();
        extra.push(tmp.to_string_lossy().to_string());
        let spec = RunSpec {
            engine: team.synth,
            prompt: ctx(None, &prompt),
            system: format!("{}{}{}{}{}", tw.system, teamwork::INTEGRATE_SYSTEM, crate::leadrun::worker_note(team.synth), roadmap::next_system(left, n, &pool), if commits { crate::lead::COMMIT_SYSTEM } else { "" }),
            cwd: root.clone(),
            work: true,
            resume: resume.take(),
            plugin_dirs: if team.synth == EngineId::Claude { tw.plugin_dirs.to_vec() } else { vec![] },
            model: model_for(p, team.synth, s),
            effort: effort.clone(),
            max_tokens,
            extra_dirs: extra,
            steer: None,
            attachments: tw.files.to_vec(),
        };
        lead.integrating.store(true, std::sync::atomic::Ordering::Relaxed);
        let gate = crate::leadrun::integration_begin(&lead_ctx).await;
        let mut j = if team.synth.resumable() {
            let key = format!("synth:{}", team.synth.as_str());
            crate::leadrun::run_worker(&lead_ctx, spec, crate::leadrun::WorkerOpts { key: &key, title: "통합", role: "synth", stream: true, budget: tw.timeout, reviewed: crate::leadrun::needs_review(team.synth), check_drift: false, crew: None }).await
        } else {
            run_engine(app, st, conv, task, mid, "synth", spec, tw.timeout, tw.cancel).await
        };
        steps.push(step(team.synth, "synth", &j, &spec_effort(p), spec_model(p, team.synth, s)));
        if let Some((guard, repo, before)) = gate {
            j.text = crate::leadrun::integration_end(&lead_ctx, &repo, &before, &j.text, j.ok).await;
            drop(guard);
        }
        // An integrator that cannot resume gets its window tests run now; a failure gives it one more round to fix it.
        if j.ok && !team.synth.resumable() {
            let parsed = crate::lead::parse_requests(&j.text);
            if !parsed.requests.is_empty() {
                j.text = parsed.clean;
                let key = format!("synth:{}", team.synth.as_str());
                let results = crate::leadrun::run_asked_tests(&lead_ctx, &key, &root, parsed.requests, tw.cancel).await;
                if !tests_fixed && !tw.cancel.is_cancelled() && results.iter().any(|r| crate::leadrun::test_failed(r)) {
                    tests_fixed = true;
                    tested = results;
                    continue;
                }
                tests_tail(&mut j.text, &results);
            }
        }
        // Keep this stage's copies for the retry: the integrator may still need the conflicting versions in them.
        if !j.ok && !integrate_retried && !tw.cancel.is_cancelled() {
            integrate_retried = true;
            retrying = true;
            resume = j.session_id.clone().filter(|_| team.synth.resumable());
            crate::leadrun::note(&lead_ctx, "warn", format!("{INTEGRATE_FAILED_NOTE} ({})", cut(j.error.as_deref().unwrap_or("-"), 200)));
            tokio::select! {
                _ = tokio::time::sleep(INTEGRATE_RETRY_WAIT) => {}
                _ = tw.cancel.cancelled() => {}
            }
            if !tw.cancel.is_cancelled() {
                continue;
            }
        }
        tested.clear();
        // 이 단계의 사본은 통합자가 충돌 판을 읽은 뒤에는 쓰지 않는다.
        remove_agent_dirs(root.clone(), tmp.clone());
        let (next, clean) = roadmap::parse_next(&j.text, n, &pool);
        j.text = clean;
        // 블록만 쓰고 본문이 없어도 다음 단계로 간다(최종 요약은 마지막 통합이 쓴다).
        if next.is_empty() || left == 0 || tw.cancel.is_cancelled() || !j.ok {
            break j;
        }

        // 7) 다음 단계: 통합자가 AI를 고른 칸은 그대로(요청 글에서 정한 팀은 팀 그대로), 나머지는 OpenCode 로드맵이, 그것도 없으면 팀에서 차례로 맡는다.
        lead.integrating.store(false, std::sync::atomic::Ordering::Relaxed);
        added += 1;
        let stage_no = added + 1;
        let chosen: Vec<Option<EngineId>> = next.iter().map(|x| x.engine.filter(|_| !team.pinned)).collect();
        let parts: Vec<teamwork::Part> = next.into_iter().map(|x| x.part).collect();
        let next_tmp = home.join(format!("{task}-s{stage_no}"));
        let want_roadmap = tw.roadmap && !team.pinned && chosen.iter().any(Option::is_none);
        let roadmap_key = format!("roadmap:{stage_no}");
        let (picks, prepared) = {
            let pr: &Plan = p;
            tokio::join!(
                async {
                    if want_roadmap {
                        plan_roadmap(tw, pr, &roadmap_key, &parts, &pool, steps).await
                    } else {
                        None
                    }
                },
                async {
                    let base = snapshot_of(&root).await?;
                    let (dirs, note) = prepare_stage(&root, &next_tmp, parts.len(), &base).await?;
                    Ok::<_, String>((base, dirs, note))
                },
            )
        };
        let (base, dirs, space_note) = match prepared {
            Ok(x) => x,
            Err(e) => {
                remove_agent_dirs(root.clone(), next_tmp);
                p.reasons.push(format!("작업 폴더를 준비하지 못해 추가된 단계를 건너뛰었습니다 ({e}).", e = e));
                break j;
            }
        };
        tmp = next_tmp;
        if tw.cancel.is_cancelled() {
            remove_agent_dirs(root, tmp);
            push_lead(steps, &lead);
        return stopped(steps, p, team_record(&records, None, &conflicts, &lead));
        }
        let len = team.agents.len();
        let mut units: Vec<Vec<TeamAgent>> = vec![];
        let mut tiers: Vec<Option<Budget>> = vec![];
        for (i, lead_pick) in chosen.iter().enumerate() {
            let pick = picks.as_ref().and_then(|r| r.get(i));
            let mut seat = team.agents[i % len].clone();
            if let Some(e) = lead_pick.or_else(|| pick.and_then(|x| x.engine)).filter(|e| *e != seat.engine) {
                seat = TeamAgent { engine: e, model: None };
            }
            units.push(vec![seat]);
            tiers.push(pick.and_then(|x| x.tier).and_then(budget_for).filter(|_| p.skill.is_none() && p.models.is_empty()));
        }
        let efforts = (0..parts.len()).map(|i| vec![team.member_efforts.get(i % len).cloned().flatten().or_else(|| team.agent_effort.clone())]).collect();
        let stage = Stage { parts, units, tiers, efforts, prefix: format!("agent:s{stage_no}-") };
        let list = stage.parts.iter().zip(&stage.units).zip(&stage.tiers).map(|((x, u), t)| roadmap_line(&x.title, u[0].engine, t.as_ref())).collect::<Vec<_>>().join(", ");
        p.reasons.push(format!("통합 에이전트가 로드맵 {stage}단계를 추가했습니다: {list}", stage = stage_no, list = list));
        emit(app, "task:plan", json!({ "taskId": task, "conversationId": conv, "team": p.team, "reasons": p.reasons }));
        lead.workers.lock().unwrap_or_else(|e| e.into_inner()).extend(stage.parts.iter().enumerate().map(|(i, x)| crate::leadrun::WorkerInfo { key: stage.key(i, 0), title: x.title.clone(), group: None }));
        part_views.extend(stage.views());
        emit(app, "task:parts", json!({ "taskId": task, "conversationId": conv, "parts": part_views }));
        let (more, more_views) = run_stage(tw, p, &stage, &dirs, &tmp, &space_note, &base, &lead_ctx, ctx, steps).await;
        from = records.len();
        records.extend(more);
        conflicts.extend(more_views.iter().map(|v| v.path.clone()));
        views = more_views;
        if tw.cancel.is_cancelled() {
            remove_agent_dirs(root, tmp);
            push_lead(steps, &lead);
        return stopped(steps, p, team_record(&records, None, &conflicts, &lead));
        }
        // 세션을 이어 쓰는 통합자는 앞선 통합의 맥락을 그대로 이어 간다.
        resume = j.session_id.clone().filter(|_| team.synth.resumable());
    };
    // Tests the lead asked for during the last integration (for example while fixing a rejected commit) still get run and shown.
    let late = crate::leadrun::settle_tests(&lead_ctx).await;
    if j.ok {
        tests_tail(&mut j.text, &late);
    }
    let first_ok = records.iter().find(|r| r.ok()).map(|r| r.agent.engine);
    if j.ok && !j.text.trim().is_empty() {
        push_lead(steps, &lead);
        return Some((j.text.clone(), message_data(steps, p, json!({ "synthBy": team.synth, "draftedBy": first_ok, "team": team_record(&records, Some(team.synth), &conflicts, &lead) }))));
    }
    // 통합에 실패하면 합친 결과는 폴더에 남기고 각 에이전트의 요약을 이어 붙여 보여 준다.
    let joined: String = records.iter().filter(|r| r.ok()).map(|r| format!("### {} · {}\n{}\n", r.title, seat_label(&r.agent), r.out.text.trim())).collect::<Vec<_>>().join("\n");
    if joined.trim().is_empty() {
        let why = j.error.clone().or_else(|| records.iter().find_map(|r| r.out.error.clone())).unwrap_or_else(|| "답을 받지 못했습니다".into());
        push_lead(steps, &lead);
        return Some((format!("⚠ {why}"), message_data(steps, p, json!({ "error": why, "team": team_record(&records, None, &conflicts, &lead) }))));
    }
    let mut p2 = p.clone();
    p2.reasons.push(format!("통합 에이전트 {who}의 작업이 실패하여 병합된 파일은 유지되고 각 에이전트의 요약이 대신 표시됩니다.", who = team.synth.label()));
    push_lead(steps, &lead);
    Some((joined, message_data(steps, &p2, json!({ "draftedBy": first_ok, "team": team_record(&records, None, &conflicts, &lead) }))))
}

/// 자기 세션을 이어 쓰는 엔진(Claude Code·Antigravity)이 지금까지를 봤다고 적는다.
fn mark_seen(st: &AppState, conv: &str, engine: EngineId) {
    if engine.resumable() {
        let _ = store::set_seen(&st.db, conv, engine.as_str(), &crate::db::now());
    }
}

/// 요약은 쉬운 일이라 엔진마다 가장 싼 모델로 시킨다(None이면 설정·기본 모델). 공식 단가표 기준 확인값(2026-10-31).
fn summary_model(e: EngineId) -> Option<&'static str> {
    match e {
        EngineId::Claude => Some("haiku"),
        EngineId::ClaudeApi => Some("claude-haiku-5-5"),
        EngineId::OpenAi => Some("gpt-5-nano"),
        EngineId::GeminiApi => Some("gemini-2.5-flash-lite"),
        _ => None,
    }
}

/// 요약을 맡길 순서: 단가표로 계산한 싼 순서(같으면 API 먼저 — CLI는 띄우는 데 시간이 더 든다).
pub fn summarizers(installed: &[EngineId]) -> Vec<(EngineId, Option<&'static str>)> {
    let mut v: Vec<(EngineId, Option<&'static str>)> = engines::ALL.into_iter().filter(|e| installed.contains(e)).map(|e| (e, summary_model(e))).collect();
    v.sort_by(|a, b| crate::pricing::blended(a.0, a.1).total_cmp(&crate::pricing::blended(b.0, b.1)).then(b.0.is_api().cmp(&a.0.is_api())));
    v
}

/// 세션 요약 갱신: 오래된 대화가 충분히 쌓이면 가장 싼 AI가 요약에 접어 넣는다(화면에는 흘리지 않음).
pub async fn refresh_memory(app: &AppHandle, st: &AppState, conv: &str) {
    static BUSY: Mutex<Vec<String>> = Mutex::new(vec![]);
    let msgs = store::list_messages(&st.db, conv).unwrap_or_default();
    let history = memory::turns(&msgs);
    let (summary, upto) = store::get_memory(&st.db, conv).unwrap_or((None, None));
    let Some(fold) = memory::to_fold(&history, upto.as_deref()) else { return };
    {
        let mut b = BUSY.lock().unwrap_or_else(|p| p.into_inner());
        if b.iter().any(|c| c == conv) {
            return;
        }
        b.push(conv.to_string());
    }
    let new_upto = fold.last().map(|t| t.at.clone());
    let installed = st.installed();
    let s = st.settings();
    for (engine, model) in summarizers(&installed) {
        let spec = RunSpec {
            engine,
            prompt: memory::summary_prompt(summary.as_deref(), &fold),
            system: "당신은 작업 세션의 기억을 정리하는 요약자입니다. 요청한 형식의 요약만 출력하세요.".into(),
            cwd: s.workspace(),
            work: false,
            resume: None,
            plugin_dirs: vec![],
            model: model.map(str::to_string).or_else(|| s.models.get(engine.as_str()).cloned()),
            effort: Some("low".into()),
            max_tokens: Some(1200),
            extra_dirs: vec![],
            steer: None,
            attachments: vec![],
        };
        let o = engines::run(&st.http, &spec, Duration::from_secs(120), &CancellationToken::new(), |_| {}).await;
        let _ = store::record_usage(&st.db, &store::Usage { engine: engine.as_str(), role: "memory", ok: o.ok, latency_ms: o.latency_ms, input_tokens: o.input_tokens, output_tokens: o.output_tokens, cost_usd: o.cost_usd });
        if o.ok && !o.text.trim().is_empty() {
            let _ = store::set_memory(&st.db, conv, Some(&memory::clean_summary(&o.text)), new_upto.as_deref());
            emit(app, "memory:updated", json!({ "conversationId": conv, "by": engine }));
            break;
        }
    }
    BUSY.lock().unwrap_or_else(|p| p.into_inner()).retain(|c| c != conv);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn catalog() -> (tempfile::TempDir, Catalog) {
        let tmp = tempfile::tempdir().unwrap();
        skills::seed(tmp.path()).unwrap();
        let w = tmp.path().join("skills/fix");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(w.join("SKILL.md"), "---\nname: fix\ndescription: 고치기\nmode: work\nengine: codex\n---\n고쳐라").unwrap();
        let c = skills::load(tmp.path(), &HashMap::new());
        (tmp, c)
    }

    #[test]
    fn picks_engines_by_request_and_installed_set() {
        let (_t, c) = catalog();
        let all = [EngineId::Claude, EngineId::Codex, EngineId::Antigravity];
        let p = plan("오늘 날씨 어때", None, false, &all, &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.primary, EngineId::Claude);
        assert_eq!(p.fallbacks, vec![EngineId::Antigravity, EngineId::Codex]);
        assert!(p.reviewer.is_none(), "짧은 요청은 검토 안 함");
        // 코드 요청 + Claude 없음 → Codex
        let p = plan("이 함수 버그 찾아줘", None, false, &[EngineId::Codex, EngineId::Antigravity], &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.primary, EngineId::Codex);
        assert!(plan("Python으로 리스트 뒤집기", None, false, &[EngineId::Codex, EngineId::Antigravity], &c, ReviewMode::Auto, None).unwrap().primary == EngineId::Codex, "대소문자 무시");
        // 직접 고르면 그대로, 없으면 오류
        assert_eq!(plan("hi", Some(EngineId::Antigravity), false, &all, &c, ReviewMode::Auto, None).unwrap().primary, EngineId::Antigravity);
        assert!(plan("hi", Some(EngineId::Antigravity), false, &[EngineId::Claude], &c, ReviewMode::Auto, None).is_err());
        assert!(plan("hi", None, false, &[], &c, ReviewMode::Auto, None).is_err());
    }

    #[test]
    fn review_rules() {
        let (_t, c) = catalog();
        let deep = "새 서비스 출시 전략을 자세히 세워줘";
        let p = plan(deep, None, false, &[EngineId::Claude, EngineId::Codex], &c, ReviewMode::Auto, None).unwrap();
        assert_eq!((p.primary, p.reviewer), (EngineId::Claude, Some(EngineId::Codex)), "다른 AI가 검토");
        let p = plan(deep, None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.reviewer, Some(EngineId::Claude), "다른 CLI가 없으면 같은 CLI가 새 세션으로");
        assert!(plan(deep, None, false, &[EngineId::Claude], &c, ReviewMode::Off, None).unwrap().reviewer.is_none());
        assert!(plan("안녕", None, false, &[EngineId::Claude], &c, ReviewMode::Always, None).unwrap().reviewer.is_some());
        assert!(plan(deep, None, true, &[EngineId::Claude], &c, ReviewMode::Always, None).unwrap().reviewer.is_none(), "작업 모드는 검토 안 함");
        // 스킬 review: true(report) / false(code-review)
        assert!(plan("/report 전기차 시장", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap().reviewer.is_some());
        assert!(plan("/code-review 자세히 해줘", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap().reviewer.is_none());
    }

    #[test]
    fn jev_picks_only_enabled_skills_and_words_alone_do_not() {
        let (t, c) = catalog();
        // 낱말이 있어도 JEV 판단 없이는 적용하지 않는다
        assert!(plan("이 코드 리뷰 부탁해", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap().skill.is_none());
        let j = |id: &str, fit: f64| Judgment { skill: Some(id.into()), skill_fit: Some(fit), ..Default::default() };
        let p = plan("로그인이 튕겨", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, Some(&j("root-cause", 0.9))).unwrap();
        assert_eq!(p.skill.as_ref().map(|s| (s.0.id.as_str(), s.0.how)), Some(("root-cause", "auto")));
        assert!(p.reasons.iter().any(|r| r.contains("JEV") && r.contains("root-cause") && r.contains("90%")));
        assert!(plan("안녕", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, Some(&j("root-cause", 0.3))).unwrap().skill.is_none(), "적합도가 낮으면 적용 안 함");
        assert!(plan("안녕", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, Some(&j("없는스킬", 0.9))).unwrap().skill.is_none(), "목록에 없는 스킬은 무시");
        // 꺼진 스킬은 고를 수 없다
        let mut off = HashMap::new();
        off.insert("skill:root-cause".to_string(), false);
        let c2 = skills::load(t.path(), &off);
        assert!(plan("로그인이 튕겨", None, false, &[EngineId::Claude], &c2, ReviewMode::Auto, Some(&j("root-cause", 0.9))).unwrap().skill.is_none());
        // /이름은 JEV 판단보다 먼저
        let p = plan("/summarize 이 글", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, Some(&j("root-cause", 0.9))).unwrap();
        assert_eq!(p.skill.unwrap().0.how, "explicit");
    }

    #[test]
    fn jev_picks_engine_within_rules() {
        use EngineId::*;
        let (_t, c) = catalog();
        let j = |e: EngineId, conf: f64| Judgment { engine: Some(e.as_str().into()), engine_confidence: Some(conf), ..Default::default() };
        let all = [Claude, Codex, GeminiApi];
        let p = plan("이거 해줘", None, false, &all, &c, ReviewMode::Off, Some(&j(GeminiApi, 0.9))).unwrap();
        assert_eq!(p.primary, GeminiApi);
        assert!(p.fallbacks.contains(&Claude) && !p.fallbacks.contains(&GeminiApi));
        assert_eq!(plan("이거 해줘", None, false, &all, &c, ReviewMode::Off, Some(&j(GeminiApi, 0.2))).unwrap().primary, Claude, "확신이 낮으면 기본 순서");
        assert_eq!(plan("이거 해줘", None, false, &[Claude, Codex], &c, ReviewMode::Off, Some(&j(GeminiApi, 0.9))).unwrap().primary, Claude, "쓸 수 없는 AI는 무시");
        assert_eq!(plan("파일 고쳐", None, true, &all, &c, ReviewMode::Off, Some(&j(GeminiApi, 0.9))).unwrap().primary, Claude, "작업은 도구가 있는 CLI");
        assert_eq!(plan("파일 고쳐", None, true, &all, &c, ReviewMode::Off, Some(&j(Codex, 0.9))).unwrap().primary, Codex);
        assert_eq!(plan("이거 해줘", Some(Claude), false, &all, &c, ReviewMode::Off, Some(&j(Codex, 0.9))).unwrap().primary, Claude, "직접 고른 AI가 먼저");
        let needs_files = Judgment { local: Some(0.9), ..j(GeminiApi, 0.9) };
        assert_eq!(plan("내 폴더 봐줘", None, false, &all, &c, ReviewMode::Off, Some(&needs_files)).unwrap().primary, Claude, "파일이 필요하면 API를 고르지 않는다");
        let hard = Judgment { difficulty: Some(2.8), difficulty_confidence: Some(0.9), ..j(Nvidia, 0.9) };
        assert_eq!(plan("어려운 설계", None, false, &[Claude, Nvidia], &c, ReviewMode::Off, Some(&hard)).unwrap().primary, Claude, "어려운 요청에 약한 AI는 고르지 않는다");
    }

    #[test]
    fn jev_decides_parallel_only_for_clearly_split_work() {
        use EngineId::*;
        let (_t, c) = catalog();
        let all = [Claude, Codex];
        let j = |n: u8, single: f64| Judgment { parts: Some(n), parts_single: Some(single), ..Default::default() };
        let auto = |text: &str, work: bool, jj: Option<&Judgment>, choice: Option<EngineId>| plan_with(text, choice, work, &all, &c, ReviewMode::Auto, jj, 0).unwrap().team;
        assert_eq!(auto("세 모듈 추가", true, Some(&j(3, 0.04)), None).map(|t| t.agents.len()), Some(3));
        assert!(auto("세 모듈 추가", true, Some(&j(3, 0.5)), None).is_none(), "나누지 않는 편일 확률이 높으면 하나로");
        assert!(auto("이름 바꿔", true, Some(&j(1, 0.99)), None).is_none());
        assert!(auto("세 모듈 추가", false, Some(&j(3, 0.04)), None).is_none(), "대화 답은 나누지 않는다");
        assert!(auto("세 모듈 추가", true, None, None).is_none(), "JEV가 없으면 하나로");
        assert!(auto("세 모듈 추가", true, Some(&j(3, 0.04)), Some(Claude)).is_none(), "AI를 직접 고르면 하나로");
        assert_eq!(auto("에이전트 2개로 고쳐", true, Some(&j(4, 0.01)), None).map(|t| t.agents.len()), Some(2), "요청 글의 수가 먼저");
        // 수동 설정은 JEV 판단을 쓰지 않는다
        assert!(plan_with("세 모듈 추가", None, true, &all, &c, ReviewMode::Auto, Some(&j(3, 0.04)), 1).unwrap().team.is_none());
    }

    /// 실제 호출로 임계값을 확인한다: TYPESAFE_API_KEY=... cargo test --lib live_jev -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_jev_thresholds() {
        let key = std::env::var("TYPESAFE_API_KEY").unwrap();
        let http = reqwest::Client::new();
        let d = |a: &'static str, b: &'static str| (a, b);
        for (req, ans) in [
            d("1부터 10까지 더하면?", "1부터 10까지의 합은 55입니다."),
            d("1부터 10까지 더하면?", "100입니다."),
            d("파이썬으로 CSV 읽어서 합계 내는 코드 짜줘", "죄송합니다. 잘 모르겠습니다."),
            d("대한민국의 수도와 인구, 면적, 주요 산업을 표로 정리해 줘", "서울입니다."),
            d("리액트 useEffect 정리 함수가 언제 실행되는지 설명해 줘", "useEffect의 정리 함수는 컴포넌트가 사라지기 직전과, 의존성이 바뀌어 효과가 다시 실행되기 직전에 실행됩니다. 구독 해제나 타이머 제거에 씁니다."),
        ] {
            println!("draft_ready {:.3}  {req} => {ans}", draft_ready(&http, &key, req, ans).await.unwrap().0);
        }
        for (rep, label) in [
            ("로그인 페이지를 만들고 테스트까지 통과했습니다. 바꾼 파일: login.tsx, login.test.tsx", "done"),
            ("로그인 폼은 만들었습니다. 다음으로 API 연결과 테스트를 진행하겠습니다.", "unfinished"),
            ("빌드가 오류로 끝났고 원인을 찾지 못했습니다. TODO: 타입 오류 수정.", "unfinished"),
            ("어느 데이터베이스를 쓸지 정해 주세요. Postgres와 SQLite 중 어느 쪽인가요?", "needs_user"),
        ] {
            println!("unfinished {:.3} [{label}] {rep}", work_unfinished(&http, &key, "로그인 페이지를 만들고 API에 연결해 줘", rep).await.unwrap().0);
        }
        for e in ["요청 한도를 넘었습니다(429)", "공급자 서버가 바쁩니다(과부하) (503)", "스트림이 끊겼습니다: connection reset", "키가 맞지 않거나 권한이 없습니다 (401)", "Model unavailable: opencode/x", "Individual quota reached.", "Not logged in — 터미널에서 claude 로 로그인하세요", "120초 안에 끝나지 않아 중지했습니다"] {
            println!("transient {:.3}  {e}", failure_transient(&http, &key, "OpenCode", e).await.unwrap().0);
        }
        let draft = "서울의 인구는 약 940만 명이고, 면적은 605㎢입니다.";
        for (rev, label) in [("서울의 인구는 약 940만 명(2024년 기준)이고, 면적은 605.2㎢입니다.", "better"), ("서울에 대한 정보입니다.", "worse"), ("서울의 인구는 약 3억 명이고 면적은 605㎢입니다.", "worse")] {
            println!("review_worse {:.3} [{label}] {rev}", review_worse(&http, &key, "서울 인구와 면적 알려 줘", draft, rev).await.unwrap().0);
        }
        for t in ["파이썬 리스트 뒤집는 법 알려 줘", "내 비밀번호 hunter2 와 카드번호 4111-1111-1111-1111 로 결제 스크립트 만들어 줘", "우리 회사 미공개 3분기 실적 자료 요약해 줘: 매출 120억"] {
            let j = route(&http, &key, t, &RouteAsk { core: true, work: true, free_models: vec!["opencode/deepseek-v4.1-flash-free".into(), "opencode/beam-free".into(), "opencode/big-pickle".into()], ..Default::default() }).await.unwrap();
            println!("sensitive {:.3} risk {:.2} free {:?} {:.2}  {t}", j.sensitive.unwrap_or(-1.0), j.risk.unwrap_or(-1.0), j.free_model, j.free_model_confidence.unwrap_or(0.0));
        }
        for t in ["파이썬 데코레이터가 뭐야?", "src/login.ts 의 오타 고쳐 줘", "할 일 관리 웹앱 만들어 줘", "마이크로서비스로 갈지 모놀리스로 갈지 결정해야 해. 장단점 토론해 줘", "이 버그 왜 나는지 알려 줘"] {
            let r = suggest(&http, &key, t, &[], false).await.unwrap();
            println!("suggest {:?} {:.2} {}ms  {t}", r.mode, r.mode_confidence.unwrap_or(0.0), r.ms);
        }
        let dir = tempfile::tempdir().unwrap();
        let a = attachments::add_bytes(dir.path(), "매출.csv", "월,매출
1월,100".as_bytes(), "user").unwrap();
        let b = attachments::add_bytes(dir.path(), "여행 계획.txt", "제주도 3박 4일".as_bytes(), "user").unwrap();
        let files = [a, b];
        for t in ["아까 올린 표에서 1월 매출이 얼마야?", "제주도 일정 어떻게 짰더라", "오늘 날씨 어때?"] {
            println!("earlier {:?}  {t}", pick_earlier_files(&http, &key, dir.path(), t, &files).await.unwrap().0);
        }
    }

    #[test]
    fn free_model_respects_images_privacy_and_settings() {
        let all: Vec<String> = ["opencode/beam-free", "opencode/big-pickle", "opencode/deepseek-v4.1-flash-free", "opencode/glm-5.3-flash-free", "opencode/mimo-v2.6-flash-free"].iter().map(|m| m.to_string()).collect();
        let j = |m: &str, conf: f64, sensitive: f64| Judgment { free_model: Some(m.into()), free_model_confidence: Some(conf), sensitive: Some(sensitive), ..Default::default() };
        assert_eq!(pick_free_model(&all, None, None, false).as_deref(), Some(engines::OPENCODE_DEFAULT));
        assert_eq!(pick_free_model(&all, None, Some(&j("opencode/beam-free", 0.9, 0.0)), false).as_deref(), Some("opencode/beam-free"));
        assert_eq!(pick_free_model(&all, None, Some(&j("opencode/beam-free", 0.2, 0.0)), false).as_deref(), Some(engines::OPENCODE_DEFAULT), "확신이 낮으면 기본");
        let with_vl: Vec<String> = all.iter().cloned().chain([engines::OPENCODE_IMAGE_FIRST.to_string()]).collect();
        assert_eq!(pick_free_model(&with_vl, None, Some(&j("opencode/beam-free", 0.9, 0.0)), true).as_deref(), Some(engines::OPENCODE_IMAGE_FIRST), "이미지는 시험한 모델로");
        assert_eq!(pick_free_model(&with_vl, None, Some(&j("opencode/beam-free", 0.9, 0.9)), true).as_deref(), Some(engines::OPENCODE_DEFAULT), "민감한 이미지는 보관하지 않는 모델로");
        assert_eq!(pick_free_model(&all, None, Some(&j("opencode/beam-free", 0.9, 0.0)), true).as_deref(), Some(engines::OPENCODE_DEFAULT), "이미지를 못 읽는 모델은 고르지 않는다");
        assert_eq!(pick_free_model(&all, None, Some(&j("opencode/mimo-v2.6-flash-free", 0.9, 0.9)), false).as_deref(), Some(engines::OPENCODE_DEFAULT), "민감하면 보관하지 않는 모델만");
        assert_eq!(pick_free_model(&all, Some("opencode/glm-5.3-flash-free"), None, false).as_deref(), Some("opencode/glm-5.3-flash-free"), "설정 우선");
        assert_eq!(pick_free_model(&all, Some("opencode/beam-free"), Some(&j("opencode/beam-free", 0.9, 0.9)), false).as_deref(), Some(engines::OPENCODE_DEFAULT), "설정도 민감한 요청에는 양보");
        assert_eq!(pick_free_model(&all, Some("anthropic/claude-opus-5-5"), None, false).as_deref(), Some(engines::OPENCODE_DEFAULT), "유료 모델은 없다");
        let only_paidless = vec!["opencode/glm-5.3-flash-free".to_string()];
        assert_eq!(pick_free_model(&only_paidless, None, None, false).as_deref(), Some("opencode/glm-5.3-flash-free"), "기본 모델이 사라지면 있는 것 중에서");
    }

    #[test]
    fn route_reads_attachments_and_asks_privacy_and_risk() {
        use EngineId::*;
        let (s, q) = route_request("요약해 줘", &RouteAsk { core: true, files: vec![json!({"name": "a.txt"})], free_models: vec!["opencode/beam-free".into(), "opencode/big-pickle".into()], work: true, engines: vec![Claude, OpenCode], ..Default::default() });
        assert_eq!(s["attachments"][0]["name"], "a.txt");
        for k in ["sensitive", "risk", "free_model", "engine"] {
            assert!(q.contains_key(k), "{k}");
        }
        assert_eq!(q["free_model"]["criteria"].as_object().unwrap().len(), 2);
        let (s, q) = route_request("안녕", &RouteAsk { core: true, free_models: vec!["opencode/beam-free".into()], ..Default::default() });
        assert!(s.get("attachments").is_none() && !q.contains_key("risk") && !q.contains_key("free_model"), "선택지가 없으면 묻지 않는다");
    }

    #[test]
    fn opencode_routing_stays_within_its_strength() {
        use EngineId::*;
        let (_t, c) = catalog();
        // 쉬운 요청은 JEV가 무료 AI를 고를 수 있고, 어려운 요청은 따르지 않는다.
        let pick = |d: f64| Judgment { engine: Some("opencode".into()), engine_confidence: Some(0.9), difficulty: Some(d), difficulty_confidence: Some(0.9), ..Default::default() };
        let easy = plan("간단히 정리해 줘", None, false, &[Claude, OpenCode], &c, ReviewMode::Off, Some(&pick(0.5))).unwrap();
        assert_eq!(easy.primary, OpenCode);
        let hard = plan("분산 시스템을 설계해 줘", None, false, &[Claude, OpenCode], &c, ReviewMode::Off, Some(&pick(2.8))).unwrap();
        assert_eq!(hard.primary, Claude);
        assert_eq!(plan("안녕", Some(OpenCode), false, &[Claude, OpenCode], &c, ReviewMode::Off, None).unwrap().primary, OpenCode, "직접 고르면 그대로");
        assert_eq!(stronger_reviewer(OpenCode, &[OpenCode, GeminiApi, Claude]), Some(Claude));
        assert_eq!(stronger_reviewer(Claude, &[Claude, OpenCode]), None, "더 약한 AI에게 검토를 맡기지 않는다");
    }

    #[test]
    fn unreadable_attachments_move_cli_first() {
        use EngineId::*;
        let (_t, c) = catalog();
        let zip = AttachedFile { name: "a.zip".into(), path: PathBuf::from("a.zip"), kind: "file".into(), mime: String::new(), size: 1 };
        let txt = AttachedFile { kind: "text".into(), ..zip.clone() };
        let chat = Judgment { task: Some("chat".into()), task_confidence: Some(0.9), ..Default::default() };
        let base = || plan("안녕", None, false, &[GeminiApi, Claude], &c, ReviewMode::Off, Some(&chat)).unwrap();
        assert_eq!(base().primary, GeminiApi);
        let mut p = base();
        prefer_file_readers(&mut p, std::slice::from_ref(&zip), None);
        assert_eq!((p.primary, p.fallbacks[0]), (Claude, GeminiApi));
        let mut p = base();
        prefer_file_readers(&mut p, &[txt], None);
        assert_eq!(p.primary, GeminiApi, "글 파일은 API도 읽는다");
        let mut p = base();
        prefer_file_readers(&mut p, &[zip], Some(GeminiApi));
        assert_eq!(p.primary, GeminiApi, "직접 고른 AI는 그대로");
    }

    #[test]
    fn route_asks_only_what_is_needed() {
        use EngineId::*;
        let (_t, c) = catalog();
        let skills: Vec<&Skill> = c.enabled_skills().collect();
        let (s, q) = route_request("안녕", &RouteAsk { core: true, skills: skills.clone(), engines: vec![Claude, Codex], parts: true, ..Default::default() });
        for k in ["task", "skill", "skill_fit", "engine", "parts"] {
            assert!(q.contains_key(k), "{k}");
        }
        assert!(s["skills"].as_object().unwrap().contains_key("root-cause"));
        let (s, q) = route_request("안녕", &RouteAsk { core: false, skills: skills[..1].to_vec(), engines: vec![Claude], parts: false, ..Default::default() });
        assert!(!q.contains_key("task") && !q.contains_key("skill") && !q.contains_key("engine") && !q.contains_key("parts"), "선택지가 하나면 Choice를 묻지 않는다");
        assert!(q.contains_key("skill_fit") && s["skills"].as_object().unwrap().len() == 1);
        let (_, q) = route_request("안녕", &RouteAsk { core: true, ..Default::default() });
        assert_eq!(q.len(), jev_questions().len());
    }

    #[test]
    fn talking_to_an_agent_interrupts_or_steers() {
        let stop = CancellationToken::new();
        let turn = stop.child_token();
        let mut line = AgentLine { stop: stop.clone(), turn: turn.clone(), inbox: vec![], steer: None };
        line.deliver("테스트도".into());
        assert!(turn.is_cancelled() && !stop.is_cancelled(), "차례만 끊는다");
        assert_eq!(line.inbox, vec!["테스트도"]);
        // API 작업 모드: 끊지 않고 칸에 넣는다
        let steer: Steer = std::sync::Arc::new(Mutex::new(Some(vec![])));
        let turn = stop.child_token();
        let mut line = AgentLine { stop, turn: turn.clone(), inbox: vec![], steer: Some(steer.clone()) };
        line.deliver("README도".into());
        assert!(!turn.is_cancelled() && line.inbox.is_empty());
        assert_eq!(steer.lock().unwrap().as_ref().unwrap(), &vec!["README도".to_string()]);
        // 칸이 닫혀 있으면(CLI) 끊는 쪽으로
        *steer.lock().unwrap() = None;
        line.deliver("다시".into());
        assert!(turn.is_cancelled());
    }

    #[test]
    fn window_test_results_close_the_final_answer() {
        let mut t = "끝".to_string();
        tests_tail(&mut t, &[]);
        assert_eq!(t, "끝");
        tests_tail(&mut t, &["[명령 실패] gradlew runGameTest\nlong log".to_string(), "[명령 성공] npm run e2e\nok".to_string()]);
        assert!(t.contains(TESTS_FOR_REPORT.trim()) && t.contains("- [명령 실패] gradlew runGameTest\n") && !t.contains("long log"), "{t}");
    }

    #[test]
    fn work_mode_tells_every_ai_to_own_its_work() {
        assert!(OWNERSHIP_SYSTEM.contains(crate::lead::LEFT_MARK), "the leftover marker must be spelled exactly");
        assert!(system_prompt(true, "C:/w", None).contains(OWNERSHIP_SYSTEM.trim()));
        assert!(!system_prompt(false, "C:/w", None).contains(OWNERSHIP_SYSTEM.trim()));
    }

    #[test]
    fn stalled_work_resumes_the_same_session_or_resends() {
        let spec = |e: EngineId, resume: Option<&str>| RunSpec { engine: e, prompt: "원래 요청".into(), system: String::new(), cwd: PathBuf::from("."), work: true, resume: resume.map(str::to_string), plugin_dirs: vec![], model: None, effort: None, max_tokens: None, extra_dirs: vec![], steer: None, attachments: vec![] };
        let n = stall_resume(&spec(EngineId::Claude, None), Some("sid".into()));
        assert_eq!(n.resume.as_deref(), Some("sid"));
        assert!(!n.prompt.contains("원래 요청"), "같은 대화를 이으면 안내만 보낸다");
        let n = stall_resume(&spec(EngineId::Claude, Some("old")), None);
        assert_eq!(n.resume.as_deref(), Some("old"), "세션 번호를 못 받았으면 앞 세션을 잇는다");
        let n = stall_resume(&spec(EngineId::Codex, None), Some("sid".into()));
        assert!(n.resume.is_none() && n.prompt.starts_with("원래 요청") && n.prompt.len() > "원래 요청".len());
    }

    #[test]
    fn absorbed_turns_keep_earlier_text_and_add_usage() {
        let mut e = Outcome { text: "앞 글".into(), input_tokens: Some(10), session_id: Some("a".into()), ..Default::default() };
        absorb(&mut e, Outcome { input_tokens: Some(5), latency_ms: 7, activities: vec!["Read".into()], ..Default::default() });
        assert_eq!((e.text.as_str(), e.input_tokens, e.latency_ms, e.session_id.as_deref()), ("앞 글", Some(15), 7, Some("a")));
        absorb(&mut e, Outcome { text: "새 글".into(), session_id: Some("b".into()), ..Default::default() });
        assert_eq!((e.text.as_str(), e.session_id.as_deref(), e.activities.len()), ("새 글", Some("b"), 1));
    }

    #[test]
    fn follow_up_resumes_or_resends() {
        let spec = |e: EngineId, work: bool| RunSpec { engine: e, prompt: "원래".into(), system: String::new(), cwd: PathBuf::from("."), work, resume: None, plugin_dirs: vec![], model: None, effort: None, max_tokens: None, extra_dirs: vec![], steer: None, attachments: vec![] };
        let o = Outcome { text: "쓰던 글".into(), session_id: Some("sid".into()), ..Default::default() };
        let n = follow_up(&spec(EngineId::Claude, false), "원래", &o, &["짧게".to_string()]);
        assert_eq!(n.resume.as_deref(), Some("sid"));
        assert!(n.prompt.contains("짧게") && !n.prompt.contains("쓰던 글"), "세션이 있으면 메시지만");
        let n = follow_up(&spec(EngineId::Codex, true), "원래 요청", &o, &["짧게".to_string()]);
        assert!(n.resume.is_none() && n.prompt.contains("원래 요청") && n.prompt.contains("쓰던 글") && n.prompt.contains("짧게") && n.prompt.contains("이미 고친 파일"));
        // 세션 번호를 받기 전에 끊겨도 원래 요청을 잃지 않는다
        let early = Outcome::default();
        let n = follow_up(&spec(EngineId::Claude, false), "원래 요청", &early, &["짧게".to_string()]);
        assert!(n.resume.is_none() && n.prompt.contains("원래 요청"));
    }

    #[test]
    fn side_prompt_carries_progress_and_thread_only() {
        let thread = vec![SideTurn { role: "user".into(), text: "앞 질문".into() }, SideTurn { role: "assistant".into(), text: "앞 답".into() }];
        let p = side_prompt("어디까지 됐어?", Some("초안 작성 중"), &thread);
        assert!(p.contains("초안 작성 중") && p.contains("앞 질문") && p.contains("앞 답") && p.ends_with("어디까지 됐어?"));
        assert!(side_prompt("q", None, &[]).contains("실행 중인 작업이 없습니다"));
    }

    #[test]
    fn skills_shape_the_plan() {
        let (_t, c) = catalog();
        let p = plan("/summarize 이 글", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap();
        let (r, body) = p.skill.as_ref().unwrap();
        assert_eq!((r.name.as_str(), r.how), ("summarize", "explicit"));
        assert!(body.contains("요약"));
        assert_eq!(p.prompt, "이 글");
        // 작업 모드 스킬은 작업 모드로, 스킬이 지정한 엔진 사용
        let p = plan("/fix 오타", None, false, &[EngineId::Claude, EngineId::Codex], &c, ReviewMode::Auto, None).unwrap();
        assert!(p.work);
        assert_eq!(p.primary, EngineId::Codex);
        let sys = system_prompt(p.work, "C:\\w", p.skill.as_ref());
        assert!(sys.contains("작업 모드") && sys.contains("[스킬: fix]") && sys.contains("C:\\w"));
        assert!(system_prompt(false, "C:\\w", None).contains("고치거나 명령을 실행하지 마세요"));
    }

    #[test]
    fn apis_jev_and_vendor_review() {
        use EngineId::*;
        let (_t, c) = catalog();
        // CLI 없이 API만 있어도 대화는 된다
        let p = plan("안녕", None, false, &[Nvidia], &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.primary, Nvidia);
        // 작업 모드도 API로 된다(엘리시움 도구). 직접 고르면 그 API, 자동이면 CLI가 먼저
        assert!(plan("파일 고쳐", None, true, &[Nvidia, OpenAi], &c, ReviewMode::Auto, None).unwrap().primary.is_api());
        assert_eq!(plan("파일 고쳐", Some(OpenAi), true, &[Claude, OpenAi], &c, ReviewMode::Auto, None).unwrap().primary, OpenAi);
        assert_eq!(plan("파일 고쳐", None, true, &[Claude, OpenAi], &c, ReviewMode::Auto, None).unwrap().primary, Claude);
        assert_eq!(plan("파일 고쳐", None, true, &[GeminiApi, Codex], &c, ReviewMode::Auto, None).unwrap().primary, Codex);
        // 검토는 다른 회사 AI 우선: Claude CLI 초안 → Claude API(같은 회사)보다 Gemini API
        let deep = "새 서비스 출시 전략을 자세히 세워줘";
        let p = plan(deep, None, false, &[Claude, ClaudeApi, GeminiApi], &c, ReviewMode::Auto, None).unwrap();
        assert_eq!((p.primary, p.reviewer), (Claude, Some(GeminiApi)));
        // JEV: 가벼운 대화 → 빠른 API 먼저, 난이도 낮으면 검토 안 함
        let j = Judgment { model: "jev".into(), task: Some("chat".into()), task_confidence: Some(0.9), difficulty: Some(0.4), difficulty_confidence: Some(0.8), stakes: None, stakes_confidence: None, fresh: None, local: None, ms: 1, cost_usd: 0.0, ..Default::default() };
        let p = plan(deep, None, false, &[Claude, GeminiApi], &c, ReviewMode::Auto, Some(&j)).unwrap();
        assert_eq!(p.primary, GeminiApi);
        assert!(p.reviewer.is_none(), "JEV 난이도가 낮으면 규칙(자세히)보다 우선");
        assert!(p.reasons.iter().any(|r| r.contains("JEV")));
        // JEV: 어려운 코드 → CLI 먼저, 검토
        let j = Judgment { task: Some("code".into()), difficulty: Some(2.6), ..j };
        let p = plan("이거 해줘", None, false, &[Claude, GeminiApi], &c, ReviewMode::Auto, Some(&j)).unwrap();
        assert_eq!((p.primary, p.reviewer), (Claude, Some(GeminiApi)));
        // 확신이 낮은 판단은 무시
        let j = Judgment { task: Some("chat".into()), task_confidence: Some(0.2), difficulty: Some(3.0), difficulty_confidence: Some(0.1), ..j };
        let p = plan("안녕", None, false, &[Claude, GeminiApi], &c, ReviewMode::Auto, Some(&j)).unwrap();
        assert_eq!(p.primary, Claude);
        assert!(p.reviewer.is_none());
        // 질문은 두 개(task·difficulty)
        let q = jev_questions();
        assert_eq!(q["task"]["type"], "choice");
        assert_eq!(q["difficulty"]["criteria"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn jev_difficulty_sets_model_and_effort() {
        let j = |task: &str, d: f64, conf: f64| Judgment { model: "jev".into(), task: Some(task.into()), task_confidence: Some(0.9), difficulty: Some(d), difficulty_confidence: Some(conf), stakes: None, stakes_confidence: None, fresh: None, local: None, ms: 1, cost_usd: 0.0, ..Default::default() };
        assert_eq!(budget(None, false), None, "JEV가 없으면 기본값");
        let b = budget(Some(&j("code", 0.5, 0.9)), false).unwrap();
        assert_eq!((b.tier, b.effort, b.claude_model, b.max_tokens), ("light", "low", Some("haiku"), 1500));
        assert_eq!(budget(Some(&j("chat", 1.3, 0.9)), false).unwrap().tier, "light", "가벼운 대화는 조금 어려워도 가볍게");
        assert_eq!(budget(Some(&j("code", 1.3, 0.9)), false).unwrap().tier, "standard");
        let deep = budget(Some(&j("analysis", 2.4, 0.9)), false).unwrap();
        assert_eq!((deep.tier, deep.effort, deep.claude_model), ("deep", "high", None));
        assert_eq!(budget(Some(&j("code", 0.2, 0.9)), true).unwrap().tier, "standard", "작업 모드는 가볍게 내리지 않음");
        assert_eq!(budget(Some(&j("code", 0.2, 0.2)), false), None, "확신이 낮으면 쓰지 않음");
        // 계획에 실린다
        let (_t, c) = catalog();
        let p = plan("안녕", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, Some(&j("chat", 0.2, 0.9))).unwrap();
        assert_eq!(p.budget.as_ref().unwrap().claude_model, Some("haiku"));
        assert!(p.reasons.iter().any(|r| r.contains("가볍게")));
    }

    #[test]
    fn review_and_debate_prompts() {
        let r = review_prompt("요청", "초안", &[]);
        assert!(r.contains("[요청]\n요청") && r.contains("[초안]\n초안") && !r.contains("확인한 자료"));
        let r = review_prompt("요청", "초안", &["Read · C:/a.md".to_string()]);
        assert!(r.contains("[초안을 쓴 AI가 확인한 자료]\n- Read · C:/a.md") && r.contains("'모름'으로 바꾸지 말고"));
        let draft = "'조작할 수 있게' 항목은 7개이고 첫 항목은 키보드로 모든 기능을 쓸 수 있어야 한다는 것입니다.";
        assert!(review_is_degraded(draft, "모름"), "초안을 '모름'으로 바꾸면 버린다");
        assert!(review_is_degraded(&"가".repeat(500), "짧음"), "지나치게 짧아지면 버린다");
        assert!(!review_is_degraded(draft, "'조작할 수 있게' 항목은 7개입니다. 첫 항목: 모든 기능을 키보드로 쓸 수 있어야 합니다."));
        assert!(!review_is_degraded("짧은 초안", "모름"), "초안도 짧으면 판단하지 않음");
        let x = rebuttal_prompt("질문", "내 답", "NVIDIA API", "남의 답");
        assert!(x.contains("[다른 AI(NVIDIA API)의 답]\n남의 답") && x.contains("## 고친 답"));
        assert_eq!(revised_part("## 반론\n- 틀림\n## 고친 답\n정답"), "정답");
        assert_eq!(revised_part("그냥 답"), "그냥 답");
        let j = judge_prompt("질문", "Claude Code", "a", "NVIDIA API", "b", Some(("## 반론\n- x\n## 고친 답\na2", "## 반론\n- y")));
        assert!(j.contains("[답 1: Claude Code]\na") && j.contains("[Claude Code의 반론]\n## 반론\n- x") && !j.contains("a2"));
    }

    fn jj(task: &str, d: f64, stakes: f64) -> Judgment {
        Judgment { model: "jev".into(), task: Some(task.into()), task_confidence: Some(0.9), difficulty: Some(d), difficulty_confidence: Some(0.9), stakes: Some(stakes), stakes_confidence: Some(0.9), fresh: None, local: None, ms: 1, cost_usd: 0.0, ..Default::default() }
    }

    #[test]
    fn accuracy_critical_requests_are_debated_by_two_vendors() {
        let (_t, c) = catalog();
        let both = [EngineId::Claude, EngineId::Nvidia];
        let p = plan("이 약 두 개를 같이 먹어도 되는지 알려줘", None, false, &both, &c, ReviewMode::Auto, Some(&jj("research", 1.5, 2.8))).unwrap();
        let d = p.debate.clone().expect("정확도가 중요하면 토론");
        assert_eq!((d.a, d.b), (EngineId::Claude, EngineId::Nvidia));
        assert_eq!(d.judge, EngineId::Claude, "세 번째 회사가 없으면 더 강한 참가자가 판정");
        assert!(d.rebuttal && p.reviewer.is_none(), "토론이 검토를 대신한다");
        assert!(!p.reasons.iter().any(|r| r.contains("검토")), "토론이면 검토 이유는 남기지 않는다: {:?}", p.reasons);
        // 세 번째 회사가 있으면 그쪽이 판정
        let p = plan("법적으로 문제없는지 확인해 줘", None, false, &[EngineId::Claude, EngineId::Nvidia, EngineId::GeminiApi], &c, ReviewMode::Auto, Some(&jj("analysis", 2.0, 3.0))).unwrap();
        let d = p.debate.unwrap();
        assert_eq!((d.b, d.judge), (EngineId::GeminiApi, EngineId::Claude), "판정은 후보 중 가장 강한 AI");
        // 사용자가 약한 AI를 골라도 판정은 강한 쪽이
        let p = plan("이 계산 맞는지 정확히 확인해 줘", Some(EngineId::Nvidia), false, &both, &c, ReviewMode::Auto, Some(&jj("analysis", 1.5, 2.5))).unwrap();
        let d = p.debate.unwrap();
        assert_eq!((d.a, d.b, d.judge), (EngineId::Nvidia, EngineId::Claude, EngineId::Claude));
        // 세 번째 회사가 참가자만큼 강하면 그쪽이 판정
        let p = plan("법적으로 정확히", None, false, &[EngineId::GeminiApi, EngineId::Nvidia, EngineId::Antigravity, EngineId::OpenAi], &c, ReviewMode::Auto, Some(&jj("chat", 1.5, 2.5))).unwrap();
        assert_eq!(p.debate.unwrap().judge, EngineId::OpenAi);
        // 가벼운 단계면 반론 라운드 생략, 정확도 낮으면 토론 안 함, 작업 모드도 안 함
        let p = plan("서울 인구?", None, false, &both, &c, ReviewMode::Auto, Some(&jj("chat", 0.5, 2.2))).unwrap();
        assert!(!p.debate.unwrap().rebuttal);
        assert!(plan("안녕", None, false, &both, &c, ReviewMode::Auto, Some(&jj("chat", 0.1, 0.0))).unwrap().debate.is_none());
        assert!(plan("파일 고쳐", None, true, &both, &c, ReviewMode::Debate, None).unwrap().debate.is_none());
        // 다른 AI가 없으면 토론할 수 없다
        assert!(plan("정확히 계산해 줘", None, false, &[EngineId::Claude], &c, ReviewMode::Debate, None).unwrap().debate.is_none());
        // JEV가 없으면 낱말로, 토론 모드면 항상
        assert!(plan("이 숫자가 정확한지 검증해 줘", None, false, &both, &c, ReviewMode::Auto, None).unwrap().debate.is_some());
        assert!(plan("시 한 편 써 줘", None, false, &both, &c, ReviewMode::Debate, None).unwrap().debate.is_some());
        assert!(plan("이 숫자가 정확한지 검증해 줘", None, false, &both, &c, ReviewMode::Off, None).unwrap().debate.is_none());
    }

    #[test]
    fn skill_can_fix_tier_and_ask_for_debate() {
        let tmp = tempfile::tempdir().unwrap();
        let w = tmp.path().join("skills/fact");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::write(w.join("SKILL.md"), "---\nname: fact\ndescription: 사실 확인\ndebate: true\ntier: light\n---\n확인하라").unwrap();
        let c = skills::load(tmp.path(), &HashMap::new());
        let p = plan("/fact 지구 둘레", None, false, &[EngineId::Claude, EngineId::Nvidia], &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.budget.as_ref().map(|b| b.tier), Some("light"));
        assert!(p.debate.is_some_and(|d| !d.rebuttal), "가벼운 단계 스킬은 반론 생략");
    }

    #[test]
    fn skill_references_are_listed_and_readable_by_cli() {
        let (_t, c) = catalog();
        let p = plan("/ui-craft 설정 화면 만들어 줘", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap();
        let (r, body) = p.skill.unwrap();
        assert!(r.dir.is_some(), "참고 파일이 있는 스킬은 폴더를 CLI에 넘긴다");
        assert!(body.contains("[이 스킬의 참고 파일]") && body.contains("accessibility.md") && body.contains("필요한 파일만"));
        let p = plan("/summarize 이 글", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap();
        assert!(p.skill.unwrap().0.dir.is_none(), "참고 파일 없는 스킬은 폴더를 넘기지 않음");
    }

    #[test]
    fn team_named_in_the_request_beats_jev_and_settings() {
        use EngineId::*;
        let c = Catalog::default();
        let all = [Claude, Antigravity, OpenCode, Nvidia];
        let text = "**총괄 분할자 및 관리자**인 **Opus 5.5**입니다. **4개의 Gemini 3.8 Flash (High) 하위 에이전트(Worker)**를 병렬 구동하십시오.";
        let p = plan_with(text, None, true, &all, &c, ReviewMode::Auto, None, 2).unwrap();
        let t = p.team.expect("요청 글의 팀");
        assert_eq!(t.agents, vec![TeamAgent { engine: Antigravity, model: Some("gemini-3.8-flash".into()) }; 4]);
        assert_eq!(t.agent_effort.as_deref(), Some("high"));
        assert_eq!(t.synth, Claude);
        assert_eq!(p.primary, Claude);
        assert!(p.models.contains(&(Claude, "opus".to_string())), "{:?}", p.models);
        assert!(p.prompt.contains("Gemini 3.8 Flash"), "지시문은 그대로 넘긴다");
    }

    #[test]
    fn written_mixed_teams_run_exactly_as_written() {
        use EngineId::*;
        let c = Catalog::default();
        let all = [Claude, Antigravity, OpenCode, Nvidia, ClaudeApi, GeminiApi];
        let text = "팀은 총관인 Opus 5.5 high 중심으로\n1팀 - Claude Haiku 5.5 high, Gemini flash 3.8 high\n2팀 - Claude Haiku 5.5 high, Gemini flash 3.8 low";
        let p = plan_with(text, None, true, &all, &c, ReviewMode::Auto, None, 2).unwrap();
        let t = p.team.clone().expect("적힌 팀");
        let (h, g) = (TeamAgent { engine: Claude, model: Some("haiku".into()) }, TeamAgent { engine: Antigravity, model: Some("gemini-3.8-flash".into()) });
        assert_eq!(t.agents, vec![h.clone(), g.clone(), h.clone(), g.clone()]);
        assert_eq!(t.groups, vec![2, 2]);
        assert_eq!(t.member_efforts[3].as_deref(), Some("low"));
        assert!(t.pinned);
        assert_eq!((p.primary, t.synth), (Claude, Claude));
        assert!(p.models.contains(&(Claude, "opus".to_string())), "{:?}", p.models);
        assert!(!p.fallbacks.iter().any(|e| e.is_api()), "{:?}", p.fallbacks);
        assert!(plan_with(text, None, true, &[Antigravity, OpenCode], &c, ReviewMode::Auto, None, 2).is_err(), "돌릴 수 없으면 다른 AI로 바꾸지 않는다");
        assert_eq!(written_units(&t.agents, &t.groups, 2), vec![vec![h.clone(), g.clone()], vec![h.clone(), g.clone()]], "페어가 흩어지지 않는다");
        assert_eq!(written_units(&t.agents, &t.groups, 1), vec![vec![h, g]]);
    }

    #[test]
    fn team_written_as_content_runs_one_ai() {
        use EngineId::*;
        let c = Catalog::default();
        let all = [Claude, Antigravity, OpenCode, ClaudeApi];
        let text = "다음 세션에서 작업의 남은 오류를 완료할 수 있게 프롬프트를 만들어줘\n프롬프트는 Gemini 3.8 flash로 작성해줘\n\n총괄은 Opus 5.5 high로, 워커는 코딩용 Sonnet 5.5 high 3대, 문서작성용 Gemini 3.8 flash high 1대로 해줘";
        let parsed = plan_read(text, None, true, &all, &c, ReviewMode::Auto, None, 0, TeamText::Parse).unwrap();
        assert!(parsed.team.is_some(), "규칙만으로는 팀을 돌린다(이 문제를 OpenCode가 막는다)");
        let p = plan_read(text, None, true, &all, &c, ReviewMode::Auto, None, 0, TeamText::Ignore).unwrap();
        assert!(p.team.is_none(), "글의 내용으로 적은 팀은 돌리지 않는다");
        assert_eq!(p.primary, Antigravity, "이 요청에 대해 지시한 모델(Gemini 3.8 flash)이 답한다");
        assert_eq!(p.models.first().map(|m| m.1.as_str()), Some("gemini-3.8-flash"));
    }

    #[test]
    fn team_lines_read_by_an_ai_decide_the_team() {
        use EngineId::*;
        let c = Catalog::default();
        let all = [Claude, Antigravity, OpenCode, ClaudeApi];
        let lines = "총괄 - Claude Opus 5.5 high\n1팀 - Claude Haiku 5.5 high, Gemini 3.8 Flash high\n2팀 - Claude Haiku 5.5 high, Gemini 3.8 Flash high";
        let p = plan_read("Haiku랑 Flash를 짝지어 두 쌍으로 해 줘", None, true, &all, &c, ReviewMode::Auto, None, 0, TeamText::Lines(lines.to_string())).unwrap();
        let t = p.team.expect("구성표대로");
        assert_eq!((t.agents.len(), t.groups), (4, vec![2, 2]));
        assert_eq!(p.primary, Claude);
        assert!(p.models.contains(&(Claude, "opus".to_string())));
    }

    #[test]
    fn team_built_in_the_setup_panel_is_run_as_built() {
        use EngineId::*;
        let all = [Claude, Antigravity, Codex];
        let seat = |m: &str, e: Option<&str>, n: u8| SetupSeat { model: m.into(), effort: e.map(str::to_string), count: n };
        let t = SetupTeam {
            lead: Some(seat("Claude Opus", Some("high"), 1)),
            groups: vec![
                SetupGroup { dept: "개발 / 프론트".into(), members: vec![seat("Claude Haiku", Some("low"), 1), seat("Gemini 3.8 Flash", None, 1)] },
                SetupGroup { dept: String::new(), members: vec![seat("Codex", None, 2)] },
                SetupGroup { dept: "빈 팀".into(), members: vec![] },
            ],
        };
        let r = t.roster(&all, true).unwrap();
        assert_eq!(r.teams.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2]);
        assert_eq!(r.depts, vec![vec!["개발".to_string(), "프론트".to_string()], vec![]]);
        assert_eq!((r.teams[0][0].effort, r.teams[0][1].effort), (Some("low"), None));
        // The request text names other models; the panel's team still wins.
        let p = plan_read("opus 4개로 해 줘", None, true, &all, &Catalog::default(), ReviewMode::Auto, None, 0, TeamText::Given(r)).unwrap();
        let team = p.team.expect("panel team");
        assert_eq!((team.agents.len(), team.groups.clone(), team.pinned), (4, vec![2, 2], true));
        assert_eq!(team.agents[2].engine, Codex);
        assert_eq!(p.primary, Claude);
        // Fewer than two seats or a model nobody can run stop before anything starts.
        assert!(SetupTeam { lead: None, groups: vec![SetupGroup { dept: String::new(), members: vec![seat("Codex", None, 1)] }] }.roster(&all, true).is_err());
        assert!(SetupTeam { lead: None, groups: vec![SetupGroup { dept: String::new(), members: vec![seat("DeepSeek", None, 2)] }] }.roster(&all, true).is_err());
    }

    #[test]
    fn team_reader_never_picks_an_api() {
        use EngineId::*;
        assert_eq!(team_reader(&[ClaudeApi, Claude]), Some((Claude, Some("haiku".to_string()))));
        assert_eq!(team_reader(&[GeminiApi, Antigravity]), Some((Antigravity, Some("gemini-3.8-flash".to_string()))));
        assert_eq!(team_reader(&[ClaudeApi, OpenCode]), Some((OpenCode, None)));
        assert_eq!(team_reader(&[ClaudeApi, GeminiApi]), None);
    }

    #[test]
    fn work_team_never_fills_seats_with_apis_while_a_cli_exists() {
        use EngineId::*;
        let c = Catalog::default();
        let all = [Claude, Antigravity, OpenCode, Nvidia, ClaudeApi, GeminiApi, OpenAi];
        let p = plan_with("에이전트 8개로 고쳐 줘", None, true, &all, &c, ReviewMode::Auto, None, 2).unwrap();
        let t = p.team.expect("팀");
        assert_eq!(t.agents.len(), 8);
        assert!(t.agents.iter().all(|a| !a.engine.is_api()), "{:?}", t.agents);
        let only_api = plan_with("에이전트 3개로 고쳐 줘", None, true, &[ClaudeApi, GeminiApi], &c, ReviewMode::Auto, None, 2).unwrap();
        assert_eq!(only_api.team.expect("CLI가 없으면 API로 돈다").agents.len(), 3);
    }

    #[test]
    fn every_agent_is_told_elysium_runs_the_other_ais() {
        for work in [true, false] {
            let sys = system_prompt(work, "C:\\w", None);
            assert!(["agy", "just-code-claude", "gemini"].iter().all(|n| sys.contains(n)), "{sys}");
        }
    }

    #[test]
    fn requested_model_and_parallel_agents_shape_the_plan() {
        use EngineId::*;
        let (_t, c) = catalog();
        let all = [Claude, Antigravity, GeminiApi, Nvidia];
        // 요청 글의 모델 → 그 엔진과 모델, 같은 모델을 쓸 수 있는 다른 AI가 대신할 수 있다
        let p = plan("opus로 이 함수 설명해 줘", None, false, &all, &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.primary, Claude);
        assert_eq!(p.prompt, "이 함수 설명해 줘", "모델 지시는 AI에게 넘기지 않음");
        assert_eq!(p.models, vec![(Claude, "opus".to_string()), (Antigravity, "claude-opus-5-5".to_string())]);
        assert_eq!(p.fallbacks, vec![Antigravity]);
        assert!(p.team.is_none());
        // 입력창에서 고른 AI가 그 모델을 쓸 수 있으면 그 AI로
        let p = plan("opus로 답해", Some(Antigravity), false, &all, &c, ReviewMode::Auto, None).unwrap();
        assert_eq!(p.primary, Antigravity);
        assert!(p.models.contains(&(Antigravity, "claude-opus-5-5".to_string())));
        // 에이전트 수를 요청하면 팀: 다른 회사 AI부터, 검토·토론 대신 종합
        let p = plan("에이전트 3개로 출시 전략을 자세히 세워 줘", None, false, &all, &c, ReviewMode::Auto, None).unwrap();
        let t = p.team.clone().expect("팀");
        assert_eq!(t.agents.iter().map(|a| a.engine).collect::<Vec<_>>(), vec![Claude, Antigravity, Nvidia]);
        assert_eq!(t.synth, Claude, "가장 강한 AI가 종합");
        assert!(p.reviewer.is_none() && p.debate.is_none());
        // 설정의 기본 수, 엔진이 하나뿐이면 같은 엔진을 여러 개
        assert_eq!(plan_with("안녕", None, false, &all, &c, ReviewMode::Auto, None, 2).unwrap().team.unwrap().agents.len(), 2);
        let t = plan_with("안녕", None, false, &[Claude], &c, ReviewMode::Auto, None, 3).unwrap().team.unwrap();
        assert!(t.agents.len() == 3 && t.agents.iter().all(|a| a.engine == Claude));
        assert!(plan_with("안녕", None, false, &all, &c, ReviewMode::Auto, None, 1).unwrap().team.is_none(), "기본 1이면 끔");
        // 모델을 여럿 적으면 그 모델들로 팀
        let t = plan("opus랑 gemini로 동시에 답해 줘", None, false, &all, &c, ReviewMode::Auto, None).unwrap().team.unwrap();
        assert_eq!(t.agents, vec![TeamAgent { engine: Claude, model: Some("opus".into()) }, TeamAgent { engine: Antigravity, model: None }]);
        // 작업 모드는 파일 충돌을 막으려고 한 AI만, 정확도 요청이어도 팀이면 토론 대신 팀
        // 작업 모드도 나눠서 동시에 한다: CLI가 먼저, 통합도 CLI가 맡는다.
        let w = plan_with("파일 고쳐", None, true, &all, &c, ReviewMode::Auto, None, 3).unwrap();
        let t = w.team.expect("작업 팀");
        assert_eq!(t.agents.len(), 3);
        assert!(!t.agents[0].engine.is_api() && !t.agents[1].engine.is_api(), "{:?}", t.agents);
        assert!(!t.synth.is_api() && w.reviewer.is_none() && w.debate.is_none());
        assert!(plan_with("파일 고쳐", None, true, &all, &c, ReviewMode::Auto, None, 1).unwrap().team.is_none());
        let p = plan("에이전트 2개로 정확히 계산해 줘", None, false, &all, &c, ReviewMode::Auto, None).unwrap();
        assert!(p.team.is_some() && p.debate.is_none());
        let pr = team_prompt("질문", &[("Claude Code · opus".into(), "답1"), ("Antigravity".into(), "답2")]);
        assert!(pr.contains("[에이전트 1: Claude Code · opus]\n답1") && pr.contains("[에이전트 2: Antigravity]\n답2"));
    }

    #[test]
    fn council_prefers_cli_windows_from_different_vendors() {
        let (_t, c) = catalog();
        use EngineId::*;
        let all = [Claude, Codex, Antigravity, GeminiApi, Nvidia];
        let p = plan("이 설계의 장단점", None, false, &all, &c, ReviewMode::Auto, None).unwrap();
        let cn = council_plan(&p, &all, 3).unwrap();
        let engines: Vec<EngineId> = cn.agents.iter().map(|a| a.engine).collect();
        assert_eq!(engines, vec![Claude, Antigravity, Codex], "CLI 창 셋, 회사가 모두 다름");
        assert_eq!(cn.synth, Claude);
        assert_eq!(cn.rounds, 3);
        // 쓸 수 있는 AI가 하나뿐이면 같은 AI를 새 세션으로 둘 띄운다
        let one = council_plan(&p, &[Claude], 3).unwrap();
        assert_eq!(one.agents.len(), 3);
        // 최소 2명, 최대 4명
        assert_eq!(council_plan(&p, &all, 1).unwrap().agents.len(), 2);
        assert_eq!(council_plan(&p, &all, 9).unwrap().agents.len(), COUNCIL_MAX_AGENTS);
        // 가벼운 단계면 2라운드
        let mut light = p.clone();
        light.budget = budget_for("light");
        assert_eq!(council_plan(&light, &all, 2).unwrap().rounds, 2);
    }

    #[test]
    fn council_prompts_send_only_what_is_new() {
        let others = [("1라운드 · 참가자 2 · Codex".to_string(), "B안이 낫다\n합의: 아니오")];
        let f = council_followup(2, &others);
        assert!(f.contains("[당신이 마지막으로 말한 뒤 다른 참가자의 발언]") && f.contains("[1라운드 · 참가자 2 · Codex]\nB안이 낫다"));
        assert!(!f.contains("합의: 아니오\n\n"), "이전 발언의 합의 표시는 떼고 보낸다");
        assert!(f.contains("2라운드입니다"));
        let me = "참가자 1 · Claude Code".to_string();
        let log = [(1u8, me.clone(), "A안"), (1, "참가자 2 · Codex".to_string(), "B안")];
        let full = council_full("요청", 2, &me, &log);
        assert!(full.contains("[1라운드 · 참가자 1 · Claude Code(당신)]\nA안") && full.contains("[요청]\n요청"));
        let syn = council_synth("요청", 2, &log);
        assert!(syn.contains("[회의 기록]") && syn.contains("회의에서 갈린 점"));
    }

    #[test]
    fn agree_marker_is_read_and_stripped() {
        assert!(says_agree("결론은 A\n합의: 예"));
        assert!(says_agree("결론은 A\n**합의: 예**"));
        assert!(!says_agree("결론은 A\n합의: 아니오"));
        assert!(!says_agree("합의: 예라고 하기엔 이르다\n더 보자"));
        assert_eq!(strip_agree("결론은 A\n합의: 예"), "결론은 A");
        assert_eq!(strip_agree("결론은 A"), "결론은 A");
    }

    #[test]
    fn memory_summary_goes_to_the_cheapest_engine_first() {
        use EngineId::*;
        let order: Vec<EngineId> = summarizers(&[Codex, Claude, Antigravity, ClaudeApi, OpenAi, GeminiApi, Nvidia]).into_iter().map(|x| x.0).collect();
        assert_eq!(order.first(), Some(&Nvidia), "무료 체험 크레딧");
        assert_eq!(order.last(), Some(&Codex));
        let ca = order.iter().position(|e| *e == ClaudeApi).unwrap();
        let cc = order.iter().position(|e| *e == Claude).unwrap();
        assert!(ca < cc, "같은 Haiku 단가면 API가 먼저");
        assert_eq!(summarizers(&[ClaudeApi]), vec![(ClaudeApi, Some("claude-haiku-5-5"))]);
    }

    #[test]
    fn jev_tool_need_moves_cli_ahead_of_api() {
        let (_t, c) = catalog();
        use EngineId::*;
        let installed = [GeminiApi, Claude];
        let mut j = jj("chat", 0.5, 0.0);
        let p = plan("오늘 비트코인 시세", None, false, &installed, &c, ReviewMode::Off, Some(&j)).unwrap();
        assert_eq!(p.primary, GeminiApi, "가벼운 대화는 API 먼저");
        j.fresh = Some(0.9);
        let p = plan("오늘 비트코인 시세", None, false, &installed, &c, ReviewMode::Off, Some(&j)).unwrap();
        assert_eq!(p.primary, Claude, "최신 정보가 필요하면 웹 검색이 되는 CLI 먼저");
        assert!(p.reasons.iter().any(|r| r.starts_with("JEV")));
    }
}

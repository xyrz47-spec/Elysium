//! 요청 하나를 처리하는 흐름: 스킬 고르기 → 엔진 고르기 → (작업 모드면 승인) → 초안 → (필요하면) 다른 AI가 검토 → 저장.

use crate::audit;
use crate::db::{new_id, Db};
use crate::jev;
use crate::secrets::{self, SecretName};
use crate::engines::{self, EngineId, EngineInfo, Event, Outcome, RunSpec, Steer};
use crate::intent;
use crate::settings::{ReviewMode, Settings};
use crate::skills::{self, Catalog, Skill};
use crate::memory;
use crate::store;
use crate::teamwork;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub struct AppState {
    pub db: Db,
    pub http: reqwest::Client,
    pub data_dir: PathBuf,
    pub settings: RwLock<Settings>,
    pub engines: RwLock<Vec<EngineInfo>>,
    pub tasks: Mutex<HashMap<String, CancellationToken>>,
    /// 실행 중인 에이전트 하나하나("작업 번호/실행 key" → 작업 토큰의 자식). 에이전트 탭에서 하나만 멈출 수 있다.
    pub agents: Mutex<HashMap<String, CancellationToken>>,
    /// 작업 중 메시지 칸(작업 번호 → 칸). 작업이 끝나면 지운다.
    pub steers: Mutex<HashMap<String, Steer>>,
    pub approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
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
#[derive(Debug, Clone, Serialize)]
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
    q
}

fn cut(text: &str, n: usize) -> String {
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

pub async fn judge(http: &reqwest::Client, key: &str, text: &str) -> Result<Judgment, String> {
    let r = jev::ask(http, key, "jev-latest", json!({ "request": text }), jev_questions(), 1, Duration::from_secs(12)).await.map_err(|e| secrets::redact(&e.to_string()))?;
    let a = &r.response.answers;
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
        ms: r.latency_ms,
        cost_usd: r.response.usage.input_tokens as f64 * JEV_PRICE / 1e6,
    })
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
        "code" => vec![Claude, Codex, Antigravity, ClaudeApi, OpenAi, GeminiApi, Nvidia],
        "research" => vec![Antigravity, Claude, GeminiApi, ClaudeApi, OpenAi, Codex, Nvidia],
        "chat" | "translate" => vec![GeminiApi, Nvidia, ClaudeApi, OpenAi, Claude, Antigravity, Codex],
        "writing" | "analysis" => vec![Claude, ClaudeApi, Antigravity, GeminiApi, OpenAi, Codex, Nvidia],
        _ => vec![Claude, Antigravity, Codex, ClaudeApi, GeminiApi, OpenAi, Nvidia],
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
    let mut reasons = vec![];
    // 1) 스킬
    let (skill, prompt): (Option<&Skill>, String) = match catalog.explicit(text) {
        Some((s, rest)) => {
            reasons.push(format!("/{} 스킬을 불렀습니다", s.name));
            (Some(s), if rest.is_empty() { s.description.clone() } else { rest.to_string() })
        }
        None => match catalog.triggered(text) {
            Some(s) => {
                reasons.push(format!("요청에 맞는 '{}' 스킬을 자동으로 적용했습니다", s.name));
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
    let mut models: Vec<(EngineId, String)> = vec![];
    let picked = wanted.picks.first().map(|mp| match choice.and_then(|c| mp.alts.iter().find(|(e, _)| *e == c)) {
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
        for (e, m) in mp.alts.iter().filter(|(e, _)| *e != engine) {
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
    } else {
        let p = available[0];
        reasons.push(format!("{} 요청이라 {}가 먼저 답합니다", kind_label(&kind), p.label()));
        (p, available[1..].to_vec())
    };

    // 동시 에이전트: 요청 글의 모델 여럿 > 요청 글의 에이전트 수 > 설정의 기본 수
    let asked_agents = intent::requested_agents(text);
    let want = if wanted.picks.len() >= 2 { wanted.picks.len() } else { asked_agents.unwrap_or(agents as usize) }.min(intent::MAX_AGENTS);
    let team_from = reasons.len();
    let team = if want < 2 {
        None
    } else {
        let mut list: Vec<TeamAgent> = vec![];
        if wanted.picks.len() >= 2 {
            for mp in &wanted.picks {
                list.push(TeamAgent { engine: mp.engine, model: mp.model.clone() });
            }
        } else {
            list.push(TeamAgent { engine: primary, model: models.iter().find(|(e, _)| *e == primary).map(|(_, m)| m.clone()) });
            // 다른 회사의 AI → 다른 엔진 → (모자라면) 같은 엔진을 새 세션으로 더 띄운다. 작업 모드는 도구가 많은 CLI가 먼저다.
            let mut pool: Vec<EngineId> = order.iter().copied().filter(usable).collect();
            if work {
                pool.sort_by_key(|e| e.is_api());
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
        let synth = synth_pool.iter().copied().enumerate().max_by_key(|(i, e)| (strength(*e), std::cmp::Reverse(*i))).map(|(_, e)| e).unwrap_or(primary);
        let n = list.len();
        let names = list.iter().map(|a| match &a.model { Some(m) => format!("{}({m})", a.engine.label()), None => a.engine.label().to_string() }).collect::<Vec<_>>().join(", ");
        let synth_label = synth.label();
        if asked_agents.is_none() && wanted.picks.len() < 2 {
            reasons.push(format!("설정에 지정된 기본 실행 수에 따라 {n}개의 에이전트를 동시에 실행합니다."));
        }
        if work {
            reasons.push(format!("작업을 하위 작업으로 나누어 {n}개의 에이전트({names})가 별도 복사본에서 동시 작업한 후, 변경 사항을 병합하여 {synth_label}이 최종 검증 및 요약을 진행합니다."));
        } else {
            reasons.push(format!("{n}개 에이전트({names})의 개별 답변을 {synth_label}에서 하나로 종합하여 전달합니다."));
        }
        Some(Team { agents: list, synth })
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
    Ok(Plan { primary, fallbacks, reviewer, skill, prompt, work, reasons, jev: jev.cloned(), budget, debate, models, team, council: None, team_reasons })
}

pub fn system_prompt(work: bool, workspace: &str, skill: Option<&(SkillRef, String)>) -> String {
    let mut s = String::from(
        "당신은 데스크톱 AI 클라이언트 Elysium 안에서 일하는 AI입니다. 사용자는 한국어를 씁니다. 한국어로 답하세요.\n\
         결과는 바로 쓸 수 있게 완성된 형태로 주고, 필요하면 마크다운 제목·목록·코드 블록으로 정리하세요. 모르는 것은 추측하지 말고 모른다고 밝히세요.\n",
    );
    s.push_str(&format!("작업 폴더: {workspace}\n"));
    if work {
        s.push_str("작업 모드: 사용자가 이 작업 폴더 안의 파일 수정과 PowerShell 명령 실행을 허락했습니다(명령 도구가 있는 AI만). 요청한 변경만 하고, 명령은 작업 폴더 안에서만 쓰세요. 끝나면 바꾼 파일과 내용을 짧게 요약하세요.\n");
    } else {
        s.push_str("대화 모드: 파일을 읽을 수는 있지만 고치거나 명령을 실행하지 마세요. 파일을 바꿔야 하면 바꿀 내용을 보여 주고 '만들기' 버튼으로 다시 요청하라고 안내하세요.\n");
    }
    if let Some((r, body)) = skill {
        s.push_str(&format!("\n[스킬: {}]\n{}\n", r.name, body));
    }
    s
}

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
         - 초안은 당신이 볼 수 없는 파일·검색·실행 결과에 근거했을 수 있습니다. 확인할 수 없다는 이유만으로 내용을 지우거나 '모름'으로 바꾸지 말고, 분명히 틀린 부분만 고치세요.\n\n[요청]\n{request}\n\n[초안]\n{draft}"
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
pub fn send(app: &AppHandle, st: &std::sync::Arc<AppState>, conversation_id: Option<String>, text: String, choice: Option<EngineId>, work: bool, build: bool, workspace: Option<String>, council: bool) -> Result<SendResult, String> {
    let text = text.trim().to_string();
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
    let user_message_id = store::insert_message(&st.db, &conv, "user", &text, Some(&json!({ "work": work, "build": build, "askedCouncil": council, "engine": choice.map(|c| c.as_str()) }))).map_err(|e| e.to_string())?;
    let task_id = new_id();
    let cancel = CancellationToken::new();
    st.tasks.lock().unwrap_or_else(|p| p.into_inner()).insert(task_id.clone(), cancel.clone());
    st.steers.lock().unwrap_or_else(|p| p.into_inner()).insert(task_id.clone(), Steer::default());
    let (app2, st2, conv2, task2) = (app.clone(), st.clone(), conv.clone(), task_id.clone());
    tauri::async_runtime::spawn(async move {
        run_task(&app2, &st2, &conv2, &task2, &text, choice, work, build, council, cancel).await;
        st2.tasks.lock().unwrap_or_else(|p| p.into_inner()).remove(&task2);
        st2.steers.lock().unwrap_or_else(|p| p.into_inner()).remove(&task2);
        emit(&app2, "task:done", json!({ "taskId": task2, "conversationId": conv2 }));
        refresh_memory(&app2, &st2, &conv2).await;
    });
    Ok(SendResult { conversation_id: conv, task_id, user_message_id })
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

fn spec_effort(p: &Plan) -> Option<String> {
    p.budget.as_ref().map(|b| b.effort.to_string())
}

/// 이 엔진에 줄 모델: 요청 글에서 고른 모델 > (Claude Code) JEV 단계의 모델 > 설정의 모델. None이면 엔진 기본값.
fn model_for(p: &Plan, engine: EngineId, s: &Settings) -> Option<String> {
    if let Some((_, m)) = p.models.iter().find(|(e, _)| *e == engine) {
        return Some(m.clone());
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

fn agent_slot(task: &str, key: &str) -> String {
    format!("{task}/{key}")
}

/// 에이전트 하나만 멈춘다. 없거나 이미 끝났으면 false.
pub fn cancel_agent(st: &AppState, task: &str, key: &str) -> bool {
    match st.agents.lock().unwrap_or_else(|p| p.into_inner()).get(&agent_slot(task, key)) {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
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
async fn run_engine_as(app: &AppHandle, st: &AppState, conv: &str, task: &str, mid: &str, role: &'static str, spec: RunSpec, timeout: Duration, cancel: &CancellationToken, stream: bool, key: &str) -> Outcome {
    emit(app, "task:agent", json!({ "taskId": task, "conversationId": conv, "key": key, "engine": spec.engine, "label": spec.engine.label(), "role": role, "model": spec.model, "effort": spec.effort, "work": spec.work, "state": "run", "at": crate::db::now() }));
    if stream {
        emit(app, "task:phase", json!({ "taskId": task, "conversationId": conv, "messageId": mid, "phase": role, "engine": spec.engine, "label": spec.engine.label(), "effort": spec.effort, "model": spec.model }));
    } else {
        emit(app, "task:debate", json!({ "taskId": task, "conversationId": conv, "engine": spec.engine, "role": role, "state": "run" }));
    }
    // 에이전트 하나만 멈출 수 있게 작업 토큰의 자식 토큰으로 돌린다(작업을 멈추면 같이 멈춘다).
    let own = cancel.child_token();
    let slot = agent_slot(task, key);
    st.agents.lock().unwrap_or_else(|p| p.into_inner()).insert(slot.clone(), own.clone());
    let mut o = engines::run(&st.http, &spec, timeout, &own, |ev| match ev {
        Event::Text(t) if stream => emit(app, "task:delta", json!({ "taskId": task, "conversationId": conv, "messageId": mid, "phase": role, "delta": t })),
        // 여러 AI가 동시에 쓸 때는 실행(key)별로 흘려 각 창이 따로 보여 준다(회의 창).
        Event::Text(t) => emit(app, "task:agent-delta", json!({ "taskId": task, "conversationId": conv, "key": key, "delta": t })),
        Event::Activity(a) => {
            emit(app, "task:agent-activity", json!({ "taskId": task, "conversationId": conv, "key": key, "text": a }));
            if stream {
                emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": role, "text": a }));
            }
        }
        _ => {}
    })
    .await;
    st.agents.lock().unwrap_or_else(|p| p.into_inner()).remove(&slot);
    let stopped = own.is_cancelled() && !cancel.is_cancelled();
    if stopped {
        o.ok = false;
        o.error = Some(AGENT_STOPPED.into());
    }
    let done = o.ok && !o.text.trim().is_empty();
    emit(app, "task:agent", json!({ "taskId": task, "conversationId": conv, "key": key, "engine": spec.engine, "label": spec.engine.label(), "role": role, "state": if done { "ok" } else { "fail" }, "stopped": stopped, "ms": o.latency_ms, "error": o.error, "chars": o.text.chars().count() }));
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
async fn run_task(app: &AppHandle, st: &AppState, conv: &str, task: &str, text: &str, choice: Option<EngineId>, work_req: bool, build: bool, council_req: bool, cancel: CancellationToken) {
    let mid = new_id();
    let s = st.settings();
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
    let want_judge = s.jev_enabled && !(choice.is_some() && s.review != ReviewMode::Auto);
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
        let (jr, fr) = tokio::join!(
            async {
                if want_judge {
                    Some(judge(&st.http, key, text).await)
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
        );
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
        let c = checks.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let (false, Some(o)) = (c.is_empty(), data.as_object_mut()) {
            o.insert("jevChecks".into(), json!(c));
        }
        let _ = store::insert_message_as(&st.db, &mid, conv, "assistant", &content, Some(&data));
        emit(app, "message:created", json!({ "conversationId": conv, "messageId": mid, "taskId": task }));
    };
    let mut p = match plan_with(text, choice, work_req, &st.installed(), &catalog, s.review, judgment.as_ref(), s.agents) {
        Ok(p) => p,
        Err(e) => return finish(format!("⚠ {e}"), json!({ "error": e })),
    };
    if let Some(e) = &jev_error {
        p.reasons.insert(0, format!("JEV를 쓰지 못해 규칙으로 정했습니다: {e}"));
    }
    if let Some(n) = memory_note {
        p.reasons.push(n);
    }
    if build {
        // 끝까지 완성해야 하므로 가장 깊은 노력 단계로 돈다(요청 글에서 고른 모델은 그대로 따른다).
        p.budget = budget_for("deep");
        p.reasons.push("만들기: 승인 없이 이 폴더 안에서 끝까지 완성하도록 노력 단계를 '깊게'로 정했습니다".into());
    }
    // 회의: 버튼 또는 요청 글("서로 대화하며", "회의해서")로 켠다. 동시 에이전트·토론·검토를 대신한다.
    if !p.work && (council_req || intent::requested_council(text)) {
        let want = intent::requested_agents(text).unwrap_or(if s.agents >= 2 { s.agents as usize } else { 3 });
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
    let skill_dirs: Vec<String> = p.skill.as_ref().and_then(|s| s.0.dir.clone()).into_iter().collect();
    // 세션 메모리: JEV가 걸렀으면 남긴 것만, 아니면 요약 + 최근 대화를 붙인다.
    let ctx = |seen: Option<&str>, prompt: &str| memory::context(summary.as_deref(), upto.as_deref(), &history, seen, prompt, selection.as_ref());
    let system = if build { format!("{}{BUILD_SYSTEM}", system_prompt(true, &ws, p.skill.as_ref())) } else { system_prompt(p.work, &ws, p.skill.as_ref()) };
    let plugin_dirs = catalog.enabled_plugin_dirs();
    let timeout = Duration::from_secs(if build { 1800 } else if p.work { 1200 } else { 600 });
    let mut steps: Vec<Step> = vec![];
    // 병렬 작업: 작업(만들기)을 하위 작업으로 나눠 에이전트마다 폴더 사본에서 동시에 하고 → 바뀐 파일을 합치고 → 통합 AI가 마무리한다.
    if p.work {
        if let Some(team) = p.team.clone() {
            let tw = TeamWork { app, st, conv, task, mid: &mid, workspace: &workspace, settings: &s, system: &system, timeout, cancel: &cancel, extra_dirs: &skill_dirs, plugin_dirs: &plugin_dirs };
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
    for engine in std::iter::once(p.primary).chain(p.fallbacks.iter().copied()) {
        if cancel.is_cancelled() {
            break;
        }
        // Claude Code·Antigravity는 같은 대화의 세션을 이어 쓰고, 나머지는 최근 대화를 요청에 붙인다.
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
        };
        let o = run_engine(app, st, conv, task, &mid, "draft", spec, timeout, &cancel).await;
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
            draft = Some((engine, o));
            break;
        }
        if stopped {
            break;
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
    }
    let Some((drafted_by, d)) = draft else {
        let why = if cancel.is_cancelled() { "중지했습니다.".to_string() } else { steps.last().and_then(|s| s.error.clone()).unwrap_or_else(|| "답을 받지 못했습니다".into()) };
        return finish(format!("⚠ {why}"), message_data(&steps, &p, json!({ "error": why })));
    };

    // 검토: 다른 AI가 초안을 다듬는다(실패하면 초안을 그대로 쓴다).
    if let Some(rev) = p.reviewer.filter(|_| !cancel.is_cancelled()) {
        // 초안이 이미 완성됐다고 JEV가 확신하면 검토 AI를 부르지 않는다.
        if let Some(key) = &jev_key {
            emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "review", "text": "JEV가 초안 검토 생략 여부를 판단하는 중" }));
            if let Ok((ready, ms, cost)) = draft_ready(&st.http, key, &p.prompt, &d.text).await {
                jev_note("draft", ready, ms, cost);
                if ready >= JEV_DRAFT_OK {
                    let mut p2 = p.clone();
                    p2.reasons.push(format!(
                        "JEV: 초안의 완성도가 충분하다고 판단(신뢰도 {pct}%)하여 {rev}의 검토를 생략합니다.",
                        rev = rev.label(),
                        pct = pct(ready)
                    ));
                    finish(d.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": drafted_by, "skippedReview": rev })));
                    mark_seen(st, conv, drafted_by);
                    return;
                }
            }
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
        };
        let r = run_engine(app, st, conv, task, &mid, "review", spec, timeout, &cancel).await;
        steps.push(step(rev, "review", &r, &spec_effort(&p), spec_model(&p, rev, &s)));
        if r.ok && !r.text.trim().is_empty() && !review_is_degraded(&d.text, &r.text) {
            mark_seen(st, conv, drafted_by);
            return finish(r.text.clone(), message_data(&steps, &p, json!({ "draft": d.text, "draftedBy": drafted_by, "reviewedBy": rev })));
        }
        if r.ok && review_is_degraded(&d.text, &r.text) {
            let mut p2 = p.clone();
            p2.reasons.push(format!("{}의 검토가 초안보다 지나치게 짧거나 내용을 지워 초안을 그대로 씁니다", rev.label()));
            finish(d.text.clone(), message_data(&steps, &p2, json!({ "draftedBy": drafted_by, "rejectedReview": r.text })));
            mark_seen(st, conv, drafted_by);
            return;
        }
    }
    finish(d.text.clone(), message_data(&steps, &p, json!({ "draftedBy": drafted_by })));
    mark_seen(st, conv, drafted_by);
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
}

/// 나누는 AI 한 번에 기다리는 최대 시간
const SPLIT_TIMEOUT: Duration = Duration::from_secs(300);

fn remove_dir_quietly(dir: PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

/// 병렬 작업. Some이면 끝난 답(내용, 기록), None이면 나눌 수 없어 한 에이전트로 돌아간다(이유는 p에 남기고 p.team을 지운다).
async fn run_team_work(tw: &TeamWork<'_>, p: &mut Plan, team: &Team, ctx: &(dyn Fn(Option<&str>, &str) -> String + Sync), steps: &mut Vec<Step>) -> Option<(String, Value)> {
    let (app, st, conv, task, mid, s) = (tw.app, tw.st, tw.conv, tw.task, tw.mid, tw.settings);
    let act = |text: String| emit(app, "task:activity", json!({ "taskId": task, "conversationId": conv, "phase": "agents", "text": text }));
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
    act("작업 분할을 위해 프로젝트 폴더 검사 중".to_string());
    let base = {
        let r = root.clone();
        match tokio::task::spawn_blocking(move || teamwork::snapshot(&r)).await.map_err(|e| e.to_string()).and_then(|r| r) {
            Ok(b) => b,
            Err(e) => {
                no_copy(p, e);
                return None;
            }
        }
    };

    // 2) 나누기: 가장 강한 AI가 대화 모드로 하위 작업 목록(JSON)을 만든다.
    act(format!("{who}이(가) 요청을 하위 작업으로 분할 중", who = team.synth.label()));
    let split = RunSpec {
        engine: team.synth,
        prompt: ctx(None, &teamwork::split_prompt(&p.prompt, &teamwork::listing(&base), n)),
        system: system_prompt(false, &ws, p.skill.as_ref()),
        cwd: root.clone(),
        work: false,
        resume: None,
        plugin_dirs: vec![],
        model: model_for(p, team.synth, s),
        effort: Some("medium".into()),
        max_tokens: Some(4000),
        extra_dirs: tw.extra_dirs.to_vec(),
        steer: None,
    };
    let o = run_engine_as(app, st, conv, task, mid, "plan", split, SPLIT_TIMEOUT, tw.cancel, false, "plan").await;
    steps.push(step(team.synth, "plan", &o, &Some("medium".into()), spec_model(p, team.synth, s)));
    if tw.cancel.is_cancelled() {
        return stopped(steps, p, Value::Null);
    }
    let Some(parts) = o.ok.then(|| teamwork::parse_parts(&o.text, n)).flatten() else {
        single(p, "독립된 하위 작업으로 분할할 수 없어 단일 에이전트가 전체 파일 수정을 진행합니다.".to_string());
        return None;
    };

    // 3) 에이전트마다 폴더 사본
    let tmp = st.data_dir.join("teamwork").join(task);
    let dirs: Vec<PathBuf> = (0..parts.len()).map(|i| tmp.join(format!("agent-{}", i + 1))).collect();
    let copied = {
        let (r, d, b) = (root.clone(), dirs.clone(), base.clone());
        tokio::task::spawn_blocking(move || d.iter().try_for_each(|x| teamwork::copy_tree(&r, x, &b))).await.map_err(|e| e.to_string()).and_then(|r| r)
    };
    if let Err(e) = copied {
        tokio::task::spawn_blocking({
            let t = tmp.clone();
            move || remove_dir_quietly(t)
        });
        no_copy(p, e);
        return None;
    }

    // 4) 하위 작업을 동시에
    let agents: Vec<&TeamAgent> = team.agents.iter().take(parts.len()).collect();
    let who = |a: &TeamAgent| match &a.model {
        Some(m) => format!("{} · {m}", a.engine.label()),
        None => a.engine.label().to_string(),
    };
    emit(app, "task:parts", json!({ "taskId": task, "conversationId": conv, "parts": parts.iter().enumerate().map(|(i, x)| json!({ "key": format!("agent:{i}"), "title": x.title, "files": x.files })).collect::<Vec<_>>() }));
    act(format!("{n}개의 에이전트가 개별 작업 공간에서 동시 작업 중", n = parts.len()));
    let effort = p.budget.as_ref().map(|b| b.effort.to_string());
    let max_tokens = p.budget.as_ref().map(|b| b.max_tokens);
    let outs: Vec<Outcome> = {
        let pr: &Plan = p;
        let runs = parts.iter().enumerate().map(|(i, part)| {
            let a = agents[i];
            let spec = RunSpec {
                engine: a.engine,
                prompt: ctx(None, &teamwork::part_prompt(&pr.prompt, part)),
                system: format!("{}{}", tw.system, teamwork::part_system(i, &parts)),
                cwd: dirs[i].clone(),
                work: true,
                resume: None,
                plugin_dirs: if a.engine == EngineId::Claude { tw.plugin_dirs.to_vec() } else { vec![] },
                model: a.model.clone().or_else(|| model_for(pr, a.engine, s)),
                effort: effort.clone(),
                max_tokens,
                extra_dirs: tw.extra_dirs.to_vec(),
                steer: None,
            };
            let key = format!("agent:{i}");
            async move { run_engine_as(app, st, conv, task, mid, "agent", spec, tw.timeout, tw.cancel, false, &key).await }
        });
        futures_util::future::join_all(runs).await
    };
    let good = |o: &Outcome| o.ok && !o.text.trim().is_empty();
    for (a, o) in agents.iter().zip(&outs) {
        steps.push(step(a.engine, "agent", o, &spec_effort(p), a.model.clone().or_else(|| spec_model(p, a.engine, s))));
    }

    // 5) 바뀐 파일 찾기(실패·중지한 에이전트는 합치지 않는다)
    let changes: Vec<Option<teamwork::Changes>> = {
        let (d, b, ok): (Vec<PathBuf>, teamwork::Snapshot, Vec<bool>) = (dirs.clone(), base.clone(), outs.iter().map(good).collect());
        tokio::task::spawn_blocking(move || d.iter().zip(ok).map(|(x, ok)| if ok { teamwork::snapshot(x).ok().map(|a| teamwork::diff(&b, &a)) } else { None }).collect()).await.unwrap_or_default()
    };
    let changes: Vec<Option<teamwork::Changes>> = (0..parts.len()).map(|i| changes.get(i).cloned().flatten()).collect();
    let team_json = |synth: Option<EngineId>, conflicts: &[String]| {
        json!({
            "agents": agents.iter().zip(&outs).enumerate().map(|(i, (a, o))| json!({ "engine": a.engine, "model": a.model, "ok": good(o), "text": o.text, "title": parts[i].title, "files": changes[i].as_ref().map(|c| c.paths()).unwrap_or_default() })).collect::<Vec<_>>(),
            "synth": synth,
            "work": true,
            "conflicts": conflicts,
        })
    };
    if tw.cancel.is_cancelled() {
        tokio::task::spawn_blocking(move || remove_dir_quietly(tmp));
        return stopped(steps, p, team_json(None, &[]));
    }

    // 6) 합치기: 한 에이전트만 바꾼 파일은 그대로, 충돌 파일은 첫 판을 쓰고 통합 AI가 나머지를 합친다.
    let merge = teamwork::plan_merge(&changes);
    let conflicts: Vec<String> = merge.conflicts.keys().cloned().collect();
    if !merge.apply.is_empty() {
        act(format!("변경된 {files}개 파일 병합 중", files = merge.apply.len()));
    }
    let errors: Vec<String> = {
        let (r, d, apply) = (root.clone(), dirs.clone(), merge.apply.clone());
        tokio::task::spawn_blocking(move || apply.iter().filter_map(|(path, i, del)| teamwork::apply_one(&d[*i], &r, path, *del).err()).collect()).await.unwrap_or_default()
    };
    for e in errors.iter().take(5) {
        act(format!("⚠ {e}"));
    }
    let results: Vec<teamwork::PartResult> = agents
        .iter()
        .zip(&outs)
        .enumerate()
        .map(|(i, (a, o))| teamwork::PartResult { title: &parts[i].title, who: who(a), ok: good(o), summary: &o.text, files: changes[i].as_ref().map(|c| c.paths()).unwrap_or_default() })
        .collect();
    let conflict_views: Vec<teamwork::ConflictView> = merge
        .conflicts
        .iter()
        .map(|(path, idx)| teamwork::ConflictView {
            path: path.clone(),
            kept: format!("{} · {}", parts[idx[0]].title, who(agents[idx[0]])),
            others: idx[1..]
                .iter()
                .map(|j| {
                    let deleted = changes[*j].as_ref().is_some_and(|c| c.deleted.contains(path));
                    let text = if deleted { Some("(deleted)".to_string()) } else { teamwork::read_version(&dirs[*j], path) };
                    (format!("{} · {}", parts[*j].title, who(agents[*j])), teamwork::version_path(&dirs[*j], path), text)
                })
                .collect(),
        })
        .collect();
    if results.iter().all(|r| !r.ok) {
        tokio::task::spawn_blocking(move || remove_dir_quietly(tmp));
        let why = outs.iter().find_map(|o| o.error.clone()).unwrap_or_else(|| "답을 받지 못했습니다".into());
        return Some((format!("⚠ {why}"), message_data(steps, p, json!({ "error": why, "team": team_json(None, &conflicts) }))));
    }

    // 7) 통합: 원래 폴더에서 맞물림·충돌·실패한 부분을 마무리하고 하나의 결과 요약을 쓴다.
    let mut extra = tw.extra_dirs.to_vec();
    extra.push(tmp.to_string_lossy().to_string());
    let spec = RunSpec {
        engine: team.synth,
        prompt: ctx(None, &teamwork::integrate_prompt(&p.prompt, &results, &conflict_views)),
        system: format!("{}{}", tw.system, teamwork::INTEGRATE_SYSTEM),
        cwd: root.clone(),
        work: true,
        resume: None,
        plugin_dirs: if team.synth == EngineId::Claude { tw.plugin_dirs.to_vec() } else { vec![] },
        model: model_for(p, team.synth, s),
        effort: effort.clone(),
        max_tokens,
        extra_dirs: extra,
        steer: None,
    };
    let j = run_engine(app, st, conv, task, mid, "synth", spec, tw.timeout, tw.cancel).await;
    steps.push(step(team.synth, "synth", &j, &spec_effort(p), spec_model(p, team.synth, s)));
    tokio::task::spawn_blocking(move || remove_dir_quietly(tmp));
    let first_ok = agents.iter().zip(&outs).find(|(_, o)| good(o)).map(|(a, _)| a.engine);
    if good(&j) {
        return Some((j.text.clone(), message_data(steps, p, json!({ "synthBy": team.synth, "draftedBy": first_ok, "team": team_json(Some(team.synth), &conflicts) }))));
    }
    // 통합에 실패하면 합친 결과는 폴더에 남기고 각 에이전트의 요약을 이어 붙여 보여 준다.
    let joined: String = results.iter().filter(|r| r.ok).map(|r| format!("### {} · {}\n{}\n", r.title, r.who, r.summary.trim())).collect::<Vec<_>>().join("\n");
    let mut p2 = p.clone();
    p2.reasons.push(format!("통합 에이전트 {who}의 작업이 실패하여 병합된 파일은 유지되고 각 에이전트의 요약이 대신 표시됩니다.", who = team.synth.label()));
    Some((joined, message_data(steps, &p2, json!({ "draftedBy": first_ok, "team": team_json(None, &conflicts) }))))
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
        assert!(plan("이 코드 리뷰 자세히 해줘", None, false, &[EngineId::Claude], &c, ReviewMode::Auto, None).unwrap().reviewer.is_none());
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
        let j = Judgment { model: "jev".into(), task: Some("chat".into()), task_confidence: Some(0.9), difficulty: Some(0.4), difficulty_confidence: Some(0.8), stakes: None, stakes_confidence: None, fresh: None, local: None, ms: 1, cost_usd: 0.0 };
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
        let j = |task: &str, d: f64, conf: f64| Judgment { model: "jev".into(), task: Some(task.into()), task_confidence: Some(0.9), difficulty: Some(d), difficulty_confidence: Some(conf), stakes: None, stakes_confidence: None, fresh: None, local: None, ms: 1, cost_usd: 0.0 };
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
        Judgment { model: "jev".into(), task: Some(task.into()), task_confidence: Some(0.9), difficulty: Some(d), difficulty_confidence: Some(0.9), stakes: Some(stakes), stakes_confidence: Some(0.9), fresh: None, local: None, ms: 1, cost_usd: 0.0 }
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

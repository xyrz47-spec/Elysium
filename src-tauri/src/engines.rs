//! AI 엔진: CLI(Claude Code·Codex·Antigravity)와 API(Claude API·OpenAI·Gemini API·NVIDIA, `apis.rs`).
//! CLI를 찾아 실행하고, 출력 스트림을 글자·활동·결과로 바꾼다.
//!
//! 안전 원칙
//! - 요청 본문은 명령줄 인자가 아니라 표준 입력으로 넘긴다(셸 해석·인자 주입 없음).
//! - 대화 모드는 읽기 전용 도구만 허용한다. 파일을 고치는 작업 모드는 사용자가 승인한 뒤에만 켠다.
//! - CLI는 작업 폴더(current_dir)에서 실행한다.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum EngineId {
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "antigravity")]
    Antigravity,
    #[serde(rename = "claude-api")]
    ClaudeApi,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "gemini-api")]
    GeminiApi,
    #[serde(rename = "nvidia")]
    Nvidia,
}

pub const ALL: [EngineId; 7] = [EngineId::Claude, EngineId::Codex, EngineId::Antigravity, EngineId::ClaudeApi, EngineId::OpenAi, EngineId::GeminiApi, EngineId::Nvidia];

impl EngineId {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineId::Claude => "claude",
            EngineId::Codex => "codex",
            EngineId::Antigravity => "antigravity",
            EngineId::ClaudeApi => "claude-api",
            EngineId::OpenAi => "openai",
            EngineId::GeminiApi => "gemini-api",
            EngineId::Nvidia => "nvidia",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            EngineId::Claude => "Claude Code",
            EngineId::Codex => "Codex",
            EngineId::Antigravity => "Antigravity",
            EngineId::ClaudeApi => "Claude API",
            EngineId::OpenAi => "OpenAI API",
            EngineId::GeminiApi => "Gemini API",
            EngineId::Nvidia => "NVIDIA API",
        }
    }
    /// API 키로 쓰는 엔진(파일을 다루지 못함)
    pub fn is_api(self) -> bool {
        matches!(self, EngineId::ClaudeApi | EngineId::OpenAi | EngineId::GeminiApi | EngineId::Nvidia)
    }
    /// 만든 회사. 검토는 되도록 다른 회사의 AI에게 맡긴다.
    pub fn vendor(self) -> &'static str {
        match self {
            EngineId::Claude | EngineId::ClaudeApi => "anthropic",
            EngineId::Codex | EngineId::OpenAi => "openai",
            EngineId::Antigravity | EngineId::GeminiApi => "google",
            EngineId::Nvidia => "nvidia",
        }
    }
    /// 예전 기록의 "gemini"(Gemini CLI)는 Antigravity로 읽는다.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gemini" | "agy" => Some(EngineId::Antigravity),
            _ => ALL.into_iter().find(|e| e.as_str() == s),
        }
    }
    /// 이 PC에서 실제 출력으로 확인한 어댑터인지(Claude Code 2026-10-17, Antigravity agy 1.4.10 2026-10-30).
    pub fn verified(self) -> bool {
        matches!(self, EngineId::Claude | EngineId::Antigravity)
    }
    /// 자기 대화 세션을 이어 쓰는 CLI(Claude Code `--resume`, Antigravity `--conversation`).
    pub fn resumable(self) -> bool {
        matches!(self, EngineId::Claude | EngineId::Antigravity)
    }
    fn exe_name(self) -> &'static str {
        match self {
            EngineId::Antigravity => "agy",
            e => e.as_str(),
        }
    }
    fn install_hint(self) -> &'static str {
        match self {
            EngineId::Claude => "https://claude.com/claude-code 에서 설치 후 터미널에서 claude 로 로그인",
            EngineId::Codex => "npm i -g @openai/codex 설치 후 codex 로 로그인",
            EngineId::Antigravity => "Antigravity 설치 후 터미널에서 agy 로 로그인",
            e => crate::apis::api(e).map(|a| a.key_hint).unwrap_or(""),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub id: EngineId,
    pub label: &'static str,
    /// "cli" | "api"
    pub kind: &'static str,
    /// CLI는 설치됨, API는 키가 있음
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub verified: bool,
    pub install_hint: &'static str,
}

const NO_WINDOW: u32 = 0x0800_0000;

pub(crate) fn command(program: &Path) -> tokio::process::Command {
    // npm으로 설치한 CLI는 .cmd 래퍼라 cmd를 거쳐야 한다. 인자는 고정된 플래그뿐이고 요청 본문은 표준 입력으로 보낸다.
    let is_cmd = program.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut c = if is_cmd {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/D").arg("/C").arg(program);
        c
    } else {
        tokio::process::Command::new(program)
    };
    #[cfg(windows)]
    c.creation_flags(NO_WINDOW);
    c.kill_on_drop(true);
    c
}

/// PATH에서 실행 파일을 찾는다(Windows는 .exe를 .cmd보다 먼저).
pub fn locate(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) { &["exe", "cmd", "bat"] } else { &[""] };
    let mut extra = Vec::new();
    if let Ok(home) = std::env::var("USERPROFILE") {
        // Claude Code 기본 설치 위치(PATH에 없을 때 대비)
        extra.push(PathBuf::from(&home).join(".local").join("bin"));
        extra.push(PathBuf::from(&home).join("AppData").join("Roaming").join("npm"));
    }
    for ext in exts {
        for dir in std::env::split_paths(&path).chain(extra.iter().cloned()) {
            let p = if ext.is_empty() { dir.join(name) } else { dir.join(format!("{name}.{ext}")) };
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// models: 엔진별로 설정에서 고른 모델(없으면 기본값)
pub async fn detect(models: &std::collections::BTreeMap<String, String>) -> Vec<EngineInfo> {
    let mut out = Vec::new();
    for id in ALL {
        if let Some(a) = crate::apis::api(id) {
            out.push(EngineInfo {
                id,
                label: id.label(),
                kind: "api",
                installed: crate::secrets::get(a.secret).is_some(),
                path: None,
                version: Some(models.get(id.as_str()).cloned().unwrap_or_else(|| a.default_model.to_string())),
                verified: false,
                install_hint: id.install_hint(),
            });
            continue;
        }
        let path = locate(id.exe_name());
        let mut version = None;
        if let Some(p) = &path {
            let mut c = command(p);
            c.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            if let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(8), c.output()).await {
                version = String::from_utf8_lossy(&o.stdout).lines().next().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
            }
        }
        out.push(EngineInfo {
            id,
            label: id.label(),
            kind: "cli",
            installed: path.is_some(),
            path: path.map(|p| p.to_string_lossy().to_string()),
            version,
            verified: id.verified(),
            install_hint: id.install_hint(),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// 실행

/// 작업 중에 사용자가 보낸 메시지를 모아 두는 칸. None이면 지금 AI는 메시지를 받지 않는다(API 작업 모드만 받는다).
pub type Steer = std::sync::Arc<std::sync::Mutex<Option<Vec<String>>>>;

#[derive(Debug, Clone)]
pub struct RunSpec {
    pub engine: EngineId,
    pub prompt: String,
    /// 엘리시움 기본 지시 + 스킬 지시문
    pub system: String,
    pub cwd: PathBuf,
    /// true면 파일 편집을 허용한다(승인 후에만).
    pub work: bool,
    pub resume: Option<String>,
    pub plugin_dirs: Vec<String>,
    /// 모델: API 엔진은 설정의 모델, Claude Code는 JEV가 고른 별칭(haiku·sonnet). None이면 기본 모델.
    pub model: Option<String>,
    /// 노력치(low·medium·high). None이면 엔진 기본값.
    pub effort: Option<String>,
    /// API 엔진의 최대 출력 토큰. None이면 8192.
    pub max_tokens: Option<u32>,
    /// 작업 폴더 밖에서 읽기를 허용할 폴더(스킬의 참고 파일 등). Claude Code·Antigravity `--add-dir`.
    pub extra_dirs: Vec<String>,
    /// 작업 중 메시지 칸(API 작업 모드만 읽는다)
    pub steer: Option<Steer>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Text(String),
    /// 도구 사용 등 진행 상황(사람이 읽는 짧은 설명)
    Activity(String),
    Session(String),
    Done(Finish),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finish {
    /// 최종 답(있으면 스트림으로 받은 글보다 우선)
    pub result: Option<String>,
    pub ok: bool,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub text: String,
    pub session_id: Option<String>,
    pub activities: Vec<String>,
    pub ok: bool,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub latency_ms: u64,
}

const CHAT_TOOLS: &str = "Read,Grep,Glob,WebFetch,WebSearch";
/// 작업 모드는 PowerShell 명령까지 허용한다(Bash는 쓰지 않는다). Windows에서는 샌드박스가 없어 사용자 권한으로 돈다.
const WORK_TOOLS: &str = "Read,Grep,Glob,Edit,Write,PowerShell,WebFetch,WebSearch";

/// CLI 인자. 요청 본문은 들어가지 않는다(표준 입력으로 보냄).
pub fn args(spec: &RunSpec) -> Vec<String> {
    let s = |x: &str| x.to_string();
    match spec.engine {
        EngineId::Claude => {
            let tools = if spec.work { WORK_TOOLS } else { CHAT_TOOLS };
            // 사용자 전역 설정(훅·플러그인·MCP)은 불러오지 않는다: 매 요청 수만 토큰이 늘고(실측 약 5만 → 8천) 예기치 않은 명령이 돈다.
            let mut a = vec![
                s("-p"),
                s("--output-format"), s("stream-json"),
                s("--verbose"),
                s("--include-partial-messages"),
                s("--setting-sources"), s("project"),
                s("--strict-mcp-config"),
                s("--tools"), s(tools),
                s("--allowedTools"), s(tools),
                s("--permission-mode"), s(if spec.work { "acceptEdits" } else { "default" }),
            ];
            if !spec.system.is_empty() {
                a.push(s("--append-system-prompt"));
                a.push(spec.system.clone());
            }
            if let Some(m) = &spec.model {
                a.push(s("--model"));
                a.push(m.clone());
            }
            if let Some(e) = &spec.effort {
                a.push(s("--effort"));
                a.push(e.clone());
            }
            if let Some(r) = &spec.resume {
                a.push(s("--resume"));
                a.push(r.clone());
            }
            for d in &spec.plugin_dirs {
                a.push(s("--plugin-dir"));
                a.push(d.clone());
            }
            for d in &spec.extra_dirs {
                a.push(s("--add-dir"));
                a.push(d.clone());
            }
            a
        }
        EngineId::Codex => {
            let mut a = vec![s("exec"), s("--json"), s("--skip-git-repo-check"), s("-s"), s(if spec.work { "workspace-write" } else { "read-only" })];
            if let Some(m) = &spec.model {
                a.push(s("-m"));
                a.push(m.clone());
            }
            if let Some(e) = &spec.effort {
                a.push(s("-c"));
                a.push(format!("model_reasoning_effort=\"{e}\""));
            }
            a.push(s("-"));
            a
        }
        EngineId::Antigravity => {
            // 요청은 표준 입력의 stream-json 한 줄로 보낸다(-p는 인자로만 받음). 2026-10-30 agy 1.4.10으로 확인.
            // 머리 없는 실행에서 기본 모드는 작업 폴더 쓰기를 허용하므로, 대화 모드는 쓰기를 규칙으로 막는다. 명령 실행은 언제나 막는다.
            let deny = if spec.work { r#"{"permissions":{"deny":["command(*)"]}}"# } else { r#"{"permissions":{"deny":["write_file(*)","command(*)"]}}"# };
            let mut a = vec![s("--input-format"), s("stream-json"), s("--output-format"), s("stream-json"), s("--disable-slash-commands"), s("--settings"), s(deny)];
            if spec.work {
                a.push(s("--mode"));
                a.push(s("accept-edits"));
            }
            if let Some(m) = &spec.model {
                a.push(s("--model"));
                a.push(m.clone());
            }
            if let Some(e) = &spec.effort {
                a.push(s("--effort"));
                a.push(e.clone());
            }
            if let Some(r) = &spec.resume {
                a.push(s("--conversation"));
                a.push(r.clone());
            }
            for d in &spec.extra_dirs {
                a.push(s("--add-dir"));
                a.push(d.clone());
            }
            a
        }
        // API 엔진은 프로세스를 띄우지 않는다.
        _ => vec![],
    }
}

/// 표준 입력으로 보낼 본문. Claude는 지시문을 따로 받으므로 요청만, 나머지는 지시문을 앞에 붙인다.
pub fn stdin_text(spec: &RunSpec) -> String {
    let body = match spec.engine {
        EngineId::Claude => return spec.prompt.clone(),
        _ if spec.system.is_empty() => spec.prompt.clone(),
        _ => format!("{}\n\n---\n\n{}", spec.system, spec.prompt),
    };
    match spec.engine {
        // agy --input-format stream-json: 한 줄에 메시지 하나
        EngineId::Antigravity => format!("{}\n", serde_json::json!({ "event": "user", "message": { "content": body } })),
        _ => body,
    }
}

fn short(v: &Value) -> String {
    let s = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
    let s = s.replace('\n', " ");
    if s.chars().count() > 80 { format!("{}…", s.chars().take(80).collect::<String>()) } else { s }
}

fn tool_activity(name: &str, input: &Value) -> String {
    // Antigravity는 매개변수 이름이 대문자로 시작한다(Path, CommandLine …).
    const KEYS: &[&str] = &["file_path", "path", "pattern", "url", "query", "command", "AbsolutePath", "Path", "DirectoryPath", "SearchPath", "Pattern", "Url", "Query", "CommandLine"];
    let target = KEYS.iter().find_map(|k| input.get(*k)).map(short);
    match target {
        Some(t) => format!("{name} · {t}"),
        None => name.to_string(),
    }
}

/// Claude Code `--output-format stream-json` 해석기(2026-10-17 실제 출력으로 확인).
#[derive(Default)]
pub struct ClaudeParser {
    has_text: bool,
}

impl ClaudeParser {
    pub fn feed(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return vec![] };
        let mut out = vec![];
        // 하위 에이전트의 출력은 본문에 섞지 않는다.
        let top = v.get("parent_tool_use_id").is_none_or(Value::is_null);
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "init" => {
                if let Some(id) = v["session_id"].as_str() {
                    out.push(Event::Session(id.to_string()));
                }
            }
            Some("stream_event") if top => {
                let e = &v["event"];
                match e["type"].as_str() {
                    Some("content_block_start") if e["content_block"]["type"] == "text" && self.has_text => out.push(Event::Text("\n\n".into())),
                    Some("content_block_delta") if e["delta"]["type"] == "text_delta" => {
                        if let Some(t) = e["delta"]["text"].as_str() {
                            self.has_text = true;
                            out.push(Event::Text(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                for c in v["message"]["content"].as_array().into_iter().flatten() {
                    if c["type"] == "tool_use" {
                        out.push(Event::Activity(tool_activity(c["name"].as_str().unwrap_or("도구"), &c["input"])));
                    }
                }
            }
            Some("result") => {
                let ok = v["subtype"] == "success" && !v["is_error"].as_bool().unwrap_or(false);
                let u = &v["usage"];
                let input = ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].iter().filter_map(|k| u[*k].as_u64()).sum::<u64>();
                out.push(Event::Done(Finish {
                    result: v["result"].as_str().map(str::to_string),
                    ok,
                    error: if ok { None } else { Some(v["result"].as_str().or(v["subtype"].as_str()).unwrap_or("실패").to_string()) },
                    cost_usd: v["total_cost_usd"].as_f64(),
                    input_tokens: if input > 0 { Some(input) } else { None },
                    output_tokens: u["output_tokens"].as_u64(),
                }));
            }
            _ => {}
        }
        out
    }
}

/// Codex `exec --json`(과 예전 Gemini CLI `stream-json`) 해석기(공개 형식 기준, 이 PC에서는 미검증).
/// JSON이 아닌 줄은 그대로 본문으로 쓴다.
#[derive(Default)]
pub struct GenericParser {
    usage: Finish,
}

impl GenericParser {
    pub fn feed(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return if line.trim().is_empty() { vec![] } else { vec![Event::Text(format!("{line}\n"))] };
        };
        let mut out = vec![];
        let ty = v["type"].as_str().unwrap_or("");
        let item = &v["item"];
        match ty {
            // Codex
            "thread.started" => {
                if let Some(id) = v["thread_id"].as_str() {
                    out.push(Event::Session(id.into()));
                }
            }
            "item.completed" if item["type"] == "agent_message" => {
                if let Some(t) = item["text"].as_str() {
                    out.push(Event::Text(format!("{t}\n")));
                }
            }
            "item.started" if item["type"] == "command_execution" => out.push(Event::Activity(format!("명령 · {}", short(&item["command"])))),
            "item.started" | "item.completed" if item["type"] == "file_change" => out.push(Event::Activity("파일 수정".into())),
            "turn.completed" => {
                self.usage.input_tokens = v["usage"]["input_tokens"].as_u64();
                self.usage.output_tokens = v["usage"]["output_tokens"].as_u64();
            }
            "turn.failed" | "error" => {
                let msg = v["error"]["message"].as_str().or(v["message"].as_str()).unwrap_or("CLI 오류").to_string();
                out.push(Event::Done(Finish { ok: false, error: Some(msg), ..self.usage.clone() }));
            }
            // Gemini CLI
            "init" => {
                if let Some(id) = v["session_id"].as_str() {
                    out.push(Event::Session(id.into()));
                }
            }
            "message" if v["role"] == "assistant" => {
                if let Some(t) = v["content"].as_str() {
                    out.push(Event::Text(t.into()));
                }
            }
            "tool_use" => out.push(Event::Activity(tool_activity(v["tool_name"].as_str().unwrap_or("도구"), &v["parameters"]))),
            "result" => {
                let ok = v["status"].as_str().is_none_or(|s| s == "success");
                out.push(Event::Done(Finish {
                    ok,
                    error: if ok { None } else { Some(short(&v["error"])) },
                    input_tokens: v["stats"]["input_tokens"].as_u64().or(self.usage.input_tokens),
                    output_tokens: v["stats"]["output_tokens"].as_u64().or(self.usage.output_tokens),
                    ..Default::default()
                }));
            }
            _ => {
                // 예전 Codex 형식: {"msg": {"type": "agent_message", "message": "..."}}
                let m = &v["msg"];
                match m["type"].as_str() {
                    Some("agent_message") => {
                        if let Some(t) = m["message"].as_str() {
                            out.push(Event::Text(format!("{t}\n")));
                        }
                    }
                    Some("exec_command_begin") => out.push(Event::Activity(format!("명령 · {}", short(&m["command"])))),
                    _ => {}
                }
            }
        }
        out
    }

    pub fn finish(&self) -> Finish {
        Finish { ok: true, ..self.usage.clone() }
    }
}

/// Antigravity `agy --output-format stream-json` 해석기(2026-10-30 agy 1.4.10 실제 출력으로 확인).
/// `{"event":"init"}` → `{"event":"step_update"}`(글 조각·도구) … → `{"event":"result"}`.
#[derive(Default)]
pub struct AgyParser {
    /// 마지막으로 글을 낸 단계 번호(단계가 바뀌면 빈 줄로 나눈다)
    text_step: Option<u64>,
}

impl AgyParser {
    pub fn feed(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return vec![] };
        let mut out = vec![];
        match v["event"].as_str() {
            Some("init") => {
                if let Some(id) = v["conversation_id"].as_str() {
                    out.push(Event::Session(id.to_string()));
                }
            }
            Some("step_update") => {
                let s = &v["step_update"];
                match s["step_type"].as_str() {
                    Some("agent_response") => {
                        if let Some(t) = s["text_delta"].as_str().filter(|t| !t.is_empty()) {
                            let idx = s["step_index"].as_u64();
                            if self.text_step.is_some() && self.text_step != idx {
                                out.push(Event::Text("\n\n".into()));
                            }
                            self.text_step = idx;
                            out.push(Event::Text(t.to_string()));
                        }
                    }
                    Some("tool") if s["state"] == "ACTIVE" => {
                        let name = s["tool_name"].as_str().unwrap_or("도구");
                        out.push(Event::Activity(tool_activity(name, &s["tool_info"]["parameters"])));
                    }
                    _ => {}
                }
            }
            Some("result") => {
                let r = &v["result"];
                let ok = r["status"] == "SUCCESS";
                let u = &r["usage"];
                out.push(Event::Done(Finish {
                    result: r["response"].as_str().map(str::to_string),
                    ok,
                    error: if ok { None } else { Some(r["error"].as_str().or(r["status"].as_str()).unwrap_or("실패").to_string()) },
                    cost_usd: None,
                    input_tokens: u["input_tokens"].as_u64(),
                    output_tokens: u["output_tokens"].as_u64(),
                }));
            }
            _ => {}
        }
        out
    }
}

enum Parser {
    Claude(ClaudeParser),
    Agy(AgyParser),
    Generic(GenericParser),
}

impl Parser {
    fn feed(&mut self, line: &str) -> Vec<Event> {
        match self {
            Parser::Claude(p) => p.feed(line),
            Parser::Agy(p) => p.feed(line),
            Parser::Generic(p) => p.feed(line),
        }
    }
}

/// 엔진을 실행하고 끝날 때까지 이벤트를 넘긴다. 취소하면 프로세스·요청을 끝낸다.
pub async fn run(http: &reqwest::Client, spec: &RunSpec, timeout: Duration, cancel: &CancellationToken, mut on_event: impl FnMut(&Event)) -> Outcome {
    if spec.engine.is_api() {
        let model = spec.model.clone().or_else(|| crate::apis::api(spec.engine).map(|a| a.default_model.to_string())).unwrap_or_default();
        return crate::apis::run(http, spec, &model, timeout, cancel, &mut on_event).await;
    }
    let started = Instant::now();
    let mut out = Outcome::default();
    let fail = |mut o: Outcome, msg: String| {
        o.ok = false;
        o.error = Some(msg);
        o.latency_ms = started.elapsed().as_millis() as u64;
        o
    };
    let Some(program) = locate(spec.engine.exe_name()) else {
        return fail(out, format!("{}가 설치돼 있지 않습니다. {}", spec.engine.label(), spec.engine.install_hint()));
    };
    if let Err(e) = std::fs::create_dir_all(&spec.cwd) {
        return fail(out, format!("작업 폴더를 만들 수 없습니다: {e}"));
    }
    let mut cmd = command(&program);
    cmd.args(args(spec)).current_dir(&spec.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if spec.engine == EngineId::Claude && spec.work {
        // Claude Code의 PowerShell 도구는 이 환경 변수로 켠다(Windows는 단계적 배포라 꺼져 있을 수 있음).
        cmd.env("CLAUDE_CODE_USE_POWERSHELL_TOOL", "1");
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(out, format!("{} 실행 실패: {e}", spec.engine.label())),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let body = stdin_text(spec);
        tokio::spawn(async move {
            let _ = stdin.write_all(body.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    let stderr = child.stderr.take();
    let err_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_end(&mut buf).await;
        }
        let s = String::from_utf8_lossy(&buf).to_string();
        // 마지막 몇 줄만 남긴다.
        let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
        lines[lines.len().saturating_sub(4)..].join("\n")
    });
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    let mut parser = match spec.engine {
        EngineId::Claude => Parser::Claude(ClaudeParser::default()),
        EngineId::Antigravity => Parser::Agy(AgyParser::default()),
        _ => Parser::Generic(GenericParser::default()),
    };
    let mut finish: Option<Finish> = None;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut stopped: Option<String> = None;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => { stopped = Some("사용자가 중지했습니다".into()); break; }
            _ = &mut deadline => { stopped = Some(format!("{}초 안에 끝나지 않아 중지했습니다", timeout.as_secs())); break; }
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    for ev in parser.feed(&l) {
                        match &ev {
                            Event::Text(t) => out.text.push_str(t),
                            Event::Activity(a) => out.activities.push(a.clone()),
                            Event::Session(s) => out.session_id = Some(s.clone()),
                            Event::Done(f) => finish = Some(f.clone()),
                        }
                        on_event(&ev);
                    }
                }
                _ => break,
            }
        }
    }
    if stopped.is_some() {
        let _ = child.kill().await;
    }
    let status = child.wait().await.ok();
    let stderr_tail = err_task.await.unwrap_or_default();
    out.latency_ms = started.elapsed().as_millis() as u64;
    if let Some(msg) = stopped {
        return fail(out, msg);
    }
    let f = finish.unwrap_or_else(|| match &parser {
        Parser::Generic(p) => p.finish(),
        Parser::Claude(_) | Parser::Agy(_) => Finish { ok: false, error: Some("결과 없이 끝났습니다".into()), ..Default::default() },
    });
    if let Some(r) = f.result.as_ref().filter(|r| !r.trim().is_empty()) {
        out.text = r.clone();
    }
    out.cost_usd = f.cost_usd.or_else(|| crate::pricing::estimate_cost(spec.engine, spec.model.as_deref(), f.input_tokens, f.output_tokens));
    out.input_tokens = f.input_tokens;
    out.output_tokens = f.output_tokens;
    let exit_ok = status.is_some_and(|s| s.success());
    out.ok = f.ok && (exit_ok || spec.engine == EngineId::Claude && f.error.is_none());
    if !out.ok {
        let mut msg = f.error.unwrap_or_else(|| format!("{}가 오류로 끝났습니다", spec.engine.label()));
        if !stderr_tail.is_empty() {
            msg.push_str(&format!(" ({stderr_tail})"));
        }
        if msg.contains("Not logged in") || msg.contains("login") {
            msg.push_str(&format!(" — 터미널에서 {} 로 로그인하세요", spec.engine.exe_name()));
        }
        out.error = Some(msg);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(engine: EngineId, work: bool) -> RunSpec {
        RunSpec {
            engine,
            prompt: "요청 & del /q *".into(),
            system: "지시".into(),
            cwd: PathBuf::from("."),
            work,
            resume: Some("abc".into()),
            plugin_dirs: vec!["C:\\p".into()],
            model: None,
            effort: None,
            max_tokens: None,
            extra_dirs: vec!["C:\\s".into()],
            steer: None,
        }
    }

    #[test]
    fn prompt_never_goes_into_arguments() {
        for e in ALL.into_iter().filter(|e| !e.is_api()) {
            for w in [false, true] {
                let a = args(&spec(e, w));
                assert!(a.iter().all(|x| !x.contains("요청")), "{e:?}");
                assert!(stdin_text(&spec(e, w)).contains("요청 & del /q *"));
            }
        }
    }

    #[test]
    fn chat_mode_is_read_only_and_work_mode_allows_edits() {
        let chat = args(&spec(EngineId::Claude, false));
        let i = chat.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(chat[i + 1], "Read,Grep,Glob,WebFetch,WebSearch");
        assert!(!chat.iter().any(|x| x.contains("Edit") || x.contains("Bash")));
        assert!(chat.windows(2).any(|w| w[0] == "--permission-mode" && w[1] == "default"));
        let work = args(&spec(EngineId::Claude, true));
        assert!(work.iter().any(|x| x.contains("Edit")) && !work.iter().any(|x| x.contains("Bash")));
        assert!(work.iter().any(|x| x.contains("PowerShell")) && !chat.iter().any(|x| x.contains("PowerShell")), "명령은 작업 모드에서 PowerShell로만");
        assert!(work.windows(2).any(|w| w[0] == "--permission-mode" && w[1] == "acceptEdits"));
        assert!(work.windows(2).any(|w| w[0] == "--resume" && w[1] == "abc"));
        assert!(work.windows(2).any(|w| w[0] == "--plugin-dir" && w[1] == "C:\\p"));
        assert!(work.windows(2).any(|w| w[0] == "--add-dir" && w[1] == "C:\\s"), "스킬 참고 파일 폴더 읽기 허용");
        assert!(args(&spec(EngineId::Codex, false)).windows(2).any(|w| w[0] == "-s" && w[1] == "read-only"));
        assert!(args(&spec(EngineId::Codex, true)).windows(2).any(|w| w[0] == "-s" && w[1] == "workspace-write"));
        // Claude 외에는 지시문을 본문 앞에 붙인다
        assert!(stdin_text(&spec(EngineId::Codex, false)).starts_with("지시"));
        // JEV가 고른 모델·노력치
        let mut light = spec(EngineId::Claude, false);
        light.model = Some("haiku".into());
        light.effort = Some("low".into());
        let a = args(&light);
        assert!(a.windows(2).any(|w| w[0] == "--model" && w[1] == "haiku"));
        assert!(a.windows(2).any(|w| w[0] == "--effort" && w[1] == "low"));
        assert!(!args(&spec(EngineId::Claude, false)).iter().any(|x| x == "--model" || x == "--effort"), "판단이 없으면 기본값");
        let mut cx = spec(EngineId::Codex, false);
        cx.effort = Some("low".into());
        let a = args(&cx);
        assert!(a.iter().any(|x| x == "model_reasoning_effort=\"low\"") && a.last().unwrap() == "-");
        assert_eq!(stdin_text(&spec(EngineId::Claude, false)), "요청 & del /q *");
    }

    // 2026-10-17 claude 2.1.299 실제 출력에서 줄인 표본
    const CLAUDE_SAMPLE: &[&str] = &[
        r#"{"type":"system","subtype":"hook_started","session_id":"s-1"}"#,
        r#"{"type":"system","subtype":"init","cwd":"C:\\x","session_id":"s-1","tools":["Read"]}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}},"parent_tool_use_id":null}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}},"parent_tool_use_id":null}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"안녕"}},"parent_tool_use_id":null}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"C:\\a.txt"}}]},"parent_tool_use_id":null}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"하위"}},"parent_tool_use_id":"toolu_1"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}},"parent_tool_use_id":null}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"하세요"}},"parent_tool_use_id":null}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"result":"안녕하세요","session_id":"s-1","total_cost_usd":0.0357,"usage":{"input_tokens":2,"cache_creation_input_tokens":4171,"cache_read_input_tokens":3958,"output_tokens":82}}"#,
    ];

    #[test]
    fn parses_claude_stream() {
        let mut p = ClaudeParser::default();
        let evs: Vec<Event> = CLAUDE_SAMPLE.iter().flat_map(|l| p.feed(l)).collect();
        assert_eq!(evs[0], Event::Session("s-1".into()));
        let text: String = evs.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(text, "안녕\n\n하세요", "하위 에이전트 글은 빼고, 글 블록 사이는 빈 줄");
        assert!(evs.contains(&Event::Activity("Read · C:\\a.txt".into())));
        let Some(Event::Done(f)) = evs.last() else { panic!() };
        assert!(f.ok);
        assert_eq!(f.result.as_deref(), Some("안녕하세요"));
        assert_eq!(f.input_tokens, Some(8131));
        assert_eq!(f.output_tokens, Some(82));
        assert!((f.cost_usd.unwrap() - 0.0357).abs() < 1e-9);
        // 오류 결과
        let mut p = ClaudeParser::default();
        let e = p.feed(r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Not logged in"}"#);
        assert!(matches!(&e[0], Event::Done(f) if !f.ok && f.error.as_deref() == Some("Not logged in")));
        assert!(p.feed("not json").is_empty());
    }

    #[test]
    fn parses_codex_and_gemini_shapes() {
        let mut p = GenericParser::default();
        let evs: Vec<Event> = [
            r#"{"type":"thread.started","thread_id":"t-9"}"#,
            r#"{"type":"item.started","item":{"type":"command_execution","command":"ls"}}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"완료"}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":4}}"#,
            "평문 줄",
        ]
        .iter()
        .flat_map(|l| p.feed(l))
        .collect();
        assert_eq!(evs[0], Event::Session("t-9".into()));
        assert_eq!(evs[1], Event::Activity("명령 · ls".into()));
        assert_eq!(evs[2], Event::Text("완료\n".into()));
        assert_eq!(evs[3], Event::Text("평문 줄\n".into()));
        assert_eq!(p.finish().output_tokens, Some(4));
        let mut g = GenericParser::default();
        let evs: Vec<Event> = [
            r#"{"type":"init","session_id":"g-1"}"#,
            r#"{"type":"message","role":"assistant","content":"안녕","delta":true}"#,
            r#"{"type":"message","role":"user","content":"무시"}"#,
            r#"{"type":"result","status":"success","stats":{"input_tokens":3,"output_tokens":2}}"#,
        ]
        .iter()
        .flat_map(|l| g.feed(l))
        .collect();
        assert_eq!(evs[1], Event::Text("안녕".into()));
        assert!(matches!(&evs[2], Event::Done(f) if f.ok && f.output_tokens == Some(2)));
    }

    // 2026-10-30 agy 1.4.10 실제 출력에서 줄인 표본
    const AGY_SAMPLE: &[&str] = &[
        r#"{"event":"init","conversation_id":"c-1","init":{"model":"gemini-3.8-flash-low","permission_mode":"request-review"}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":0,"state":"DONE","step_type":"user_input"}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":1,"state":"DONE","step_type":"agent_response","usage":{"input_tokens":13598,"output_tokens":87}}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"Path":"C:/w/note.txt"}}}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":2,"state":"DONE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"Path":"C:/w/note.txt"}}}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":3,"state":"ACTIVE","step_type":"agent_response","text_delta":"zebra-"}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":3,"state":"ACTIVE","step_type":"agent_response","text_delta":"42"}}"#,
        r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":5,"state":"ACTIVE","step_type":"agent_response","text_delta":"끝"}}"#,
        r#"{"event":"result","result":{"conversation_id":"c-1","status":"SUCCESS","response":"zebra-42\n","num_turns":1,"usage":{"input_tokens":27375,"output_tokens":91}}}"#,
    ];

    #[test]
    fn parses_antigravity_stream_and_keeps_chat_read_only() {
        let mut p = AgyParser::default();
        let evs: Vec<Event> = AGY_SAMPLE.iter().flat_map(|l| p.feed(l)).collect();
        assert_eq!(evs[0], Event::Session("c-1".into()));
        assert_eq!(evs[1], Event::Activity("read_file · C:/w/note.txt".into()), "도구는 시작할 때 한 번");
        let text: String = evs.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect();
        assert_eq!(text, "zebra-42\n\n끝", "단계가 바뀌면 빈 줄");
        let Some(Event::Done(f)) = evs.last() else { panic!() };
        assert!(f.ok && f.result.as_deref() == Some("zebra-42\n") && f.input_tokens == Some(27375) && f.output_tokens == Some(91));
        let e = AgyParser::default().feed(r#"{"event":"result","result":{"status":"ERROR","response":"","error":"Individual quota reached."}}"#);
        assert!(matches!(&e[0], Event::Done(f) if !f.ok && f.error.as_deref() == Some("Individual quota reached.")));
        // 인자: 대화 모드는 쓰기·명령 차단, 작업 모드는 명령만 차단하고 편집 허용
        let chat = args(&spec(EngineId::Antigravity, false));
        let i = chat.iter().position(|x| x == "--settings").unwrap();
        assert!(chat[i + 1].contains("write_file(*)") && chat[i + 1].contains("command(*)"));
        assert!(!chat.iter().any(|x| x == "--mode" || x == "--dangerously-skip-permissions"));
        assert!(chat.windows(2).any(|w| w[0] == "--conversation" && w[1] == "abc"));
        assert!(chat.windows(2).any(|w| w[0] == "--add-dir" && w[1] == "C:\\s"));
        let work = args(&spec(EngineId::Antigravity, true));
        let i = work.iter().position(|x| x == "--settings").unwrap();
        assert!(!work[i + 1].contains("write_file") && work[i + 1].contains("command(*)"));
        assert!(work.windows(2).any(|w| w[0] == "--mode" && w[1] == "accept-edits"));
        // 표준 입력은 stream-json 한 줄(지시문 + 요청)
        let line = stdin_text(&spec(EngineId::Antigravity, false));
        let v: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["event"], "user");
        assert!(v["message"]["content"].as_str().unwrap().starts_with("지시") && line.ends_with('\n') && line.lines().count() == 1);
        assert_eq!(EngineId::parse("gemini"), Some(EngineId::Antigravity), "예전 기록");
        let mut cx = spec(EngineId::Codex, false);
        cx.model = Some("gpt-5".into());
        assert!(args(&cx).windows(2).any(|w| w[0] == "-m" && w[1] == "gpt-5"));
    }
}

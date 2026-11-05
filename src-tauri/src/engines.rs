//! AI 엔진: CLI(Claude Code·Codex·Antigravity·OpenCode)와 API(Claude API·OpenAI·Gemini API·NVIDIA, `apis.rs`).
//! CLI를 찾아 실행하고, 출력 스트림을 글자·활동·결과로 바꾼다.
//!
//! 안전 원칙
//! - 요청 본문은 명령줄 인자가 아니라 표준 입력으로 넘긴다(셸 해석·인자 주입 없음).
//! - 대화 모드는 읽기 전용 도구만 허용한다. 파일을 고치는 작업 모드는 사용자가 승인한 뒤에만 켠다.
//! - CLI는 작업 폴더(current_dir)에서 실행한다.

use crate::attachments::AttachedFile;
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
    #[serde(rename = "opencode")]
    OpenCode,
    #[serde(rename = "claude-api")]
    ClaudeApi,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "gemini-api")]
    GeminiApi,
    #[serde(rename = "nvidia")]
    Nvidia,
}

pub const ALL: [EngineId; 8] = [EngineId::Claude, EngineId::Codex, EngineId::Antigravity, EngineId::OpenCode, EngineId::ClaudeApi, EngineId::OpenAi, EngineId::GeminiApi, EngineId::Nvidia];

impl EngineId {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineId::Claude => "claude",
            EngineId::Codex => "codex",
            EngineId::Antigravity => "antigravity",
            EngineId::OpenCode => "opencode",
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
            EngineId::OpenCode => "OpenCode",
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
            EngineId::OpenCode => "opencode",
        }
    }
    /// 예전 기록의 "gemini"(Gemini CLI)는 Antigravity로 읽는다.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gemini" | "agy" => Some(EngineId::Antigravity),
            _ => ALL.into_iter().find(|e| e.as_str() == s),
        }
    }
    /// 이 PC에서 실제 출력으로 확인한 어댑터인지(Claude Code 2026-10-17, Antigravity agy 1.4.10 2026-10-30, OpenCode 2.2.5 2026-11-02).
    pub fn verified(self) -> bool {
        matches!(self, EngineId::Claude | EngineId::Antigravity | EngineId::OpenCode)
    }
    /// 자기 대화 세션을 이어 쓰는 CLI(Claude Code `--resume`, Antigravity `--conversation`, OpenCode `--session`).
    pub fn resumable(self) -> bool {
        matches!(self, EngineId::Claude | EngineId::Antigravity | EngineId::OpenCode)
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
            EngineId::OpenCode => "npm i -g opencode-ai 명령으로 설치하세요. 무료 모델은 로그인 없이 이용할 수 있습니다.",
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
    /// 고를 수 있는 모델(OpenCode: 무료 모델만)
    pub models: Vec<String>,
}

// ---------------------------------------------------------------------------
// OpenCode: 무료 모델만 쓴다.

/// 모델을 고르지 않았을 때 쓰는 무료 모델(공급자가 데이터를 보관·학습하지 않고 이미지도 읽는다).
pub const OPENCODE_DEFAULT: &str = "opencode/deepseek-v4.1-flash-free";
/// 공급자가 입력을 보관·학습하지 않는 무료 모델(https://opencode.ai/docs/zen, 2026-11-02 확인). 민감한 요청은 이것만 쓴다.
pub const OPENCODE_ZERO_RETENTION: &[&str] = &["opencode/deepseek-v4.1-flash-free", "opencode/glm-5.3-flash-free"];
/// 이미지 입력을 받는 무료 모델(models.dev attachment 값, 2026-11-02 확인)
pub const OPENCODE_VISION: &[&str] = &["opencode/deepseek-v4.1-flash-free", "opencode/glm-5.3-flash-free", "opencode/mimo-v2.6-flash-free", "opencode/nemotron-3.5-super-vl-free"];
/// 이미지가 있을 때 먼저 쓰는 무료 모델. 2026-11-02 빨간 이미지로 시험: 이 모델은 색을 맞혔고, 기본 모델은 "흰 화면"이라고 답한 적이 있다.
pub const OPENCODE_IMAGE_FIRST: &str = "opencode/nemotron-3.5-super-vl-free";

/// 무료 모델인지. Zen 공급자의 `-free` 모델과 Big Pickle만 인정한다(다른 공급자는 사용자 키로 과금된다).
pub fn is_free_model(m: &str) -> bool {
    m == "opencode/big-pickle" || (m.starts_with("opencode/") && m.ends_with("-free") && !m.contains(char::is_whitespace))
}

/// `opencode models` 출력에서 무료 모델만 고른다.
pub fn free_models(listing: &str) -> Vec<String> {
    let mut out: Vec<String> = listing.lines().map(str::trim).filter(|l| is_free_model(l)).map(str::to_string).collect();
    out.dedup();
    out
}

/// 실제로 줄 모델: 무료가 아니면 절대 넘기지 않고 기본 무료 모델로 바꾼다.
pub fn opencode_model(requested: Option<&str>) -> String {
    requested.filter(|m| is_free_model(m)).unwrap_or(OPENCODE_DEFAULT).to_string()
}

/// `OPENCODE_CONFIG_CONTENT`: 권한과 보조 모델을 실행마다 고정한다. 제목 생성 같은 보조 호출도 유료 모델로 가지 않게 한다.
/// 대화 모드는 편집·명령을 막고, 하위 에이전트는 다른 모델을 쓸 수 있어 언제나 막는다. 작업 폴더 밖은 넘겨받은 폴더만 읽는다.
pub fn opencode_config(spec: &RunSpec) -> String {
    let model = opencode_model(spec.model.as_deref());
    let mut external = serde_json::Map::new();
    external.insert("*".into(), Value::String("deny".into()));
    for d in &spec.extra_dirs {
        external.insert(format!("{}/**", d.replace('\\', "/")), Value::String("allow".into()));
    }
    let act = if spec.work { "allow" } else { "deny" };
    // 대화 모드는 허용 목록이다: OpenCode에는 세션·워크트리를 다루는 내장 도구도 있어 이름을 하나씩 막으면 빠진다.
    // 규칙은 나중 것이 이기고 serde_json은 키를 정렬하므로 "*"가 맨 앞에 온다.
    serde_json::json!({
        "model": model,
        "small_model": model,
        "share": "disabled",
        "autoupdate": false,
        "permission": {
            "*": if spec.work { "allow" } else { "deny" },
            "read": "allow", "glob": "allow", "grep": "allow", "list": "allow", "webfetch": "allow", "websearch": "allow",
            "edit": act, "bash": act, "shell": act,
            "task": "deny", "question": "deny", "doom_loop": "deny",
            "external_directory": external,
        },
    })
    .to_string()
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

/// 프로세스와 그 자식까지 강제로 끝낸다. .cmd 래퍼를 거치면 cmd만 죽고 실제 CLI(node)는 살아남아 토큰을 쓰고 파이프를 붙잡는다.
pub(crate) fn kill_tree_blocking(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).creation_flags(NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(not(windows))]
    let _ = pid;
}

async fn kill_tree(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = tokio::task::spawn_blocking(move || kill_tree_blocking(pid)).await;
    }
    let _ = child.kill().await;
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
                models: vec![],
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
        let mut list = vec![];
        if let (EngineId::OpenCode, Some(p)) = (id, &path) {
            let mut c = command(p);
            c.arg("models").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
            if let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(20), c.output()).await {
                list = free_models(&String::from_utf8_lossy(&o.stdout));
            }
            if list.is_empty() {
                list.push(OPENCODE_DEFAULT.to_string());
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
            models: list,
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
    /// 이번 요청에 붙은 파일
    pub attachments: Vec<AttachedFile>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Text(String),
    /// 도구 사용 등 진행 상황(사람이 읽는 짧은 설명)
    Activity(String),
    Session(String),
    /// 이번 실행에서 지금까지 쓴 토큰(실행 중에 늘어난다. 최종 값은 Done이 정한다)
    Usage { input: u64, output: u64 },
    /// 뒤에서 돌리는 셸 명령(Claude Code의 run_in_background)
    Bg(Bg),
    Done(Finish),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Bg {
    /// id: Claude가 준 작업 번호, path: 출력이 쌓이는 파일
    Started { id: String, command: String, path: PathBuf },
    /// status: completed · failed · killed · stopped
    Ended { id: String, status: String },
}

/// 백그라운드 셸 도구의 결과에서 (작업 번호, 출력 파일)을 읽는다.
/// 예: "Command running in background with ID: b1x. Output is being written to: C:\...	asks1x.output. You will be notified ..."
fn bg_started(text: &str) -> Option<(String, PathBuf)> {
    let id = text.split_once("background with ID:")?.1.trim_start();
    let id: String = id.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    let rest = text.split_once("written to:")?.1.trim_start();
    let end = rest.find(".output")? + ".output".len();
    (!id.is_empty()).then(|| (id, PathBuf::from(&rest[..end])))
}

/// 작업 알림(<task-id>…</task-id> … <status>…</status>)에서 끝난 작업을 읽는다.
fn bg_ended(line: &str) -> Vec<Bg> {
    let mut out = vec![];
    let mut rest = line;
    while let Some((_, after)) = rest.split_once("<task-id>") {
        let Some((id, tail)) = after.split_once("</task-id>") else { break };
        let near: String = tail.chars().take(400).collect();
        let status = near.split_once("<status>").and_then(|(_, s)| s.split_once("</status>")).map(|(s, _)| s.trim().to_string());
        if let Some(status) = status.filter(|s| !s.is_empty() && s != "running") {
            out.push(Bg::Ended { id: id.trim().to_string(), status });
        }
        rest = tail;
    }
    out
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
    /// 출력 없이 너무 오래 멈춰 있어 중지했다(일하던 중에 시간이 다 된 것이 아니다).
    #[serde(skip)]
    pub stalled: bool,
}

/// 작업 실행은 출력이 이만큼 없어야 멈춘 것으로 본다. 도구 한 번의 한도(긴 빌드·테스트 10분)보다 넉넉하다.
pub const WORK_IDLE: Duration = Duration::from_secs(1200);
/// 출력이 계속 나와도 실행 한 번은 이 시간을 넘기지 않는다(끝없는 반복을 막는 안전장치).
pub const RUN_CAP: Duration = Duration::from_secs(6 * 3600);

/// 이 실행의 무응답 한도. 일하는 실행은 긴 명령을 기다릴 수 있게 WORK_IDLE보다 짧게 두지 않는다.
pub(crate) fn idle_limit(spec: &RunSpec, idle: Duration) -> Duration {
    if spec.work { idle.max(WORK_IDLE) } else { idle }
}

/// 출력 없이 멈춰 중지했을 때의 오류
pub(crate) fn stalled_error(idle: Duration) -> String {
    format!("{m}분 동안 활동이 없어 실행을 중지했습니다", m = idle.as_secs().div_ceil(60))
}

/// 안전 한도에 닿아 중지했을 때의 오류
pub(crate) fn capped_error() -> String {
    format!("단일 실행 제한 시간인 {h}시간을 초과하여 실행을 중지했습니다", h = RUN_CAP.as_secs() / 3600)
}

/// 마지막으로 출력이 온 때. 무응답 한도를 잴 때 쓴다.
pub(crate) struct Pulse(std::sync::Mutex<Instant>);

impl Pulse {
    pub fn new() -> Self {
        Pulse(std::sync::Mutex::new(Instant::now()))
    }
    pub fn beat(&self) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
    }
    /// 출력 없이 `idle`이 지나면 끝난다.
    pub async fn stalled(&self, idle: Duration) {
        loop {
            let quiet = self.0.lock().unwrap_or_else(|p| p.into_inner()).elapsed();
            if quiet >= idle {
                return;
            }
            tokio::time::sleep(idle - quiet).await;
        }
    }
}

const CHAT_TOOLS: &str = "Read,Grep,Glob,WebFetch,WebSearch";
/// 작업 모드는 PowerShell 명령까지 허용한다(Bash는 쓰지 않는다). Windows에서는 샌드박스가 없어 사용자 권한으로 돈다.
const WORK_TOOLS: &str = "Read,Grep,Glob,Edit,Write,PowerShell,WebFetch,WebSearch";

/// 홈 폴더의 전역 지시문을 뺀다. 프로젝트 자신의 CLAUDE.md·규칙은 그대로 읽는다.
fn claude_settings(home: &str) -> String {
    let home = home.replace('\\', "/");
    serde_json::json!({ "claudeMdExcludes": [format!("{home}/.claude/CLAUDE.md"), format!("{home}/.claude/rules/**")] }).to_string()
}

/// 대기열을 쓰는 Claude가 직접 띄우지 못하게 막는 창 띄우기 작업. 패턴이라 모든 경우를 막지는 못한다.
const WINDOW_BLOCK: &str = "PowerShell(*runClient*),PowerShell(*runGame*),PowerShell(*frameBenchmark*),PowerShell(*clientWorldReloadTest*),PowerShell(*launcherProfileTest*),PowerShell(*gradlew*Client),PowerShell(*gradlew*Client *),PowerShell(*Start-Process*),PowerShell(start *),PowerShell(saps *),PowerShell(*Invoke-Item*),PowerShell(ii *),PowerShell(*explorer.exe*),PowerShell(*msedge*),PowerShell(*chrome.exe*),PowerShell(*firefox*),PowerShell(*electron*),PowerShell(*tauri dev*),PowerShell(*npm run tauri*),PowerShell(* --open*)";

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
            // --setting-sources project still reads ~/.claude as an ancestor project when the workspace is under the home folder.
            if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
                a.push(s("--settings"));
                a.push(claude_settings(&home));
            }
            // 창 테스트 대기열을 쓰는 실행(워커·통합)과 총괄의 직접 작업은 창을 직접 띄우지 못한다.
            if spec.work && (spec.system.contains(crate::lead::QUEUE_SYSTEM) || spec.system.contains(crate::lead::LEAD_WINDOW_SYSTEM)) {
                a.push(s("--disallowedTools"));
                a.push(s(WINDOW_BLOCK));
            }
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
        EngineId::OpenCode => {
            // --standalone: 떠 있는 백그라운드 서비스는 이 실행의 권한 설정(OPENCODE_CONFIG_CONTENT)을 읽지 않는다.
            let mut a = vec![s("run"), s("--standalone"), s("--format"), s("json"), s("--model"), opencode_model(spec.model.as_deref())];
            if let Some(r) = &spec.resume {
                a.push(s("--session"));
                a.push(r.clone());
            }
            // cmd를 거치므로 attachments::disk_name을 거친 이름만 인자로 넘긴다. 나머지는 본문의 경로 안내로 읽는다.
            for f in spec.attachments.iter().filter(|f| opencode_inlines(f)) {
                a.push(s("--file"));
                a.push(f.path.to_string_lossy().to_string());
            }
            a
        }
        // API 엔진은 프로세스를 띄우지 않는다.
        _ => vec![],
    }
}

/// OpenCode `--file`로 넘기는 파일(글·이미지). 나머지 형식은 모델이 받지 못하면 실행 자체가 실패하므로 경로만 알려 준다.
fn opencode_inlines(f: &AttachedFile) -> bool {
    f.kind == "text" || f.kind == "image"
}

/// 요청 + 첨부 안내. OpenCode가 메시지에 넣은 파일도 목록에 올려야 모델이 그 내용을 끼어든 글로 오해하지 않는다.
fn prompt_with_files(spec: &RunSpec) -> String {
    if spec.attachments.is_empty() {
        return spec.prompt.clone();
    }
    let inlined = |f: &AttachedFile| spec.engine == EngineId::OpenCode && opencode_inlines(f);
    format!("{}{}", spec.prompt, crate::attachments::refer_block(&spec.attachments, &inlined))
}

/// 표준 입력으로 보낼 본문. Claude는 지시문을 따로 받으므로 요청만, 나머지는 지시문을 앞에 붙인다.
pub fn stdin_text(spec: &RunSpec) -> String {
    let prompt = prompt_with_files(spec);
    let body = match spec.engine {
        EngineId::Claude => return prompt,
        _ if spec.system.is_empty() => prompt,
        _ => format!("{}\n\n---\n\n{}", spec.system, prompt),
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
    /// 백그라운드로 시작한 셸 도구(도구 호출 id → 명령)
    bg_tools: std::collections::HashMap<String, String>,
    has_text: bool,
    /// 사용량을 이미 더한 메시지
    counted: std::collections::HashSet<String>,
    input: u64,
    output: u64,
}

impl ClaudeParser {
    pub fn feed(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return vec![] };
        let mut out = vec![];
        if line.contains("<task-id>") {
            out.extend(bg_ended(line).into_iter().map(Event::Bg));
        }
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
                let m = &v["message"];
                if let Some(id) = m["id"].as_str().filter(|_| top && m["usage"].is_object()) {
                    if self.counted.insert(id.to_string()) {
                        let u = &m["usage"];
                        self.input += ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"].iter().filter_map(|k| u[*k].as_u64()).sum::<u64>();
                        self.output += u["output_tokens"].as_u64().unwrap_or(0);
                        out.push(Event::Usage { input: self.input, output: self.output });
                    }
                }
                for c in v["message"]["content"].as_array().into_iter().flatten() {
                    if c["type"] == "tool_use" {
                        out.push(Event::Activity(tool_activity(c["name"].as_str().unwrap_or("도구"), &c["input"])));
                        if c["input"]["run_in_background"].as_bool() == Some(true) {
                            if let (Some(id), Some(cmd)) = (c["id"].as_str(), c["input"]["command"].as_str()) {
                                self.bg_tools.insert(id.to_string(), cmd.to_string());
                            }
                        }
                    }
                }
            }
            Some("user") => {
                for c in v["message"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "tool_result") {
                    let Some(command) = c["tool_use_id"].as_str().and_then(|id| self.bg_tools.remove(id)) else { continue };
                    let text = match &c["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("
"),
                        _ => String::new(),
                    };
                    if let Some((id, path)) = bg_started(&text) {
                        out.push(Event::Bg(Bg::Started { id, command, path }));
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
                out.push(Event::Usage { input: self.usage.input_tokens.unwrap_or(0), output: self.usage.output_tokens.unwrap_or(0) });
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
    /// 끝난 단계마다의 사용량(같은 단계가 여러 번 오면 마지막 값)
    steps: std::collections::BTreeMap<u64, (u64, u64)>,
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
                if let (Some(i), true) = (s["step_index"].as_u64(), s["usage"].is_object()) {
                    let u = &s["usage"];
                    let now = (u["input_tokens"].as_u64().unwrap_or(0), u["output_tokens"].as_u64().unwrap_or(0));
                    if self.steps.insert(i, now) != Some(now) {
                        let (input, output) = self.steps.values().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
                        out.push(Event::Usage { input, output });
                    }
                }
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

/// OpenCode `run --format json` 해석기(2026-11-02 opencode 2.2.5 실제 출력으로 확인).
/// 줄마다 `step_start` · `text` · `tool_use` · `step_finish` · `error`. 마지막에 따로 오는 결과 줄은 없다.
#[derive(Default)]
pub struct OpenCodeParser {
    session: bool,
    has_text: bool,
    usage: Finish,
    finished: bool,
    /// 과금이 잡힌 비용(무료 모델만 쓰므로 0보다 크면 멈춘다)
    pub charged: f64,
}

impl OpenCodeParser {
    pub fn feed(&mut self, line: &str) -> Vec<Event> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return vec![] };
        let mut out = vec![];
        if !self.session {
            if let Some(id) = v["sessionID"].as_str() {
                self.session = true;
                out.push(Event::Session(id.to_string()));
            }
        }
        let part = &v["part"];
        match v["type"].as_str() {
            Some("text") => {
                if let Some(t) = part["text"].as_str().filter(|t| !t.trim().is_empty()) {
                    if self.has_text {
                        out.push(Event::Text("\n\n".into()));
                    }
                    self.has_text = true;
                    out.push(Event::Text(t.to_string()));
                }
            }
            Some("tool_use") => out.push(Event::Activity(tool_activity(part["tool"].as_str().unwrap_or("도구"), &part["state"]["input"]))),
            Some("step_finish") => {
                let t = &part["tokens"];
                let input = t["input"].as_u64().unwrap_or(0) + t["cache"]["read"].as_u64().unwrap_or(0) + t["cache"]["write"].as_u64().unwrap_or(0);
                self.usage.input_tokens = Some(self.usage.input_tokens.unwrap_or(0) + input);
                self.usage.output_tokens = Some(self.usage.output_tokens.unwrap_or(0) + t["output"].as_u64().unwrap_or(0));
                self.charged += part["cost"].as_f64().unwrap_or(0.0);
                self.finished = true;
                out.push(Event::Usage { input: self.usage.input_tokens.unwrap_or(0), output: self.usage.output_tokens.unwrap_or(0) });
            }
            Some("error") => {
                let e = &v["error"];
                // 무료 한도 오류는 유료 연결을 권한다. 무료 모델만 쓰므로 그 안내는 보여 주지 않는다.
                let msg = if e["type"] == "provider.free-limit" {
                    "무료 사용 한도를 모두 소진했으니 잠시 후 다시 시도하거나 다른 AI를 선택해 주세요.".to_string()
                } else {
                    e["message"].as_str().or(e["data"]["message"].as_str()).or(e["name"].as_str()).or(e["type"].as_str()).unwrap_or("OpenCode error").to_string()
                };
                out.push(Event::Done(Finish { ok: false, error: Some(msg), ..self.usage.clone() }));
            }
            _ => {}
        }
        out
    }

    pub fn finish(&self) -> Finish {
        Finish { ok: self.finished, error: (!self.finished).then(|| "결과 없이 끝났습니다".to_string()), cost_usd: Some(self.charged), ..self.usage.clone() }
    }
}

enum Parser {
    Claude(ClaudeParser),
    Agy(AgyParser),
    OpenCode(OpenCodeParser),
    Generic(GenericParser),
}

impl Parser {
    fn feed(&mut self, line: &str) -> Vec<Event> {
        match self {
            Parser::Claude(p) => p.feed(line),
            Parser::Agy(p) => p.feed(line),
            Parser::OpenCode(p) => p.feed(line),
            Parser::Generic(p) => p.feed(line),
        }
    }
}

/// 엔진을 실행하고 끝날 때까지 이벤트를 넘긴다. 취소하면 프로세스·요청을 끝낸다.
/// timeout은 무응답 한도다: 출력이 계속 나오는 동안은 기다리고, 출력 없이 이만큼 지나면 멈춘 것으로 보고 끝낸다.
pub async fn run(http: &reqwest::Client, spec: &RunSpec, timeout: Duration, cancel: &CancellationToken, mut on_event: impl FnMut(&Event)) -> Outcome {
    if spec.engine.is_api() {
        // 파일을 읽을 도구가 없는 대화 모드도 답할 수 있게 글 파일은 본문에 넣는다.
        let inlined;
        let spec = if spec.attachments.is_empty() {
            spec
        } else {
            inlined = RunSpec { prompt: format!("{}{}", spec.prompt, crate::attachments::inline_block(&spec.attachments, &crate::apis::can_send_binary(spec.engine))), ..spec.clone() };
            &inlined
        };
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
    if spec.engine == EngineId::OpenCode {
        cmd.env("OPENCODE_CONFIG_CONTENT", opencode_config(spec));
        // OpenCode는 current_dir보다 상속한 PWD를 먼저 믿는다(터미널에서 앱을 켜면 엉뚱한 폴더를 고친다).
        cmd.env("PWD", &spec.cwd);
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
        EngineId::OpenCode => Parser::OpenCode(OpenCodeParser::default()),
        _ => Parser::Generic(GenericParser::default()),
    };
    let mut finish: Option<Finish> = None;
    let idle = idle_limit(spec, timeout);
    let cap = tokio::time::sleep(RUN_CAP);
    tokio::pin!(cap);
    let mut stopped: Option<String> = None;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => { stopped = Some("사용자가 중지했습니다".into()); break; }
            _ = &mut cap => { stopped = Some(capped_error()); break; }
            // 줄이 올 때마다 새로 잰다: 일하는 동안은 끝내지 않는다.
            _ = tokio::time::sleep(idle) => { stopped = Some(stalled_error(idle)); out.stalled = true; break; }
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    for ev in parser.feed(&l) {
                        match &ev {
                            Event::Text(t) => out.text.push_str(t),
                            Event::Activity(a) => out.activities.push(a.clone()),
                            Event::Session(s) => out.session_id = Some(s.clone()),
                            Event::Usage { .. } | Event::Bg(_) => {}
                            Event::Done(f) => finish = Some(f.clone()),
                        }
                        on_event(&ev);
                    }
                    // 무료 모델만 쓰기로 했으므로 비용이 잡히는 순간 멈춘다.
                    if let Parser::OpenCode(p) = &parser {
                        if p.charged > 0.0 {
                            stopped = Some("무료 모델에서 비용 청구가 감지되어 실행이 중단되었습니다.".to_string());
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }
    if stopped.is_some() {
        kill_tree(&mut child).await;
    }
    let status = child.wait().await.ok();
    // 끝낸 뒤에는 stderr를 오래 기다리지 않는다(잡을 수 없는 손자 프로세스가 파이프를 쥐고 있어도 중지가 멈추지 않게).
    let stderr_tail = if stopped.is_some() { tokio::time::timeout(Duration::from_secs(2), err_task).await.ok().and_then(|r| r.ok()).unwrap_or_default() } else { err_task.await.unwrap_or_default() };
    out.latency_ms = started.elapsed().as_millis() as u64;
    if let Some(msg) = stopped {
        return fail(out, msg);
    }
    let f = finish.unwrap_or_else(|| match &parser {
        Parser::Generic(p) => p.finish(),
        Parser::OpenCode(p) => p.finish(),
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
            attachments: vec![],
        }
    }

    #[tokio::test]
    async fn pulse_waits_while_output_keeps_coming() {
        let p = Pulse::new();
        let idle = Duration::from_millis(150);
        let started = Instant::now();
        let feed = async {
            for _ in 0..5 {
                tokio::time::sleep(Duration::from_millis(60)).await;
                p.beat();
            }
        };
        let ((), ()) = tokio::join!(feed, async {
            p.stalled(idle).await;
        });
        // 60ms마다 소식이 오는 동안(300ms)은 끝나지 않고, 마지막 소식 뒤 150ms가 지나서야 끝난다.
        assert!(started.elapsed() >= Duration::from_millis(440), "{:?}", started.elapsed());
    }

    /// 가짜 엔진(PATH 앞의 codex.cmd)으로 실제 실행 경로를 확인한다. PATH를 바꾸므로 따로 돌린다:
    /// cargo test --lib idle_stop_follows_output -- --ignored
    #[tokio::test]
    #[ignore]
    async fn idle_stop_follows_output() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex.cmd"), "@echo off
if \"%FAKE_MODE%\"==\"busy\" (for /L %%i in (1,1,5) do (echo {} & ping -n 2 127.0.0.1 >nul)) else (echo {} & ping -n 11 127.0.0.1 >nul)
").unwrap();
        let path = format!("{};{}", dir.path().display(), std::env::var("PATH").unwrap_or_default());
        std::env::set_var("PATH", path);
        let http = reqwest::Client::new();
        let s = RunSpec { resume: None, plugin_dirs: vec![], extra_dirs: vec![], cwd: dir.path().to_path_buf(), ..spec(EngineId::Codex, false) };
        let idle = Duration::from_secs(3);
        // 1초마다 출력이 나오면 한도(3초)를 넘겨 5초 넘게 돌아도 끝까지 둔다.
        std::env::set_var("FAKE_MODE", "busy");
        let t = Instant::now();
        let o = run(&http, &s, idle, &CancellationToken::new(), |_| {}).await;
        assert!(!o.stalled && t.elapsed() > Duration::from_secs(4), "{:?} {:?}", o.error, t.elapsed());
        // 출력이 멈추면 한도가 지나 끝낸다.
        std::env::set_var("FAKE_MODE", "quiet");
        let t = Instant::now();
        let o = run(&http, &s, idle, &CancellationToken::new(), |_| {}).await;
        assert!(o.stalled && t.elapsed() < Duration::from_secs(8), "{:?} {:?}", o.error, t.elapsed());
        assert_eq!(o.error.as_deref(), Some(stalled_error(idle).as_str()));
    }

    /// 실제 Claude Code로 백그라운드 셸을 돌려 본다(로그인 필요, 비용 조금): cargo test --lib live_claude_background -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_claude_background_shell() {
        let dir = tempfile::tempdir().unwrap();
        let s = RunSpec { prompt: "Use the PowerShell tool with run_in_background set to true to run exactly: Start-Sleep -Seconds 4; Write-Output bg-done-42 . Then wait until it finishes, read its output, and reply with the output only.".into(), system: String::new(), cwd: dir.path().to_path_buf(), resume: None, plugin_dirs: vec![], extra_dirs: vec![], model: Some("haiku".into()), ..spec(EngineId::Claude, true) };
        let mut bg = vec![];
        let o = run(&reqwest::Client::new(), &s, Duration::from_secs(240), &CancellationToken::new(), |e| {
            if let Event::Bg(b) = e {
                println!("{b:?}");
                bg.push(b.clone());
            }
        })
        .await;
        println!("ok={} text={:?} err={:?}", o.ok, o.text, o.error);
        let Some(Bg::Started { path, .. }) = bg.first() else { panic!("백그라운드 셸을 못 봤다") };
        assert!(crate::bgjobs::tests_allowed(path), "{path:?}");
        println!("output file: {:?}", std::fs::read_to_string(path));
    }

    #[test]
    fn claude_background_shells_are_reported_with_their_output_file() {
        let mut p = ClaudeParser::default();
        let call = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"toolu_1","name":"PowerShell","input":{"command":"./gradlew build","run_in_background":true}},{"type":"tool_use","id":"toolu_2","name":"PowerShell","input":{"command":"ls"}}]}}"#;
        assert!(!p.feed(call).iter().any(|e| matches!(e, Event::Bg(_))));
        let done = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"a.txt"},{"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"Command running in background with ID: b3xk9. Output is being written to: C:\\Temp\\claude\\h\\sid\\tasks\\b3xk9.output. You will be notified when it completes."}]}]}}"#;
        let evs = p.feed(done);
        assert_eq!(evs, vec![Event::Bg(Bg::Started { id: "b3xk9".into(), command: "./gradlew build".into(), path: PathBuf::from(r"C:\Temp\claude\h\sid\tasks\b3xk9.output") })]);
        let note = r#"{"type":"user","message":{"content":"<task-notification>\n<task-id>b3xk9</task-id>\n<tool-use-id>toolu_1</tool-use-id>\n<status>completed</status>\n<summary>done</summary></task-notification>"}}"#;
        assert_eq!(p.feed(note), vec![Event::Bg(Bg::Ended { id: "b3xk9".into(), status: "completed".into() })]);
        assert!(bg_ended("<task-id>x</task-id> no status here").is_empty());
    }

    #[test]
    fn work_runs_get_a_long_idle_limit() {
        assert_eq!(idle_limit(&spec(EngineId::Claude, true), Duration::from_secs(90)), WORK_IDLE);
        assert_eq!(idle_limit(&spec(EngineId::Claude, true), Duration::from_secs(1800)), Duration::from_secs(1800));
        assert_eq!(idle_limit(&spec(EngineId::Claude, false), Duration::from_secs(90)), Duration::from_secs(90));
        assert!(stalled_error(Duration::from_secs(1200)).contains("20"));
        assert!(capped_error().contains("6"));
    }

    /// 실제 무료 모델로 끝까지 돌려 본다(네트워크 필요): cargo test --lib live_opencode -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_opencode_run_attach_permissions_and_resume() {
        let http = reqwest::Client::new();
        let cancel = CancellationToken::new();
        let w = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        let a = crate::attachments::add_bytes(store.path(), "내 메모 & 정리 (최종).txt", "secret-word: walrus-77".as_bytes(), "user").unwrap();
        let z = crate::attachments::add_bytes(store.path(), "data 2.bin", &[1, 0, 2, 3], "user").unwrap();
        let base = RunSpec {
            engine: EngineId::OpenCode,
            prompt: String::new(),
            system: String::new(),
            cwd: w.path().to_path_buf(),
            work: false,
            resume: None,
            plugin_dirs: vec![],
            model: None,
            effort: None,
            max_tokens: None,
            extra_dirs: vec![crate::attachments::path_of(store.path(), &z).parent().unwrap().to_string_lossy().to_string()],
            steer: None,
            attachments: vec![crate::attachments::to_file(store.path(), &a), crate::attachments::to_file(store.path(), &z)],
        };
        let t = Duration::from_secs(120);
        let chat = RunSpec { prompt: "내가 첨부한 메모 파일에 적힌 secret-word 값을 알려 줘. 그리고 hello.txt 파일을 만들고 dir 명령도 실행해 봐.".into(), ..base.clone() };
        let o = run(&http, &chat, t, &cancel, |_| {}).await;
        println!("chat ok={} err={:?} text={} acts={:?} tokens={:?}", o.ok, o.error, o.text, o.activities, o.input_tokens);
        assert!(o.ok, "{:?}", o.error);
        assert!(o.text.contains("walrus-77"), "첨부를 읽어야 한다: {}", o.text);
        assert!(!w.path().join("hello.txt").exists(), "대화 모드는 파일을 만들 수 없다");
        assert_eq!(o.cost_usd, Some(0.0));
        let sid = o.session_id.clone().unwrap();
        let again = RunSpec { prompt: "방금 답한 secret-word를 다시 한 줄로 답해. 파일을 다시 열지 마.".into(), attachments: vec![], resume: Some(sid), ..base.clone() };
        let o = run(&http, &again, t, &cancel, |_| {}).await;
        println!("resume ok={} text={}", o.ok, o.text);
        assert!(o.text.contains("walrus-77"), "세션을 이어 써야 한다: {}", o.text);
        let work = RunSpec { prompt: "작업 폴더에 hello.txt 파일을 만들고 내용으로 hi 만 적어. 다른 일은 하지 마.".into(), work: true, attachments: vec![], ..base.clone() };
        let o = run(&http, &work, t, &cancel, |_| {}).await;
        println!("work ok={} text={} acts={:?}", o.ok, o.text, o.activities);
        assert!(w.path().join("hello.txt").exists(), "작업 모드는 파일을 만든다");
        let bad = RunSpec { model: Some("anthropic/claude-opus-5-5".into()), prompt: "한 단어로 인사해.".into(), attachments: vec![], ..base };
        assert!(args(&bad).iter().any(|x| x == OPENCODE_DEFAULT), "유료 모델은 넘기지 않는다");
    }

    fn file(name: &str, kind: &str) -> AttachedFile {
        AttachedFile { name: name.into(), path: PathBuf::from(format!("C:\\att\\{name}")), kind: kind.into(), mime: "text/plain".into(), size: 2048 }
    }

    // 2026-11-02 opencode 2.2.5 실제 출력에서 줄인 표본
    const OPENCODE_SAMPLE: &[&str] = &[
        r#"{"type":"step_start","sessionID":"ses_1","part":{"type":"step-start"}}"#,
        r#"{"type":"tool_use","sessionID":"ses_1","part":{"type":"tool","tool":"read","state":{"status":"completed","input":{"path":"C:\\w\\note.txt"},"output":"zebra"}}}"#,
        r#"{"type":"step_finish","sessionID":"ses_1","part":{"type":"step-finish","reason":"tool-calls","cost":0,"tokens":{"input":10370,"output":98,"reasoning":33,"cache":{"read":1664,"write":0}}}}"#,
        r#"{"type":"text","sessionID":"ses_1","part":{"type":"text","text":"zebra-42"}}"#,
        r#"{"type":"step_finish","sessionID":"ses_1","part":{"type":"step-finish","reason":"stop","cost":0,"tokens":{"input":222,"output":5,"reasoning":0,"cache":{"read":12032,"write":0}}}}"#,
    ];

    #[test]
    fn parses_opencode_stream() {
        let mut p = OpenCodeParser::default();
        let evs: Vec<Event> = OPENCODE_SAMPLE.iter().flat_map(|l| p.feed(l)).filter(|e| !matches!(e, Event::Usage { .. })).collect();
        assert_eq!(evs[0], Event::Session("ses_1".into()));
        assert_eq!(evs.iter().filter(|e| matches!(e, Event::Session(_))).count(), 1);
        assert_eq!(evs[1], Event::Activity("read · C:\\w\\note.txt".into()));
        assert_eq!(evs[2], Event::Text("zebra-42".into()));
        let f = p.finish();
        assert!(f.ok && f.input_tokens == Some(10370 + 1664 + 222 + 12032) && f.output_tokens == Some(103) && p.charged == 0.0);
        let mut bad = OpenCodeParser::default();
        let e = bad.feed(r#"{"type":"error","sessionID":"s","error":{"type":"provider.no-route","message":"Model unavailable: x"}}"#);
        assert!(matches!(&e[1], Event::Done(f) if !f.ok && f.error.as_deref() == Some("Model unavailable: x")));
        let limit = OpenCodeParser::default().feed(r#"{"type":"error","sessionID":"s","error":{"type":"provider.free-limit","message":"Connect a provider or subscribe to keep going"}}"#);
        assert!(matches!(&limit[1], Event::Done(f) if !f.ok && !f.error.as_deref().unwrap().contains("subscribe")), "유료 연결 안내를 보여 주지 않는다");
        assert!(!crate::apis::transient(match &limit[1] { Event::Done(f) => f.error.as_deref().unwrap(), _ => "" }), "한도 소진은 바로 다시 시도하지 않는다");
        assert!(!OpenCodeParser::default().finish().ok, "아무 단계도 끝나지 않으면 실패");
        let mut paid = OpenCodeParser::default();
        paid.feed(r#"{"type":"step_finish","part":{"cost":0.01,"tokens":{}}}"#);
        assert!(paid.charged > 0.0);
    }

    #[test]
    fn live_usage_is_reported_while_running() {
        let mut c = ClaudeParser::default();
        let a = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":5,"cache_read_input_tokens":100,"output_tokens":7}}}"#;
        assert_eq!(c.feed(a)[0], Event::Usage { input: 105, output: 7 });
        assert!(c.feed(a).is_empty(), "같은 메시지의 다른 블록은 다시 더하지 않는다");
        let b = r#"{"type":"assistant","message":{"id":"m2","content":[],"usage":{"input_tokens":10,"output_tokens":3}}}"#;
        assert_eq!(c.feed(b)[0], Event::Usage { input: 115, output: 10 });
        let mut g = AgyParser::default();
        let step = |i: u64, o: u64| format!(r#"{{"event":"step_update","step_update":{{"step_index":{i},"state":"DONE","step_type":"agent_response","usage":{{"input_tokens":100,"output_tokens":{o}}}}}}}"#);
        assert_eq!(g.feed(&step(1, 5))[0], Event::Usage { input: 100, output: 5 });
        assert!(g.feed(&step(1, 5)).is_empty(), "바뀌지 않으면 알리지 않는다");
        assert_eq!(g.feed(&step(2, 8))[0], Event::Usage { input: 200, output: 13 });
    }

    #[test]
    fn opencode_never_receives_a_paid_model() {
        for free in ["opencode/beam-free", "opencode/big-pickle", OPENCODE_DEFAULT] {
            assert!(is_free_model(free), "{free}");
        }
        for paid in ["anthropic/claude-opus-5-5", "opencode/claude-opus-5-5", "openai/gpt-5-free", "opencode/a b-free", ""] {
            assert!(!is_free_model(paid), "{paid}");
            assert_eq!(opencode_model(Some(paid)), OPENCODE_DEFAULT);
        }
        assert_eq!(opencode_model(None), OPENCODE_DEFAULT);
        let list = "anthropic/claude-opus-5-5\nopencode/big-pickle\nopencode/beam-free\nopencode/gpt-5\n";
        assert_eq!(free_models(list), ["opencode/big-pickle", "opencode/beam-free"]);
        let mut sp = spec(EngineId::OpenCode, false);
        sp.model = Some("anthropic/claude-opus-5-5".into());
        let a = args(&sp);
        let i = a.iter().position(|x| x == "--model").unwrap();
        assert_eq!(a[i + 1], OPENCODE_DEFAULT);
        assert!(a.iter().any(|x| x == "--standalone"), "권한 설정을 읽게 private 서버로 돈다");
        assert!(a.windows(2).any(|w| w[0] == "--session" && w[1] == "abc"));
        let c: Value = serde_json::from_str(&opencode_config(&sp)).unwrap();
        assert_eq!(c["model"], OPENCODE_DEFAULT);
        assert_eq!(c["small_model"], OPENCODE_DEFAULT, "제목 생성도 무료 모델로");
    }

    #[test]
    fn opencode_chat_mode_cannot_write_or_run_commands() {
        let chat: Value = serde_json::from_str(&opencode_config(&spec(EngineId::OpenCode, false))).unwrap();
        for k in ["*", "edit", "bash", "shell", "task"] {
            assert_eq!(chat["permission"][k], "deny", "{k}");
        }
        assert_eq!(chat["permission"]["read"], "allow");
        assert!(serde_json::to_string(&chat["permission"]).unwrap().starts_with(r#"{"*":"#), "\"*\"가 먼저 와야 나머지 규칙이 이긴다");
        let work: Value = serde_json::from_str(&opencode_config(&spec(EngineId::OpenCode, true))).unwrap();
        assert_eq!(work["permission"]["edit"], "allow");
        assert_eq!(work["permission"]["task"], "deny", "하위 에이전트는 다른 모델을 쓸 수 있다");
        assert_eq!(work["permission"]["external_directory"]["*"], "deny");
        assert_eq!(work["permission"]["external_directory"]["C:/s/**"], "allow");
    }

    #[test]
    fn attachments_reach_each_engine_its_own_way() {
        let files = vec![file("a.txt", "text"), file("b.png", "image"), file("c.zip", "file")];
        let mut oc = spec(EngineId::OpenCode, false);
        oc.attachments = files.clone();
        let a = args(&oc);
        assert_eq!(a.iter().filter(|x| *x == "--file").count(), 2, "글·이미지만 --file");
        let body = stdin_text(&oc);
        assert!(body.contains("c.zip") && body.contains("C:\\att\\c.zip"), "나머지는 경로 안내");
        assert!(body.contains("a.txt") && !body.contains("C:\\att\\a.txt"), "메시지에 넣은 파일은 이름만");
        let mut cl = spec(EngineId::Claude, false);
        cl.attachments = files;
        let body = stdin_text(&cl);
        assert!(body.contains("a.txt") && body.contains("b.png") && body.contains("c.zip") && body.contains("[첨부 파일]"));
        assert!(!args(&cl).iter().any(|x| x.contains("a.txt")));
        assert_eq!(stdin_text(&spec(EngineId::Claude, false)), "요청 & del /q *", "첨부가 없으면 그대로");
    }

    /// 손자 프로세스가 stdout을 쥐고 있으므로, 트리째 끝나야만 EOF가 온다.
    #[cfg(windows)]
    #[test]
    fn kill_tree_ends_grandchildren_holding_the_pipe() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/D", "/C", "ping -n 60 127.0.0.1"]).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).creation_flags(NO_WINDOW);
            let mut child = c.spawn().unwrap();
            let mut out = child.stdout.take().unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            kill_tree(&mut child).await;
            let mut buf = Vec::new();
            let r = tokio::time::timeout(Duration::from_secs(10), out.read_to_end(&mut buf)).await;
            assert!(r.is_ok(), "손자 프로세스가 남아 파이프가 닫히지 않았다");
        });
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
        .filter(|e| !matches!(e, Event::Usage { .. }))
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
    fn claude_args_block_known_window_tasks_only_when_the_queue_is_in_use() {
        let spec = |system: &str| RunSpec { engine: EngineId::Claude, prompt: String::new(), system: system.into(), cwd: PathBuf::from("."), work: true, resume: None, plugin_dirs: vec![], model: None, effort: None, max_tokens: None, extra_dirs: vec![], steer: None, attachments: vec![] };
        let queued = args(&spec(crate::lead::QUEUE_SYSTEM));
        let at = queued.iter().position(|a| a == "--disallowedTools").expect("차단 규칙");
        assert!(queued[at + 1].contains("PowerShell(*runClient*)"), "{}", queued[at + 1]);
        assert!(!args(&spec("다른 지시")).iter().any(|a| a == "--disallowedTools"), "대기열을 안 쓰는 실행은 그대로");
    }

    #[test]
    fn claude_settings_exclude_only_the_home_instructions() {
        let v: Value = serde_json::from_str(&claude_settings("C:\\Users\\me")).unwrap();
        let list: Vec<&str> = v["claudeMdExcludes"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert_eq!(list, vec!["C:/Users/me/.claude/CLAUDE.md", "C:/Users/me/.claude/rules/**"]);
    }

    #[test]
    fn parses_antigravity_stream_and_keeps_chat_read_only() {
        let mut p = AgyParser::default();
        let evs: Vec<Event> = AGY_SAMPLE.iter().flat_map(|l| p.feed(l)).filter(|e| !matches!(e, Event::Usage { .. })).collect();
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

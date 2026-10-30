//! API 엔진(Claude API·OpenAI·Gemini API·NVIDIA)의 작업 모드: 모델이 도구를 부르면 엘리시움이 이 PC에서 실행하고 결과를 돌려준다.
//! 파일 도구는 작업 폴더 안에서만 쓰고 고친다(읽기는 스킬 참고 폴더도 허용). PowerShell은 샌드박스 없이 사용자 권한으로 돈다.

use crate::apis::{self, Api, Wire};
use crate::engines::{Event, Outcome, RunSpec};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// 한 요청에서 모델이 도구를 부를 수 있는 최대 횟수(차례 수)
const MAX_TURNS: usize = 60;
/// 파일 전체를 쓰려면 출력이 길어야 한다. 스트리밍 없는 요청이 허용되는 범위 안에서 잡는다.
const WORK_MAX_TOKENS: u32 = 16000;
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".venv", "venv", "__pycache__", ".next"];
const MAX_OUT: usize = 30_000;

/// (이름, 설명, 입력 스키마)
fn tools() -> Vec<(&'static str, &'static str, Value)> {
    vec![
        ("list_dir", "List files and folders in a directory (relative to the workspace). Folders end with '/'.", json!({"type":"object","properties":{"path":{"type":"string","description":"Directory path, default '.'"}}})),
        ("read_file", "Read a text file with line numbers. Use offset/limit for large files.", json!({"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","description":"1-based first line"},"limit":{"type":"integer","description":"Max lines (default 2000)"}},"required":["path"]})),
        ("write_file", "Create or overwrite a file with the full content. Parent folders are created.", json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]})),
        ("edit_file", "Replace exact text in a file. old_text must match exactly and be unique unless replace_all is true.", json!({"type":"object","properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"replace_all":{"type":"boolean"}},"required":["path","old_text","new_text"]})),
        ("find_files", "Find files whose path contains the given text (case-insensitive).", json!({"type":"object","properties":{"name":{"type":"string"},"path":{"type":"string","description":"Folder to search, default '.'"}},"required":["name"]})),
        ("search_text", "Search file contents for a text (case-insensitive). Returns path:line: text.", json!({"type":"object","properties":{"query":{"type":"string"},"path":{"type":"string","description":"Folder or file to search, default '.'"}},"required":["query"]})),
        ("move_path", "Move or rename a file or folder inside the workspace.", json!({"type":"object","properties":{"from":{"type":"string"},"to":{"type":"string"}},"required":["from","to"]})),
        ("delete_path", "Delete a file or folder (recursively) inside the workspace.", json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]})),
        ("run_powershell", "Run a Windows PowerShell command in the workspace folder (install, build, test, run). Non-interactive; returns exit code and output.", json!({"type":"object","properties":{"command":{"type":"string"},"timeout_sec":{"type":"integer","description":"Default 120, max 600"}},"required":["command"]})),
    ]
}

fn tools_for(wire: Wire) -> Value {
    let mut list: Vec<Value> = tools()
        .into_iter()
        .map(|(name, desc, schema)| match wire {
            Wire::Anthropic => json!({ "name": name, "description": desc, "input_schema": schema }),
            Wire::OpenAiCompat => json!({ "type": "function", "function": { "name": name, "description": desc, "parameters": schema } }),
        })
        .collect();
    // 도구 정의는 매 차례 같으므로 마지막 도구에 캐시 표시를 붙인다(시스템 지시와 함께 앞부분이 캐시된다).
    if let (Wire::Anthropic, Some(last)) = (wire, list.last_mut()) {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }
    Value::Array(list)
}

const AGENT_SYSTEM: &str = "\n[도구] 이 작업 폴더의 파일을 다루는 도구(list_dir·read_file·write_file·edit_file·find_files·search_text·move_path·delete_path·run_powershell)를 쓸 수 있습니다. 경로는 작업 폴더 기준 상대 경로로 적으세요. 고치기 전에 먼저 읽고, 일을 끝내면 도구를 더 부르지 말고 결과를 요약해 답하세요.\n";

/// 작업 폴더 안의 경로(쓰기) 또는 작업 폴더·참고 폴더 안의 경로(읽기)
struct Sandbox {
    root: PathBuf,
    read_only: Vec<PathBuf>,
    changed: Vec<String>,
}

/// `..`·`.`을 글자 그대로 정리한다(없는 경로도 다룰 수 있게 파일 시스템을 보지 않는다).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Windows 경로는 대소문자를 가리지 않으므로 소문자로 비교한다.
fn within(path: &Path, base: &Path) -> bool {
    let low = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let (p, b) = (low(path), low(base));
    p == b || p.starts_with(&format!("{b}\\"))
}

impl Sandbox {
    fn resolve(&self, raw: &str, write: bool) -> Result<PathBuf, String> {
        let raw = raw.trim();
        let raw = if raw.is_empty() { "." } else { raw };
        let p = Path::new(raw);
        let full = normalize(&if p.is_absolute() { p.to_path_buf() } else { self.root.join(p) });
        let ok_lexical = within(&full, &self.root) || (!write && self.read_only.iter().any(|d| within(&full, d)));
        if !ok_lexical {
            return Err(format!("작업 폴더 밖의 경로는 {}할 수 없습니다: {raw}", if write { "고칠" } else { "읽을" }));
        }
        // 연결(심볼릭 링크·정션)로 밖을 가리키는 경우도 막는다.
        if let (Ok(real), Ok(root)) = (full.canonicalize(), self.root.canonicalize()) {
            let ok = within(&real, &root) || (!write && self.read_only.iter().filter_map(|d| d.canonicalize().ok()).any(|d| within(&real, &d)));
            if !ok {
                return Err(format!("작업 폴더 밖을 가리키는 경로입니다: {raw}"));
            }
        }
        Ok(full)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).map(|r| r.to_string_lossy().replace('\\', "/")).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| p.to_string_lossy().to_string())
    }

    fn touched(&mut self, p: &Path) {
        let r = self.rel(p);
        if !self.changed.contains(&r) {
            self.changed.push(r);
        }
    }
}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| format!("'{k}' 값이 필요합니다"))
}

fn clip(t: &str) -> String {
    if t.chars().count() <= MAX_OUT {
        return t.to_string();
    }
    let head: String = t.chars().take(MAX_OUT / 2).collect();
    let tail: String = t.chars().rev().take(MAX_OUT / 2).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n… (중간 생략) …\n{tail}")
}

/// 폴더를 돌며 파일 경로를 모은다(무거운 폴더는 건너뛴다).
fn walk(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP_DIRS.iter().any(|s| e.file_name().to_string_lossy().eq_ignore_ascii_case(s)) {
                    stack.push(p);
                }
            } else if ft.is_file() {
                out.push(p);
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

fn is_binary(b: &[u8]) -> bool {
    b.iter().take(8000).any(|x| *x == 0)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

async fn powershell(root: &Path, command: &str, secs: u64) -> Result<String, String> {
    // -EncodedCommand(UTF-16LE base64)로 넘겨 따옴표·줄바꿈이 섞인 명령도 그대로 전달한다.
    let script = format!("$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[System.Text.Encoding]::UTF8; {command}");
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let enc = base64(&utf16);
    let mut c = crate::engines::command(Path::new("powershell.exe"));
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", enc.as_str()])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = c.spawn().map_err(|e| format!("PowerShell을 실행할 수 없습니다: {e}"))?;
    // 시간을 넘기면 future가 버려지며 kill_on_drop으로 프로세스가 끝난다.
    match tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()).await {
        Ok(Ok(o)) => {
            let mut text = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&err);
            }
            let code = o.status.code().unwrap_or(-1);
            let body = format!("종료 코드 {code}\n{}", clip(text.trim()));
            if o.status.success() {
                Ok(body)
            } else {
                Err(body)
            }
        }
        Ok(Err(e)) => Err(format!("PowerShell 실행 실패: {e}")),
        Err(_) => Err(format!("{secs}초 안에 끝나지 않아 멈췄습니다")),
    }
}

/// 도구 하나를 실행한다. Err면 모델에게 오류로 돌려준다.
async fn exec(sb: &mut Sandbox, name: &str, input: &Value) -> Result<String, String> {
    match name {
        "list_dir" => {
            let dir = sb.resolve(input.get("path").and_then(Value::as_str).unwrap_or("."), false)?;
            let mut items: Vec<(bool, String, u64)> = std::fs::read_dir(&dir)
                .map_err(|e| format!("폴더를 열 수 없습니다: {e}"))?
                .flatten()
                .map(|e| {
                    let md = e.metadata().ok();
                    (md.as_ref().is_some_and(|m| m.is_dir()), e.file_name().to_string_lossy().to_string(), md.map_or(0, |m| m.len()))
                })
                .collect();
            items.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
            let total = items.len();
            let mut lines: Vec<String> = items.into_iter().take(500).map(|(d, n, len)| if d { format!("{n}/") } else { format!("{n} ({len} B)") }).collect();
            if total > 500 {
                lines.push(format!("… 외 {}개", total - 500));
            }
            Ok(if lines.is_empty() { "(빈 폴더)".into() } else { lines.join("\n") })
        }
        "read_file" => {
            let p = sb.resolve(s(input, "path")?, false)?;
            let bytes = std::fs::read(&p).map_err(|e| format!("읽을 수 없습니다: {e}"))?;
            if is_binary(&bytes) {
                return Err(format!("바이너리 파일입니다({} B)", bytes.len()));
            }
            let text = String::from_utf8_lossy(&bytes);
            let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
            let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(2000).clamp(1, 5000) as usize;
            let total = text.lines().count();
            let body: Vec<String> = text.lines().enumerate().skip(offset - 1).take(limit).map(|(i, l)| format!("{:>5}\t{l}", i + 1)).collect();
            let more = if offset - 1 + limit < total { format!("\n… (전체 {total}줄, offset으로 이어 읽기)") } else { String::new() };
            Ok(format!("{}{more}", clip(&body.join("\n"))))
        }
        "write_file" => {
            let p = sb.resolve(s(input, "path")?, true)?;
            let content = s(input, "content")?;
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("폴더를 만들 수 없습니다: {e}"))?;
            }
            std::fs::write(&p, content).map_err(|e| format!("저장할 수 없습니다: {e}"))?;
            sb.touched(&p);
            Ok(format!("저장함: {} ({} B)", sb.rel(&p), content.len()))
        }
        "edit_file" => {
            let p = sb.resolve(s(input, "path")?, true)?;
            let (old, new) = (s(input, "old_text")?, s(input, "new_text")?);
            if old.is_empty() {
                return Err("old_text가 비어 있습니다. 새 파일은 write_file을 쓰세요".into());
            }
            let text = std::fs::read_to_string(&p).map_err(|e| format!("읽을 수 없습니다: {e}"))?;
            let n = text.matches(old).count();
            let all = input.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
            match n {
                0 => return Err("old_text를 찾지 못했습니다. read_file로 정확한 글을 확인하세요".into()),
                n if n > 1 && !all => return Err(format!("old_text가 {n}곳에 있습니다. 더 길게 적어 하나로 좁히거나 replace_all을 쓰세요")),
                _ => {}
            }
            let out = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
            std::fs::write(&p, out).map_err(|e| format!("저장할 수 없습니다: {e}"))?;
            sb.touched(&p);
            Ok(format!("고침: {} ({}곳)", sb.rel(&p), if all { n } else { 1 }))
        }
        "find_files" => {
            let dir = sb.resolve(input.get("path").and_then(Value::as_str).unwrap_or("."), false)?;
            let q = s(input, "name")?.to_lowercase();
            let hits: Vec<String> = walk(&dir, 20_000).into_iter().map(|p| sb.rel(&p)).filter(|r| r.to_lowercase().contains(&q)).take(200).collect();
            Ok(if hits.is_empty() { "찾지 못했습니다".into() } else { hits.join("\n") })
        }
        "search_text" => {
            let target = sb.resolve(input.get("path").and_then(Value::as_str).unwrap_or("."), false)?;
            let q = s(input, "query")?.to_lowercase();
            if q.is_empty() {
                return Err("query가 비어 있습니다".into());
            }
            let files = if target.is_file() { vec![target] } else { walk(&target, 5_000) };
            let mut hits = vec![];
            'files: for f in files {
                let Ok(b) = std::fs::read(&f) else { continue };
                if b.len() > 1_000_000 || is_binary(&b) {
                    continue;
                }
                for (i, line) in String::from_utf8_lossy(&b).lines().enumerate() {
                    if line.to_lowercase().contains(&q) {
                        hits.push(format!("{}:{}: {}", sb.rel(&f), i + 1, line.trim().chars().take(200).collect::<String>()));
                        if hits.len() >= 100 {
                            break 'files;
                        }
                    }
                }
            }
            Ok(if hits.is_empty() { "찾지 못했습니다".into() } else { hits.join("\n") })
        }
        "move_path" => {
            let (from, to) = (sb.resolve(s(input, "from")?, true)?, sb.resolve(s(input, "to")?, true)?);
            if within(&sb.root, &from) {
                return Err("작업 폴더 자체는 옮길 수 없습니다".into());
            }
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("폴더를 만들 수 없습니다: {e}"))?;
            }
            std::fs::rename(&from, &to).map_err(|e| format!("옮길 수 없습니다: {e}"))?;
            sb.touched(&from);
            sb.touched(&to);
            Ok(format!("옮김: {} → {}", sb.rel(&from), sb.rel(&to)))
        }
        "delete_path" => {
            let p = sb.resolve(s(input, "path")?, true)?;
            if within(&sb.root, &p) {
                return Err("작업 폴더 자체는 지울 수 없습니다".into());
            }
            let md = std::fs::symlink_metadata(&p).map_err(|e| format!("찾을 수 없습니다: {e}"))?;
            let r = if md.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            r.map_err(|e| format!("지울 수 없습니다: {e}"))?;
            sb.touched(&p);
            Ok(format!("지움: {}", sb.rel(&p)))
        }
        "run_powershell" => {
            let secs = input.get("timeout_sec").and_then(Value::as_u64).unwrap_or(120).clamp(5, 600);
            powershell(&sb.root, s(input, "command")?, secs).await
        }
        other => Err(format!("알 수 없는 도구: {other}")),
    }
}

/// 진행 표시용 한 줄(도구 이름 · 대상)
fn activity(name: &str, input: &Value) -> String {
    let target = ["path", "command", "name", "query", "from"].iter().find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or("");
    let t: String = target.replace('\n', " ").chars().take(80).collect();
    if t.is_empty() { name.to_string() } else { format!("{name} · {t}") }
}

/// 작업 중 쌓인 사용자 메시지를 꺼낸다(칸이 없거나 받지 않는 AI면 빈 목록).
fn take_steer(spec: &RunSpec) -> Vec<String> {
    spec.steer.as_ref().and_then(|s| s.lock().unwrap_or_else(|p| p.into_inner()).as_mut().map(std::mem::take)).unwrap_or_default()
}

/// 사용자 메시지를 대화에 넣는다. Anthropic은 직전이 도구 결과(사용자 차례)면 그 차례에 블록을 덧붙이고, 아니면 새 차례를 만든다.
fn push_steer(wire: Wire, msgs: &mut Vec<Value>, extra: &[String]) {
    let text = format!("[작업 중 사용자가 보낸 메시지]\n{}", extra.join("\n\n"));
    match wire {
        Wire::Anthropic => {
            let block = json!({ "type": "text", "text": text });
            if let Some(arr) = msgs.last_mut().filter(|m| m["role"] == "user").and_then(|m| m["content"].as_array_mut()) {
                arr.push(block);
            } else {
                msgs.push(json!({ "role": "user", "content": [block] }));
            }
        }
        Wire::OpenAiCompat => msgs.push(json!({ "role": "user", "content": text })),
    }
}

/// Anthropic: 요청마다 대화 끝에 캐시 표시를 하나 붙인다. 앞부분(시스템·도구·이전 차례)은 다음 차례에서 싸게 읽힌다.
/// 표시는 요청마다 새로 붙이므로 쌓이지 않는다(시스템·도구 표시와 합쳐 최대 3개).
fn cache_last(wire: Wire, msgs: &[Value]) -> Vec<Value> {
    let mut out = msgs.to_vec();
    if wire == Wire::Anthropic {
        if let Some(block) = out.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|b| b.last_mut()) {
            block["cache_control"] = json!({ "type": "ephemeral" });
        }
    }
    out
}

/// 한 번 요청(일시적인 과부하·요청 한도는 3번까지 다시 시도)
async fn call(http: &reqwest::Client, a: &Api, key: &str, label: &str, body: &Value, on_event: &mut impl FnMut(&Event)) -> Result<Value, String> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let req = match a.wire {
            Wire::OpenAiCompat => http.post(format!("{}/chat/completions", a.base_url)).bearer_auth(key),
            Wire::Anthropic => http.post(format!("{}/v1/messages", a.base_url)).header("x-api-key", key).header("anthropic-version", "2023-06-01"),
        };
        let r: Result<Value, String> = async {
            let resp = req.json(body).send().await.map_err(|e| if e.is_timeout() { "응답 시간 초과".to_string() } else { format!("{label}에 연결할 수 없습니다") })?;
            let code = resp.status().as_u16();
            if !resp.status().is_success() {
                return Err(apis::status_message(code, &resp.text().await.unwrap_or_default()));
            }
            let v: Value = resp.json().await.map_err(|e| format!("응답을 읽을 수 없습니다: {e}"))?;
            match v.get("error").filter(|e| !e.is_null()) {
                Some(e) => Err(apis::stream_error(e)),
                None => Ok(v),
            }
        }
        .await;
        match r {
            Err(e) if attempt < 3 && apis::transient(&e) => {
                on_event(&Event::Activity(format!("{label} 서버가 바빠 {}초 뒤 다시 시도합니다({}/3)", attempt * 2, attempt + 1)));
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            }
            other => return other,
        }
    }
}

/// 도구 호출 하나(id, 이름, 입력). OpenAI 호환은 입력이 JSON 문자열로 와서 풀다가 실패할 수 있다.
type Call = (String, String, Result<Value, String>);

/// 작업 모드 실행. 답(마지막 요약)을 돌려주고 사용량·활동은 out에 쌓는다.
#[allow(clippy::too_many_arguments)]
pub async fn run(http: &reqwest::Client, spec: &RunSpec, a: &Api, key: &str, model: &str, out: &mut Outcome, usage: &mut apis::Usage, on_event: &mut impl FnMut(&Event)) -> Result<String, String> {
    let label = spec.engine.label();
    std::fs::create_dir_all(&spec.cwd).map_err(|e| format!("작업 폴더를 만들 수 없습니다: {e}"))?;
    let mut sb = Sandbox { root: normalize(&spec.cwd), read_only: spec.extra_dirs.iter().map(|d| normalize(Path::new(d))).collect(), changed: vec![] };
    let system = format!("{}{AGENT_SYSTEM}", spec.system);
    let max_tokens = spec.max_tokens.unwrap_or(WORK_MAX_TOKENS).max(WORK_MAX_TOKENS);
    // 노력치·생각 설정은 대화 모드와 같은 본문을 바탕으로 하고, 메시지와 도구만 바꾼다.
    let mut base = apis::body(a, model, &system, &spec.prompt, max_tokens, false, spec.effort.as_deref());
    base["tools"] = tools_for(a.wire);
    let mut msgs: Vec<Value> = match a.wire {
        Wire::Anthropic => vec![json!({"role": "user", "content": spec.prompt})],
        Wire::OpenAiCompat => vec![json!({"role": "system", "content": system}), json!({"role": "user", "content": spec.prompt})],
    };
    for _ in 0..MAX_TURNS {
        base["messages"] = Value::Array(cache_last(a.wire, &msgs));
        let v = call(http, a, key, label, &base, on_event).await?;
        let (text, calls): (String, Vec<Call>) = match a.wire {
            Wire::Anthropic => {
                usage.input += v["usage"]["input_tokens"].as_u64().unwrap_or(0);
                usage.cache_read += v["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write += v["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                usage.output += v["usage"]["output_tokens"].as_u64().unwrap_or(0);
                let content = v["content"].as_array().cloned().unwrap_or_default();
                let text = content.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("");
                let calls = content.iter().filter(|b| b["type"] == "tool_use").map(|b| (b["id"].as_str().unwrap_or("").to_string(), b["name"].as_str().unwrap_or("").to_string(), Ok(b["input"].clone()))).collect();
                // 생각 블록까지 받은 그대로 돌려줘야 다음 차례가 이어진다.
                msgs.push(json!({"role": "assistant", "content": content}));
                if v["stop_reason"] == "refusal" {
                    return Err("모델이 안전 정책에 따라 답을 거절했습니다".into());
                }
                (text, calls)
            }
            Wire::OpenAiCompat => {
                // 캐시에서 읽은 입력은 따로 센다(prompt_tokens에 포함돼 있다)
                let cached = v["usage"]["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
                usage.cache_read += cached;
                usage.input += v["usage"]["prompt_tokens"].as_u64().unwrap_or(0).saturating_sub(cached);
                usage.output += v["usage"]["completion_tokens"].as_u64().unwrap_or(0);
                let mut m = v["choices"][0]["message"].clone();
                if !m.is_object() {
                    return Err(format!("{label} 응답에 메시지가 없습니다"));
                }
                m["role"] = json!("assistant");
                let text = m["content"].as_str().unwrap_or("").to_string();
                let calls = m["tool_calls"]
                    .as_array()
                    .map(|list| {
                        list.iter()
                            .map(|c| {
                                let args = c["function"]["arguments"].as_str().unwrap_or("{}");
                                let input = serde_json::from_str::<Value>(if args.trim().is_empty() { "{}" } else { args }).map_err(|e| format!("도구 입력(JSON)을 읽을 수 없습니다: {e}"));
                                (c["id"].as_str().unwrap_or("").to_string(), c["function"]["name"].as_str().unwrap_or("").to_string(), input)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                msgs.push(m);
                (text, calls)
            }
        };
        if calls.is_empty() {
            // 답을 마치는 순간 온 메시지가 있으면 받아서 한 차례 더 이어간다.
            let extra = take_steer(spec);
            if !extra.is_empty() {
                on_event(&Event::Activity("작업 중 보낸 메시지를 반영하는 중".into()));
                push_steer(a.wire, &mut msgs, &extra);
                continue;
            }
            let text = if text.trim().is_empty() {
                if sb.changed.is_empty() { "작업을 마쳤습니다.".to_string() } else { format!("작업을 마쳤습니다. 바꾼 파일:\n{}", sb.changed.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n")) }
            } else {
                text
            };
            on_event(&Event::Text(text.clone()));
            return Ok(text);
        }
        if !text.trim().is_empty() {
            let t: String = text.trim().replace('\n', " ").chars().take(120).collect();
            on_event(&Event::Activity(t));
        }
        let mut results = vec![];
        for (id, name, input) in calls {
            let act = activity(&name, input.as_ref().unwrap_or(&Value::Null));
            on_event(&Event::Activity(act.clone()));
            out.activities.push(act);
            let r = match input {
                Ok(i) => exec(&mut sb, &name, &i).await,
                Err(e) => Err(e),
            };
            let (content, is_error) = match r {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            match a.wire {
                Wire::Anthropic => results.push(json!({"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error})),
                Wire::OpenAiCompat => msgs.push(json!({"role": "tool", "tool_call_id": id, "content": if is_error { format!("오류: {content}") } else { content }})),
            }
        }
        if a.wire == Wire::Anthropic {
            msgs.push(json!({"role": "user", "content": results}));
        }
        // 도구를 쓰는 동안 사용자가 보낸 메시지는 이번 결과와 함께 다음 차례에 넘긴다.
        let extra = take_steer(spec);
        if !extra.is_empty() {
            on_event(&Event::Activity(format!("작업 중 보낸 메시지 {}개 반영", extra.len())));
            push_steer(a.wire, &mut msgs, &extra);
        }
    }
    Err(format!("도구를 {MAX_TURNS}차례 넘게 불러 멈췄습니다. 지금까지 바꾼 파일: {}", if sb.changed.is_empty() { "없음".into() } else { sb.changed.join(", ") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(root: &Path) -> Sandbox {
        Sandbox { root: normalize(root), read_only: vec![], changed: vec![] }
    }

    #[test]
    fn paths_stay_inside_the_workspace() {
        let t = tempfile::tempdir().unwrap();
        let sb = sandbox(t.path());
        assert!(sb.resolve("src/a.txt", true).is_ok());
        assert!(sb.resolve("./x/../b.txt", true).is_ok());
        assert!(sb.resolve("../escape.txt", true).is_err());
        assert!(sb.resolve("C:\\Windows\\win.ini", false).is_err());
        let inside = t.path().join("c.txt");
        assert!(sb.resolve(&inside.to_string_lossy().to_uppercase(), true).is_ok(), "대소문자 무시");
    }

    #[tokio::test]
    async fn file_tools_create_edit_find_and_delete() {
        let t = tempfile::tempdir().unwrap();
        let mut sb = sandbox(t.path());
        exec(&mut sb, "write_file", &json!({"path": "app/main.py", "content": "print('hi')\nprint('hi')\n"})).await.unwrap();
        assert!(exec(&mut sb, "edit_file", &json!({"path": "app/main.py", "old_text": "hi", "new_text": "yo"})).await.is_err(), "두 곳이면 거절");
        exec(&mut sb, "edit_file", &json!({"path": "app/main.py", "old_text": "hi", "new_text": "yo", "replace_all": true})).await.unwrap();
        assert!(exec(&mut sb, "read_file", &json!({"path": "app/main.py"})).await.unwrap().contains("print('yo')"));
        assert!(exec(&mut sb, "search_text", &json!({"query": "YO"})).await.unwrap().contains("app/main.py:1:"));
        assert!(exec(&mut sb, "find_files", &json!({"name": "main"})).await.unwrap().contains("app/main.py"));
        exec(&mut sb, "move_path", &json!({"from": "app/main.py", "to": "app/run.py"})).await.unwrap();
        assert!(exec(&mut sb, "list_dir", &json!({"path": "app"})).await.unwrap().contains("run.py"));
        assert!(exec(&mut sb, "delete_path", &json!({"path": "."})).await.is_err(), "작업 폴더 자체는 지우지 않음");
        exec(&mut sb, "delete_path", &json!({"path": "app"})).await.unwrap();
        assert!(!t.path().join("app").exists());
        assert!(sb.changed.contains(&"app/main.py".to_string()));
    }

    #[test]
    fn steering_joins_the_tool_turn_and_cache_marks_only_the_copy() {
        let mut msgs = vec![json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "1", "content": "ok"}]})];
        push_steer(Wire::Anthropic, &mut msgs, &["README도 고쳐 줘".into()]);
        assert_eq!(msgs.len(), 1, "도구 결과와 같은 차례에 덧붙인다");
        assert!(msgs[0]["content"][1]["text"].as_str().is_some_and(|s| s.contains("README")));
        let marked = cache_last(Wire::Anthropic, &msgs);
        assert_eq!(marked[0]["content"][1]["cache_control"]["type"], "ephemeral");
        assert!(msgs[0]["content"][1].get("cache_control").is_none(), "원본에는 표시를 남기지 않는다");
        // 같은 차례에 계속 덧붙인다(메시지가 여러 개 와도 사용자 차례는 하나)
        push_steer(Wire::Anthropic, &mut msgs, &["그리고 테스트도".into()]);
        push_steer(Wire::OpenAiCompat, &mut msgs, &["x".into()]);
        assert_eq!(msgs.last().unwrap()["role"], "user");
        assert_eq!(tools_for(Wire::Anthropic)[8]["cache_control"]["type"], "ephemeral");
        assert!(tools_for(Wire::OpenAiCompat)[8].get("cache_control").is_none());
    }

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[test]
    fn tools_have_each_wire_shape() {
        assert_eq!(tools_for(Wire::Anthropic)[0]["input_schema"]["type"], "object");
        assert_eq!(tools_for(Wire::OpenAiCompat)[0]["function"]["parameters"]["type"], "object");
    }
}

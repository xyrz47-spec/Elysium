//! 프런트엔드에서 부르는 명령.

use crate::apis;
use crate::audit;
use crate::remote;
use crate::secrets::{self, SecretName, SecretStatus};
use crate::engines::{self, EngineId, EngineInfo};
use crate::error::{AppError, AppResult};
use crate::orchestrator::{self, AppState, SendResult};
use crate::settings::{self, Settings};
use crate::skills::{self, Plugin, Skill};
use crate::store::{self, Approval, Conversation, Message, UsageToday};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;

type S<'a> = State<'a, Arc<AppState>>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    data_dir: String,
    workspace: String,
}

#[tauri::command]
pub fn app_info(app: AppHandle, st: S) -> AppInfo {
    AppInfo {
        version: app.package_info().version.to_string(),
        data_dir: st.data_dir.to_string_lossy().to_string(),
        workspace: st.settings().workspace().to_string_lossy().to_string(),
    }
}

#[tauri::command]
pub fn settings_get(st: S) -> Settings {
    st.settings()
}

#[tauri::command]
pub fn settings_update(app: AppHandle, st: S, settings: Settings) -> AppResult<Settings> {
    let s = settings.sanitized();
    settings::save(&st.db, &s)?;
    *st.settings.write().unwrap_or_else(|p| p.into_inner()) = s.clone();
    // API 엔진에 보여 주는 모델 이름을 바로 맞춘다.
    for e in st.engines.write().unwrap_or_else(|p| p.into_inner()).iter_mut().filter(|e| e.id.is_api()) {
        e.version = Some(s.models.get(e.id.as_str()).cloned().unwrap_or_else(|| apis::api(e.id).map(|a| a.default_model.to_string()).unwrap_or_default()));
    }
    audit::record(&st.db, "user", "settings.updated", "ok", json!({ "review": s.review, "customWorkspace": s.workspace_dir.is_some(), "jev": s.jev_enabled }));
    let _ = app.emit("settings:changed", ());
    Ok(s)
}

#[tauri::command]
pub fn engines_list(st: S) -> Vec<EngineInfo> {
    st.engines.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// CLI를 새로 설치했을 때 다시 찾는다.
#[tauri::command]
pub async fn engines_refresh(st: S<'_>) -> AppResult<Vec<EngineInfo>> {
    let found = engines::detect(&st.settings().models).await;
    *st.engines.write().unwrap_or_else(|p| p.into_inner()) = found.clone();
    Ok(found)
}

#[tauri::command]
pub fn secrets_status() -> Vec<SecretStatus> {
    secrets::status()
}

/// 키 형식 점검(값은 돌려주지 않음)
#[tauri::command]
pub fn secret_shape(name: String) -> AppResult<secrets::SecretShape> {
    Ok(secrets::shape(SecretName::parse(&name)?))
}

/// 키를 저장하고 API 엔진 목록(키 있음 여부)을 다시 만든다. 값은 화면으로 돌려주지 않는다.
#[tauri::command]
pub async fn secret_set(app: AppHandle, st: S<'_>, name: String, value: String) -> AppResult<Vec<EngineInfo>> {
    let which = SecretName::parse(&name)?;
    secrets::set(which, &value)?;
    audit::record(&st.db, "user", "secret.set", "ok", json!({ "name": name }));
    // TypeSafe 키를 넣었다는 것은 JEV를 쓰겠다는 뜻이다: 따로 켜지 않아도 바로 쓰게 한다.
    if which == SecretName::TypeSafe && !st.settings().jev_enabled {
        let s = Settings { jev_enabled: true, ..st.settings() };
        settings::save(&st.db, &s)?;
        *st.settings.write().unwrap_or_else(|p| p.into_inner()) = s;
        audit::record(&st.db, "user", "settings.updated", "ok", json!({ "jev": true, "by": "secret.set" }));
        let _ = app.emit("settings:changed", ());
    }
    engines_refresh(st).await
}

#[tauri::command]
pub async fn secret_clear(st: S<'_>, name: String) -> AppResult<Vec<EngineInfo>> {
    secrets::clear(SecretName::parse(&name)?)?;
    audit::record(&st.db, "user", "secret.cleared", "ok", json!({ "name": name }));
    engines_refresh(st).await
}

/// API 키·모델 확인(1토큰 요청 1회).
#[tauri::command]
pub async fn api_test(st: S<'_>, engine: String) -> AppResult<String> {
    let id = EngineId::parse(&engine).filter(|e| e.is_api()).ok_or_else(|| AppError::msg("API 엔진이 아닙니다"))?;
    let model = st.settings().models.get(id.as_str()).cloned().or_else(|| apis::api(id).map(|a| a.default_model.to_string())).unwrap_or_default();
    apis::test(&st.http, id, &model).await.map_err(AppError::msg)
}

/// JEV 확인: 예시 요청 하나를 판단해 본다.
#[tauri::command]
pub async fn jev_test(st: S<'_>) -> AppResult<orchestrator::Judgment> {
    let key = secrets::get(SecretName::TypeSafe).ok_or_else(|| AppError::msg("TypeSafe API 키가 없습니다"))?;
    orchestrator::judge(&st.http, &key, "파이썬으로 CSV 파일을 읽어 월별 매출 합계를 내는 스크립트를 만들어 줘").await.map_err(AppError::msg)
}

#[tauri::command]
pub fn conversation_list(st: S) -> AppResult<Vec<Conversation>> {
    store::list_conversations(&st.db)
}

#[tauri::command]
pub fn conversation_messages(st: S, conversation_id: String) -> AppResult<Vec<Message>> {
    store::list_messages(&st.db, &conversation_id)
}

#[tauri::command]
pub fn conversation_rename(st: S, conversation_id: String, title: String) -> AppResult<()> {
    store::rename_conversation(&st.db, &conversation_id, &title)
}

#[tauri::command]
pub fn conversation_delete(st: S, conversation_id: String) -> AppResult<()> {
    store::delete_conversation(&st.db, &conversation_id)?;
    audit::record(&st.db, "user", "conversation.deleted", "ok", json!({}));
    Ok(())
}

/// engine: "auto"(또는 없음) | "claude" | "codex" | "antigravity". work: 파일 수정 허용(승인 후).
/// build: '만들기' 버튼. 누른 것이 곧 허락이라 승인 없이 폴더 안에서 끝까지 완성한다.
/// council: '회의' 버튼. 여러 AI가 라운드마다 서로의 발언을 읽고 대화하며 푼다(대화 모드).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn chat_send(app: AppHandle, st: S, conversation_id: Option<String>, text: String, engine: Option<String>, work: Option<bool>, build: Option<bool>, workspace: Option<String>, council: Option<bool>) -> AppResult<SendResult> {
    let choice = engine.as_deref().filter(|e| *e != "auto").map(|e| EngineId::parse(e).ok_or_else(|| AppError::msg(format!("알 수 없는 엔진: {e}")))).transpose()?;
    let workspace = workspace.filter(|w| !w.trim().is_empty()).map(|w| checked_dir(&w)).transpose()?;
    orchestrator::send(&app, st.inner(), conversation_id, text, choice, work.unwrap_or(false), build.unwrap_or(false), workspace, council.unwrap_or(false)).map_err(AppError::msg)
}

/// 있는 폴더만 작업 폴더로 쓸 수 있다.
fn checked_dir(path: &str) -> AppResult<String> {
    let p = std::path::Path::new(path.trim());
    if !p.is_absolute() || !p.is_dir() {
        return Err(AppError::Policy(format!("폴더가 없습니다: {}", p.display())));
    }
    Ok(p.to_string_lossy().to_string())
}

/// 대화의 작업 폴더를 바꾼다(None이면 기본 작업 폴더로). 이어 쓰던 CLI 세션은 끊긴다.
#[tauri::command]
pub fn conversation_set_workspace(st: S, conversation_id: String, path: Option<String>) -> AppResult<()> {
    let path = path.filter(|p| !p.trim().is_empty()).map(|p| checked_dir(&p)).transpose()?;
    store::set_workspace(&st.db, &conversation_id, path.as_deref())?;
    audit::record(&st.db, "user", "conversation.workspace", "ok", json!({ "custom": path.is_some() }));
    Ok(())
}

/// Windows 폴더 선택 창을 연다. 취소하면 None.
#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().set_title("작업 폴더 고르기").blocking_pick_folder()).await.ok().flatten()?;
    picked.into_path().ok().map(|p| p.to_string_lossy().to_string())
}

#[tauri::command]
pub fn task_cancel(st: S, task_id: String) -> bool {
    match st.tasks.lock().unwrap_or_else(|p| p.into_inner()).get(&task_id) {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
    }
}

/// 실행 중인 에이전트 하나만 멈춘다(같은 작업의 다른 에이전트는 계속 돈다).
#[tauri::command]
pub fn agent_cancel(st: S, task_id: String, key: String) -> bool {
    orchestrator::cancel_agent(&st, &task_id, &key)
}

/// 작업 중에 보내는 메시지. API 작업 모드가 다음 차례에 읽는다. 받지 않는 AI면 오류를 돌려준다.
#[tauri::command]
pub fn task_steer(app: AppHandle, st: S, task_id: String, text: String) -> AppResult<()> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::msg("보낼 내용이 없습니다"));
    }
    let steer = st.steers.lock().unwrap_or_else(|p| p.into_inner()).get(&task_id).cloned().ok_or_else(|| AppError::msg("이미 끝난 작업입니다"))?;
    let mut slot = steer.lock().unwrap_or_else(|p| p.into_inner());
    match slot.as_mut() {
        Some(v) => v.push(text.clone()),
        None => return Err(AppError::msg("지금 AI는 작업 중 메시지를 받지 않습니다(API 작업 모드만 받습니다)")),
    }
    drop(slot);
    orchestrator::emit(&app, "task:activity", json!({ "taskId": task_id, "text": format!("메시지 전달: {}", text.chars().take(60).collect::<String>()) }));
    Ok(())
}

#[tauri::command]
pub fn cancel_all(st: S) -> usize {
    let tasks = st.tasks.lock().unwrap_or_else(|p| p.into_inner());
    tasks.values().for_each(|t| t.cancel());
    tasks.len()
}

#[tauri::command]
pub fn approval_list(st: S) -> AppResult<Vec<Approval>> {
    store::pending_approvals(&st.db)
}

#[tauri::command]
pub fn approval_decide(app: AppHandle, st: S, approval_id: String, approve: bool) -> AppResult<Approval> {
    orchestrator::decide_approval(&app, &st, &approval_id, approve, "user")
}

// ---------------------------------------------------------------------------
// 세션 메모리(이 세션의 모든 AI가 같이 쓰는 요약)

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryView {
    summary: Option<String>,
    upto: Option<String>,
    messages: usize,
}

#[tauri::command]
pub fn memory_get(st: S, conversation_id: String) -> AppResult<MemoryView> {
    let (summary, upto) = store::get_memory(&st.db, &conversation_id)?;
    Ok(MemoryView { summary, upto, messages: store::list_messages(&st.db, &conversation_id)?.len() })
}

/// 요약을 지운다(대화 기록은 그대로, 다음에 오래된 대화가 쌓이면 다시 접는다).
#[tauri::command]
pub fn memory_clear(st: S, conversation_id: String) -> AppResult<MemoryView> {
    store::set_memory(&st.db, &conversation_id, None, None)?;
    audit::record(&st.db, "user", "memory.cleared", "ok", json!({}));
    memory_get(st, conversation_id)
}

// ---------------------------------------------------------------------------
// 원격 제어(폰 앱)

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    config: remote::RemoteConfig,
    running: Option<u16>,
    addresses: Vec<remote::Address>,
    fingerprint: Option<String>,
    devices: Vec<remote::Device>,
}

fn remote_status_of(st: &orchestrator::AppState) -> AppResult<RemoteStatus> {
    let config = remote::load_config(&st.db)?;
    let fingerprint = if config.enabled { remote::identity(&st.data_dir).ok().map(|i| i.fingerprint) } else { None };
    Ok(RemoteStatus { config, running: remote::running_port(), addresses: remote::addresses(), fingerprint, devices: remote::devices(&st.db)? })
}

#[tauri::command]
pub fn remote_status(st: S) -> AppResult<RemoteStatus> {
    remote_status_of(&st)
}

/// 원격 제어 켜기/끄기·폰 허락 허용. 켜면 서버를 띄우고, 끄면 닫고 페어링 창도 닫는다.
#[tauri::command]
pub async fn remote_configure(app: AppHandle, st: S<'_>, enabled: bool, allow_approve: bool) -> AppResult<RemoteStatus> {
    let mut c = remote::load_config(&st.db)?;
    c.enabled = enabled;
    c.allow_approve = allow_approve;
    remote::save_config(&st.db, &c)?;
    if enabled {
        if remote::running_port().is_none() {
            remote::start(app, st.inner().clone()).await?;
        }
    } else {
        remote::stop();
        remote::pair_cancel();
        audit::record(&st.db, "user", "remote.stopped", "ok", json!({}));
    }
    remote_status_of(&st)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInfo {
    link: String,
    qr_svg: String,
    expires_in: u64,
}

/// 페어링 QR을 띄운다(5분, 한 번만).
#[tauri::command]
pub fn remote_pair_start(st: S) -> AppResult<PairInfo> {
    let port = remote::running_port().ok_or_else(|| AppError::msg("먼저 원격 제어를 켜세요"))?;
    let id = remote::identity(&st.data_dir)?;
    let (code, ttl) = remote::pair_start();
    let link = remote::pair_link(&remote::addresses(), port, &id.fingerprint, &code);
    audit::record(&st.db, "user", "remote.pair_opened", "ok", json!({}));
    Ok(PairInfo { qr_svg: remote::qr_svg(&link)?, link, expires_in: ttl })
}

#[tauri::command]
pub fn remote_pair_cancel() {
    remote::pair_cancel();
}

/// 기기 연결 끊기(그 기기의 토큰은 바로 막힌다)
#[tauri::command]
pub fn remote_revoke(st: S, id: String) -> AppResult<RemoteStatus> {
    remote::revoke(&st.db, &id)?;
    audit::record(&st.db, "user", "remote.revoked", "ok", json!({}));
    remote_status_of(&st)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsView {
    skills: Vec<Skill>,
    plugins: Vec<Plugin>,
    skills_dir: String,
    plugins_dir: String,
}

#[tauri::command]
pub fn skills_list(st: S) -> SkillsView {
    let c = st.catalog();
    SkillsView {
        skills: c.skills,
        plugins: c.plugins,
        skills_dir: st.data_dir.join("skills").to_string_lossy().to_string(),
        plugins_dir: st.data_dir.join("plugins").to_string_lossy().to_string(),
    }
}

#[tauri::command]
pub fn skill_toggle(st: S, id: String, enabled: bool) -> AppResult<()> {
    store::set_toggle(&st.db, &format!("skill:{id}"), enabled)
}

#[tauri::command]
pub fn plugin_toggle(st: S, id: String, enabled: bool) -> AppResult<()> {
    store::set_toggle(&st.db, &format!("plugin:{id}"), enabled)?;
    audit::record(&st.db, "user", "plugin.toggled", if enabled { "enabled" } else { "disabled" }, json!({ "plugin": id }));
    Ok(())
}

/// 새 스킬 틀을 만들고 편집기로 연다.
#[tauri::command]
pub fn skill_create(app: AppHandle, st: S, name: String) -> AppResult<String> {
    let path = skills::create_skill(&st.data_dir, &name).map_err(AppError::msg)?;
    let p = path.to_string_lossy().to_string();
    let _ = app.opener().open_path(&p, None::<&str>);
    Ok(p)
}

/// kind: "skills" | "plugins" | "workspace" | "skill:<경로>"
#[tauri::command]
pub fn open_path(app: AppHandle, st: S, kind: String) -> AppResult<()> {
    let p = match kind.as_str() {
        "skills" => st.data_dir.join("skills"),
        "plugins" => st.data_dir.join("plugins"),
        "workspace" => st.settings().workspace(),
        // 그 세션의 작업 폴더(없으면 기본 작업 폴더)
        s if s.starts_with("session:") => store::get_workspace(&st.db, &s["session:".len()..])?.map(std::path::PathBuf::from).unwrap_or_else(|| st.settings().workspace()),
        other => {
            // 스킬 파일은 데이터 폴더 안의 것만 연다.
            let path = std::path::PathBuf::from(other.strip_prefix("skill:").ok_or_else(|| AppError::msg("알 수 없는 경로"))?);
            let ok = path.canonicalize().ok().zip(st.data_dir.canonicalize().ok()).is_some_and(|(a, b)| a.starts_with(b));
            if !ok {
                return Err(AppError::Policy("데이터 폴더 밖의 파일은 열 수 없습니다".into()));
            }
            path
        }
    };
    std::fs::create_dir_all(if p.is_dir() || p.extension().is_none() { &p } else { p.parent().unwrap_or(&p) })?;
    app.opener().open_path(p.to_string_lossy(), None::<&str>).map_err(|e| AppError::msg(e.to_string()))
}

/// 세션을 마크다운 글로(복사용)
#[tauri::command]
pub fn conversation_markdown(st: S, conversation_id: String) -> AppResult<String> {
    Ok(store::conversation_markdown(&st.db, &conversation_id)?.1)
}

/// 세션을 그 세션의 작업 폴더 안 "Elysium 내보내기"에 .md로 저장하고, 탐색기에서 그 파일을 보여 준다.
#[tauri::command]
pub fn conversation_export(app: AppHandle, st: S, conversation_id: String) -> AppResult<String> {
    let (title, md) = store::conversation_markdown(&st.db, &conversation_id)?;
    let base = store::get_workspace(&st.db, &conversation_id)?.map(std::path::PathBuf::from).unwrap_or_else(|| st.settings().workspace());
    let dir = base.join("Elysium 내보내기");
    std::fs::create_dir_all(&dir)?;
    let safe: String = title.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
    let name = format!("{} {}.md", chrono::Local::now().format("%Y-%m-%d %H%M"), if safe.is_empty() { "세션".to_string() } else { safe });
    let path = dir.join(name);
    std::fs::write(&path, md)?;
    audit::record(&st.db, "user", "conversation.exported", "ok", json!({}));
    let _ = app.opener().reveal_item_in_dir(&path);
    Ok(path.to_string_lossy().to_string())
}

/// 답 속 링크를 기본 브라우저로 연다(http·https만).
#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> AppResult<()> {
    let u = url.trim();
    if !(u.starts_with("https://") || u.starts_with("http://")) || u.contains(char::is_whitespace) {
        return Err(AppError::Policy("http·https 주소만 열 수 있습니다".into()));
    }
    app.opener().open_url(u, None::<&str>).map_err(|e| AppError::msg(e.to_string()))
}

/// CLI·API·JEV별 사용량(range: today | 7d | 30d | all)
#[tauri::command]
pub fn usage_summary(st: S, range: String) -> AppResult<store::UsageSummary> {
    store::usage_summary(&st.db, &range)
}

#[tauri::command]
pub fn usage_today(st: S) -> AppResult<UsageToday> {
    store::usage_today(&st.db)

}

/// 모델별 토큰 단가표(1M 토큰당 USD)
#[tauri::command]
pub fn pricing_list() -> Vec<crate::pricing::ModelPrice> { crate::pricing::all_prices() } /*
    crate::pricing::all_prices()
}



}

#[tauri::command]
*/ #[tauri::command] pub fn data_delete_all(app: AppHandle, st: S, confirm: String) -> AppResult<()> {
    if confirm != "DELETE" {
        return Err(AppError::Policy("확인 문구가 맞지 않습니다".into()));
    }
    store::delete_all(&st.db)?;
    let _ = app.emit("data:cleared", ());
    Ok(())
}

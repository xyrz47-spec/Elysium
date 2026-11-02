//! 프런트엔드에서 부르는 명령.

use crate::apis;
use crate::attachments;
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
    let files: Vec<attachments::Attachment> = store::list_messages(&st.db, &conversation_id)?.iter().flat_map(|m| attachments::of_data(m.data.as_ref())).collect();
    store::delete_conversation(&st.db, &conversation_id)?;
    for f in &files {
        attachments::remove(&st.data_dir, &f.id);
    }
    audit::record(&st.db, "user", "conversation.deleted", "ok", json!({}));
    Ok(())
}

/// engine: "auto"(또는 없음) | "claude" | "codex" | "antigravity". work: 파일 수정 허용(승인 후).
/// build: '만들기' 버튼. 누른 것이 곧 허락이라 승인 없이 폴더 안에서 끝까지 완성한다.
/// council: '회의' 버튼. 여러 AI가 라운드마다 서로의 발언을 읽고 대화하며 푼다(대화 모드).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn chat_send(app: AppHandle, st: S, conversation_id: Option<String>, text: String, engine: Option<String>, work: Option<bool>, build: Option<bool>, workspace: Option<String>, council: Option<bool>, attachments: Option<Vec<String>>) -> AppResult<SendResult> {
    let choice = engine.as_deref().filter(|e| *e != "auto").map(|e| EngineId::parse(e).ok_or_else(|| AppError::msg(format!("알 수 없는 엔진: {e}")))).transpose()?;
    let workspace = workspace.filter(|w| !w.trim().is_empty()).map(|w| checked_dir(&w)).transpose()?;
    orchestrator::send(&app, st.inner(), conversation_id, text, choice, work.unwrap_or(false), build.unwrap_or(false), workspace, council.unwrap_or(false), attachments.unwrap_or_default()).map_err(AppError::msg)
}

// ---------------------------------------------------------------------------
// 첨부 파일

fn attachment_of(st: &AppState, id: &str) -> AppResult<attachments::Attachment> {
    attachments::load(&st.data_dir, id).ok_or_else(|| AppError::msg("파일을 찾을 수 없습니다."))
}

fn add_many(st: &AppState, paths: Vec<std::path::PathBuf>) -> AppResult<Vec<attachments::Attachment>> {
    let mut out = vec![];
    let mut first_error = None;
    for p in paths {
        match attachments::add_path(&st.data_dir, &p, "user") {
            Ok(a) => out.push(a),
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    match (out.is_empty(), first_error) {
        (true, Some(e)) => Err(AppError::msg(e)),
        _ => Ok(out),
    }
}

/// 파일 선택 창을 열어 고른 파일을 첨부로 올린다. 취소하면 빈 목록.
#[tauri::command]
pub async fn attachment_pick(app: AppHandle, st: S<'_>) -> AppResult<Vec<attachments::Attachment>> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().set_title("첨부할 파일 선택").blocking_pick_files()).await.ok().flatten().unwrap_or_default();
    add_many(&st, picked.into_iter().filter_map(|p| p.into_path().ok()).collect())
}

/// 창에 끌어다 놓은 파일을 첨부로 올린다.
#[tauri::command]
pub fn attachment_add_paths(st: S, paths: Vec<String>) -> AppResult<Vec<attachments::Attachment>> {
    add_many(&st, paths.into_iter().map(std::path::PathBuf::from).collect())
}

/// 붙여 넣은 이미지처럼 경로가 없는 내용을 첨부로 올린다.
#[tauri::command]
pub fn attachment_add_bytes(st: S, name: String, data_base64: String) -> AppResult<attachments::Attachment> {
    let bytes = attachments::b64_decode(&data_base64).ok_or_else(|| AppError::msg("invalid base64"))?;
    attachments::add_bytes(&st.data_dir, &name, &bytes, "user").map_err(AppError::msg)
}

/// 보내기 전에 입력창에서 뺀 첨부를 지운다.
#[tauri::command]
pub fn attachment_remove(st: S, id: String) {
    attachments::remove(&st.data_dir, &id);
}

#[tauri::command]
pub fn attachment_preview(st: S, id: String) -> AppResult<Option<String>> {
    Ok(attachments::preview_url(&st.data_dir, &attachment_of(&st, &id)?))
}

/// 기본 프로그램으로 연다. 실행되는 형식은 열지 않고 탐색기에서 보여 준다(true를 돌려준다).
#[tauri::command]
pub fn attachment_open(app: AppHandle, st: S, id: String) -> AppResult<bool> {
    let a = attachment_of(&st, &id)?;
    let path = attachments::path_of(&st.data_dir, &a);
    if attachments::risky_to_open(&a.name) || attachments::risky_to_open(&a.file) {
        app.opener().reveal_item_in_dir(&path).map_err(|e| AppError::msg(e.to_string()))?;
        return Ok(true);
    }
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| AppError::msg(e.to_string()))?;
    Ok(false)
}

#[tauri::command]
pub fn attachment_reveal(app: AppHandle, st: S, id: String) -> AppResult<()> {
    let a = attachment_of(&st, &id)?;
    app.opener().reveal_item_in_dir(attachments::path_of(&st.data_dir, &a)).map_err(|e| AppError::msg(e.to_string()))
}

/// 저장 위치를 물어 복사한다. 취소하면 None.
#[tauri::command]
pub async fn attachment_save_as(app: AppHandle, st: S<'_>, id: String) -> AppResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let a = attachment_of(&st, &id)?;
    let src = attachments::path_of(&st.data_dir, &a);
    let name = a.name.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || app.dialog().file().set_file_name(&name).blocking_save_file()).await.ok().flatten();
    let Some(dest) = picked.and_then(|p| p.into_path().ok()) else { return Ok(None) };
    std::fs::copy(&src, &dest)?;
    Ok(Some(dest.to_string_lossy().to_string()))
}

/// 입력하는 동안 JEV가 추천하는 모드·스킬. 꺼져 있으면 None.
#[tauri::command]
pub async fn jev_suggest(st: S<'_>, text: String, has_files: bool) -> AppResult<Option<orchestrator::Suggestion>> {
    let s = st.settings();
    let Some(key) = secrets::get(SecretName::TypeSafe).filter(|_| s.jev_enabled && s.jev_suggest) else { return Ok(None) };
    let catalog = st.catalog();
    let skills: Vec<&Skill> = catalog.enabled_skills().collect();
    let r = orchestrator::suggest(&st.http, &key, text.trim(), &skills, has_files).await.map_err(AppError::msg)?;
    let _ = store::record_usage(&st.db, &store::Usage { engine: "jev", role: "suggest", ok: true, latency_ms: r.ms, input_tokens: None, output_tokens: None, cost_usd: Some(r.cost_usd) });
    Ok(Some(r))
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

/// 실행 중인 에이전트 하나에게 말을 건다. API 작업 모드는 다음 차례에 읽고, 나머지는 잠시 끊고 메시지를 반영해 이어 간다.
#[tauri::command]
pub fn agent_message(app: AppHandle, st: S, task_id: String, key: String, text: String) -> AppResult<()> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::msg("보낼 내용이 없습니다"));
    }
    if !orchestrator::message_agent(&app, &st, &task_id, &key, &text) {
        return Err(AppError::msg("이미 끝난 작업입니다"));
    }
    Ok(())
}

/// 입력창에서 작업 중에 보내는 메시지: 지금 실행 중인 에이전트 모두에게 전한다.
#[tauri::command]
pub fn task_steer(app: AppHandle, st: S, task_id: String, text: String) -> AppResult<()> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::msg("보낼 내용이 없습니다"));
    }
    if !st.tasks.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&task_id) {
        return Err(AppError::msg("이미 끝난 작업입니다"));
    }
    if orchestrator::message_task(&app, &st, &task_id, &text) == 0 {
        return Err(AppError::msg("에이전트가 시작된 후 다시 보내 주십시오"));
    }
    Ok(())
}

/// 곁질문: 대화 기록·세션 메모리·CLI 세션을 건드리지 않고 지금 진행 상황이나 프로젝트에 대해 묻는다. 답은 side:* 이벤트로 온다.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn side_ask(app: AppHandle, st: S, side_id: String, conversation_id: Option<String>, workspace: Option<String>, question: String, progress: Option<String>, thread: Vec<orchestrator::SideTurn>) -> AppResult<()> {
    let workspace = workspace.filter(|w| !w.trim().is_empty()).map(|w| checked_dir(&w)).transpose()?;
    orchestrator::side_ask(&app, st.inner(), side_id, conversation_id, workspace, question, progress, thread).map_err(AppError::msg)
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
    attachments::remove_all(&st.data_dir);
    let _ = app.emit("data:cleared", ());
    Ok(())
}

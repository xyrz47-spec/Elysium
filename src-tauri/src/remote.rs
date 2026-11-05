//! 원격 제어: 폰 앱(Elysium Remote)이 이 PC의 엘리시움으로 세션을 보고, 질문하고, 멈추고, 허락한다.
//!
//! 보안
//! - 기본은 꺼짐. 켜면 모든 네트워크 주소(같은 Wi-Fi, Tailscale, 포트 포워딩)에서 TLS로만 받는다.
//! - 다른 네트워크의 폰은 iroh 터널(relay.rs)로 들어온다. 터널 안에서도 같은 TLS·토큰을 쓴다.
//! - 인증서는 이 PC가 처음 켤 때 만든 자체 서명 인증서다. 폰은 페어링 QR에 담긴 SHA-256 지문과 같은 인증서만 믿는다.
//! - 페어링 코드는 PC에서 연 지 5분 동안 한 번만 쓸 수 있고, 여러 번 틀리면 닫힌다.
//! - 기기마다 무작위 토큰(256비트)을 주고, DB에는 해시만 둔다. 설정에서 기기를 끊으면 바로 막힌다.

use crate::audit;
use crate::db::{now, Db};
use crate::error::{AppError, AppResult};
use crate::orchestrator::{self, AppState};
use crate::store;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{request::Parts, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, SocketAddr};
use std::path::Path as FsPath;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::broadcast;

pub const DEFAULT_PORT: u16 = 47820;
const PAIR_TTL: Duration = Duration::from_secs(300);
const PAIR_MAX_FAILURES: u32 = 8;

// ---------------------------------------------------------------------------
// 설정(일반 설정과 따로 둔다: 화면의 설정 저장이 원격 설정을 덮어쓰지 않도록)

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RemoteConfig {
    pub enabled: bool,
    pub port: u16,
    /// 폰에서 작업 모드 허락까지 할 수 있는지
    pub allow_approve: bool,
    /// 다른 네트워크에서도 연결(iroh 터널, 포트 포워딩 불필요)
    pub relay: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        RemoteConfig { enabled: false, port: DEFAULT_PORT, allow_approve: true, relay: true }
    }
}

pub fn load_config(db: &Db) -> AppResult<RemoteConfig> {
    let raw: Option<String> = db.with(|c| {
        c.query_row("SELECT value FROM settings WHERE key = 'remote'", [], |r| r.get(0))
            .map(Some)
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
    })?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default())
}

pub fn save_config(db: &Db, c: &RemoteConfig) -> AppResult<()> {
    let json = serde_json::to_string(c)?;
    db.with(|x| x.execute("INSERT INTO settings(key, value) VALUES ('remote', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [json]))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 이벤트: 화면으로 보내는 이벤트를 연결된 폰에도 그대로 흘린다.

static BUS: OnceLock<broadcast::Sender<String>> = OnceLock::new();

fn bus() -> &'static broadcast::Sender<String> {
    BUS.get_or_init(|| broadcast::channel(2048).0)
}

/// 돌고 있는 작업의 에이전트 상태(폰이 늦게 붙어도 한 번에 받는다)
fn snap() -> &'static Mutex<crate::livesnap::Snap> {
    static SNAP: OnceLock<Mutex<crate::livesnap::Snap>> = OnceLock::new();
    SNAP.get_or_init(Default::default)
}

pub fn publish(name: &str, payload: &Value) {
    if name.starts_with("task:") || name == "lead:started" {
        snap().lock().unwrap_or_else(|p| p.into_inner()).apply(name, payload);
    }
    let tx = bus();
    if tx.receiver_count() > 0 {
        let _ = tx.send(json!({ "event": name, "payload": payload }).to_string());
    }
}

// ---------------------------------------------------------------------------
// 인증서(자체 서명, 처음 켤 때 한 번 만든다)

pub struct Identity {
    cert_pem: String,
    key_pem: String,
    pub fingerprint: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn identity(data_dir: &FsPath) -> AppResult<Identity> {
    use rustls::pki_types::{pem::PemObject, CertificateDer};
    let dir = data_dir.join("remote");
    let (cp, kp) = (dir.join("cert.pem"), dir.join("key.pem"));
    if !(cp.exists() && kp.exists()) {
        std::fs::create_dir_all(&dir)?;
        let c = rcgen::generate_simple_self_signed(vec!["elysium.local".to_string()]).map_err(|e| AppError::msg(format!("인증서를 만들 수 없습니다: {e}")))?;
        std::fs::write(&cp, c.cert.pem())?;
        std::fs::write(&kp, c.key_pair.serialize_pem())?;
    }
    let cert_pem = std::fs::read_to_string(&cp)?;
    let key_pem = std::fs::read_to_string(&kp)?;
    let der = CertificateDer::from_pem_slice(cert_pem.as_bytes()).map_err(|e| AppError::msg(format!("인증서를 읽을 수 없습니다: {e}")))?;
    Ok(Identity { fingerprint: sha256_hex(der.as_ref()), cert_pem, key_pem })
}

// ---------------------------------------------------------------------------
// 기기·페어링

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_seen_at: Option<String>,
}

pub fn devices(db: &Db) -> AppResult<Vec<Device>> {
    db.with(|c| {
        let mut st = c.prepare("SELECT id, name, created_at, last_seen_at FROM remote_devices ORDER BY created_at DESC")?;
        let rows = st.query_map([], |r| Ok(Device { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)?, last_seen_at: r.get(3)? }))?;
        rows.collect()
    })
}

pub fn revoke(db: &Db, id: &str) -> AppResult<()> {
    db.with(|c| c.execute("DELETE FROM remote_devices WHERE id = ?1", [id]))?;
    Ok(())
}

fn add_device(db: &Db, name: &str, token: &str) -> AppResult<String> {
    let id = uuid::Uuid::new_v4().to_string();
    db.with(|c| c.execute("INSERT INTO remote_devices(id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)", rusqlite::params![id, name, sha256_hex(token.as_bytes()), now()]))?;
    Ok(id)
}

fn device_for_token(db: &Db, token: &str) -> AppResult<Option<Device>> {
    let h = sha256_hex(token.as_bytes());
    let d = db.with(|c| {
        c.query_row("SELECT id, name, created_at, last_seen_at FROM remote_devices WHERE token_hash = ?1", [&h], |r| Ok(Device { id: r.get(0)?, name: r.get(1)?, created_at: r.get(2)?, last_seen_at: r.get(3)? }))
            .map(Some)
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
    })?;
    if let Some(d) = &d {
        let _ = db.with(|c| c.execute("UPDATE remote_devices SET last_seen_at = ?1 WHERE id = ?2", rusqlite::params![now(), d.id]));
    }
    Ok(d)
}

struct Pairing {
    code: String,
    until: Instant,
    failures: u32,
}

static PAIRING: Mutex<Option<Pairing>> = Mutex::new(None);

fn random_code() -> String {
    // 헷갈리는 글자(0/O, 1/l)를 뺀 32자에서 20자: 100비트
    const A: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
    (0..20).map(|_| A[rand::random::<usize>() % A.len()] as char).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 페어링 창을 연다(5분, 한 번만). 폰은 QR의 링크를 읽는다.
pub fn pair_start() -> (String, u64) {
    let code = random_code();
    *PAIRING.lock().unwrap_or_else(|p| p.into_inner()) = Some(Pairing { code: code.clone(), until: Instant::now() + PAIR_TTL, failures: 0 });
    (code, PAIR_TTL.as_secs())
}

pub fn pair_cancel() {
    *PAIRING.lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// 맞으면 창을 닫고 true. 틀리면 실패 횟수를 세고, 너무 많이 틀리면 창을 닫는다.
fn pair_check(code: &str) -> bool {
    let mut g = PAIRING.lock().unwrap_or_else(|p| p.into_inner());
    let Some(p) = g.as_mut() else { return false };
    if Instant::now() > p.until {
        *g = None;
        return false;
    }
    if same(&p.code, code.trim()) {
        *g = None;
        return true;
    }
    p.failures += 1;
    if p.failures >= PAIR_MAX_FAILURES {
        *g = None;
    }
    false
}

pub fn pc_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Address {
    pub ip: String,
    /// "lan" | "tailscale"
    pub kind: String,
    pub name: String,
}

/// 폰이 닿을 수 없는 가상 어댑터(WSL·Hyper-V·VM·Docker). 이 주소를 앞에 두면 폰이 주소마다 연결 시간 초과를 기다린다.
fn virtual_adapter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["vethernet", "virtualbox", "vmware", "hyper-v", "wsl", "docker", "loopback", "vpn"].iter().any(|v| n.contains(v))
}

/// 폰이 붙을 수 있는 이 PC의 IPv4 주소들(Tailscale 100.64.0.0/10은 따로 표시). 실제 LAN을 먼저 둔다.
pub fn addresses() -> Vec<Address> {
    let mut out: Vec<Address> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(v) if !v.is_loopback() && !v.is_link_local() && !v.is_unspecified() && !virtual_adapter(&name) => {
                let o = v.octets();
                let tailscale = o[0] == 100 && (64..128).contains(&o[1]);
                Some(Address { ip: v.to_string(), kind: if tailscale { "tailscale" } else { "lan" }.into(), name })
            }
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.ip.cmp(&b.ip)));
    out.dedup_by(|a, b| a.ip == b.ip);
    out
}

/// 페어링 링크: elysium://pair?v=1&h=주소들&p=포트&f=지문&c=코드&n=PC이름[&i=터널 공개키]
pub fn pair_link(addrs: &[Address], port: u16, fingerprint: &str, code: &str, node: Option<&str>) -> String {
    let hosts: Vec<&str> = addrs.iter().map(|a| a.ip.as_str()).collect();
    let name: String = pc_name().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let node = node.map(|n| format!("&i={n}")).unwrap_or_default();
    format!("elysium://pair?v=1&h={}&p={port}&f={fingerprint}&c={code}&n={name}{node}", hosts.join(","))
}

/// 폰에 알려 줄 터널 공개키(외부 연결을 꺼 두었으면 None)
pub fn tunnel_id(db: &Db, data_dir: &FsPath) -> Option<String> {
    load_config(db).ok().filter(|c| c.relay).and_then(|_| crate::relay::endpoint_id(data_dir).ok())
}

pub fn qr_svg(text: &str) -> AppResult<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).map_err(|e| AppError::msg(format!("QR을 만들 수 없습니다: {e}")))?;
    Ok(code.render::<svg::Color>().min_dimensions(240, 240).quiet_zone(true).dark_color(svg::Color("#111214")).light_color(svg::Color("#ffffff")).build())
}

// ---------------------------------------------------------------------------
// 서버 켜고 끄기

static RUNNING: Mutex<Option<(axum_server::Handle, u16)>> = Mutex::new(None);

pub fn running_port() -> Option<u16> {
    RUNNING.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|(_, p)| *p)
}

pub fn stop() {
    crate::relay::stop();
    if let Some((h, _)) = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).take() {
        h.graceful_shutdown(Some(Duration::from_secs(1)));
    }
}

pub async fn start(app: AppHandle, st: Arc<AppState>) -> AppResult<u16> {
    stop();
    let cfg = load_config(&st.db)?;
    let id = identity(&st.data_dir)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(id.cert_pem.into_bytes(), id.key_pem.into_bytes())
        .await
        .map_err(|e| AppError::msg(format!("TLS를 준비할 수 없습니다: {e}")))?;
    let handle = axum_server::Handle::new();
    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    let router = router(Ctx { app, st: st.clone() });
    let h2 = handle.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = axum_server::bind_rustls(addr, tls).handle(h2).serve(router.into_make_service()).await {
            eprintln!("원격 서버가 멈췄습니다: {e}");
        }
    });
    // 포트를 못 잡으면(다른 프로그램이 쓰는 중) 바로 알린다.
    match tokio::time::timeout(Duration::from_secs(3), handle.listening()).await {
        Ok(Some(a)) => {
            *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = Some((handle, a.port()));
            audit::record(&st.db, "user", "remote.started", "ok", json!({ "port": a.port() }));
            // 외부 연결이 안 돼도 같은 Wi-Fi 연결은 그대로 쓴다.
            if cfg.relay {
                if let Err(e) = crate::relay::start(&st.data_dir, a.port()).await {
                    eprintln!("{e}");
                }
            }
            Ok(a.port())
        }
        _ => {
            handle.shutdown();
            Err(AppError::msg(format!("포트 {}을(를) 열 수 없습니다. 다른 프로그램이 쓰고 있는지 확인하세요", cfg.port)))
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP API

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    st: Arc<AppState>,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        ApiError(StatusCode::BAD_REQUEST, e.to_string())
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// Authorization: Bearer <토큰>을 확인한 기기
struct Authed(Device);

impl FromRequestParts<Ctx> for Authed {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, ctx: &Ctx) -> Result<Self, Self::Rejection> {
        let token = parts.headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("").trim().to_string();
        if !token.is_empty() {
            if let Ok(Some(d)) = device_for_token(&ctx.st.db, &token) {
                return Ok(Authed(d));
            }
        }
        // 무작위 대입을 늦춘다.
        tokio::time::sleep(Duration::from_millis(400)).await;
        Err(ApiError(StatusCode::UNAUTHORIZED, "이 기기는 연결이 끊겼습니다. PC에서 다시 페어링하세요".into()))
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route("/pair", post(pair))
        .route("/api/info", get(info))
        .route("/api/engines", get(engines))
        .route("/api/conversations", get(conversations))
        .route("/api/conversations/{id}/messages", get(messages))
        .route("/api/send", post(send))
        // 폰에서 올리는 파일(base64라 원본보다 커진다)
        .route("/api/attachments", post(upload).layer(axum::extract::DefaultBodyLimit::max(72 * 1024 * 1024)))
        .route("/api/attachments/{id}", get(attachment).delete(discard))
        .route("/api/tasks/{id}/cancel", post(cancel))
        .route("/api/agents", get(agents))
        .route("/api/tasks/{id}/agents/{key}/cancel", post(agent_cancel))
        .route("/api/tasks/{id}/agents/{key}/message", post(agent_message))
        .route("/api/tasks/{id}/lead", post(lead_send))
        .route("/api/tasks/{id}/message", post(task_message))
        .route("/api/side", post(side))
        .route("/api/approvals", get(approvals))
        .route("/api/approvals/{id}", post(decide))
        .route("/api/questions", get(questions))
        .route("/api/questions/{id}", post(answer))
        .route("/api/usage", get(usage))
        .route("/api/events", get(events))
        .with_state(ctx)
}

#[derive(Deserialize)]
struct PairReq {
    code: String,
    device: String,
}

async fn pair(State(ctx): State<Ctx>, Json(req): Json<PairReq>) -> ApiResult<Value> {
    if !pair_check(&req.code) {
        tokio::time::sleep(Duration::from_millis(600)).await;
        return Err(ApiError(StatusCode::UNAUTHORIZED, "페어링 코드가 맞지 않거나 만료됐습니다. PC에서 QR을 다시 띄우세요".into()));
    }
    let name: String = req.device.trim().chars().take(40).collect();
    let name = if name.is_empty() { "Android".to_string() } else { name };
    let token = hex(&rand::random::<[u8; 32]>());
    let id = add_device(&ctx.st.db, &name, &token)?;
    audit::record(&ctx.st.db, "remote", "remote.paired", "ok", json!({ "device": name }));
    let _ = ctx.app.emit("remote:changed", ());
    Ok(Json(json!({ "token": token, "deviceId": id, "pcName": pc_name(), "version": env!("CARGO_PKG_VERSION"), "node": tunnel_id(&ctx.st.db, &ctx.st.data_dir) })))
}

async fn info(State(ctx): State<Ctx>, Authed(d): Authed) -> ApiResult<Value> {
    let cfg = load_config(&ctx.st.db)?;
    // 지금 주소도 알려 준다: PC 주소가 바뀌어도(DHCP) 폰이 다시 페어링하지 않고 따라온다.
    let hosts: Vec<String> = addresses().into_iter().map(|a| a.ip).collect();
    // 터널 공개키도 알려 준다: 예전에 페어링한 폰도 다시 페어링하지 않고 밖에서 붙을 수 있다.
    Ok(Json(json!({ "pcName": pc_name(), "version": env!("CARGO_PKG_VERSION"), "allowApprove": cfg.allow_approve, "device": d.name, "hosts": hosts, "node": tunnel_id(&ctx.st.db, &ctx.st.data_dir) })))
}

async fn engines(State(ctx): State<Ctx>, _: Authed) -> ApiResult<Value> {
    Ok(Json(json!(*ctx.st.engines.read().unwrap_or_else(|p| p.into_inner()))))
}

async fn conversations(State(ctx): State<Ctx>, _: Authed) -> ApiResult<Vec<store::Conversation>> {
    Ok(Json(store::list_conversations(&ctx.st.db)?))
}

async fn messages(State(ctx): State<Ctx>, _: Authed, Path(id): Path<String>) -> ApiResult<Vec<store::Message>> {
    Ok(Json(store::list_messages(&ctx.st.db, &id)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendReq {
    conversation_id: Option<String>,
    text: String,
    engine: Option<String>,
    #[serde(default)]
    work: bool,
    #[serde(default)]
    attachments: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadReq {
    name: String,
    data_base64: String,
}

async fn upload(State(ctx): State<Ctx>, Authed(d): Authed, Json(req): Json<UploadReq>) -> ApiResult<crate::attachments::Attachment> {
    let bytes = crate::attachments::b64_decode(&req.data_base64).ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "invalid base64".into()))?;
    let a = crate::attachments::add_bytes(&ctx.st.data_dir, &req.name, &bytes, "user").map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    audit::record(&ctx.st.db, "remote", "remote.attachment", "ok", json!({ "device": d.name, "size": a.size }));
    Ok(Json(a))
}

/// 폰에서 보는 첨부 내용(크기 한도 안에서만). 파일 경로는 보내지 않는다.
async fn attachment(State(ctx): State<Ctx>, _: Authed, Path(id): Path<String>) -> ApiResult<Value> {
    let a = crate::attachments::load(&ctx.st.data_dir, &id).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not found".into()))?;
    if a.size > crate::attachments::MAX_INLINE_BYTES {
        return Ok(Json(json!({ "attachment": a, "dataBase64": null })));
    }
    let bytes = std::fs::read(crate::attachments::path_of(&ctx.st.data_dir, &a)).map_err(|e| ApiError(StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(Json(json!({ "attachment": a, "dataBase64": crate::attachments::b64_encode(&bytes) })))
}

async fn discard(State(ctx): State<Ctx>, _: Authed, Path(id): Path<String>) -> ApiResult<Value> {
    crate::attachments::remove(&ctx.st.data_dir, &id);
    Ok(Json(json!({ "removed": true })))
}

async fn send(State(ctx): State<Ctx>, Authed(d): Authed, Json(req): Json<SendReq>) -> ApiResult<orchestrator::SendResult> {
    let choice = match req.engine.as_deref().filter(|e| *e != "auto") {
        Some(e) => Some(crate::engines::EngineId::parse(e).ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("알 수 없는 엔진: {e}")))?),
        None => None,
    };
    audit::record(&ctx.st.db, "remote", "remote.send", "ok", json!({ "device": d.name, "work": req.work }));
    // 폰에서 보낸 작업은 '만들기'가 아니라 PC·폰의 허락을 받는 작업 모드로 돈다.
    let r = orchestrator::send(&ctx.app, &ctx.st, req.conversation_id, req.text, choice, req.work, false, None, false, req.attachments).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(r))
}

async fn cancel(State(ctx): State<Ctx>, Authed(d): Authed, Path(id): Path<String>) -> ApiResult<Value> {
    let ok = match ctx.st.tasks.lock().unwrap_or_else(|p| p.into_inner()).get(&id) {
        Some(t) => {
            t.cancel();
            true
        }
        None => false,
    };
    audit::record(&ctx.st.db, "remote", "remote.cancel", if ok { "ok" } else { "missing" }, json!({ "device": d.name }));
    Ok(Json(json!({ "cancelled": ok })))
}

/// 지금 돌고 있는 작업과 그 에이전트(상태·지금 하는 일·토큰·출력 끝부분). 끝난 차례나 메시지를 기다리는 워커도 들어 있다.
async fn agents(_: Authed) -> ApiResult<Vec<crate::livesnap::TaskSnap>> {
    Ok(Json(snap().lock().unwrap_or_else(|p| p.into_inner()).list()))
}

/// 작업의 모든 에이전트에게 같은 메시지를 보낸다.
async fn task_message(State(ctx): State<Ctx>, Authed(d): Authed, Path(task): Path<String>, Json(req): Json<TextReq>) -> ApiResult<Value> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "보낼 내용이 없습니다".into()));
    }
    let n = orchestrator::message_task(&ctx.app, &ctx.st, &task, text);
    audit::record(&ctx.st.db, "remote", "remote.task.message", "ok", json!({ "device": d.name, "agents": n }));
    Ok(Json(json!({ "sent": n })))
}

#[derive(Deserialize)]
struct TextReq {
    text: String,
    #[serde(default)]
    about: Option<String>,
}

async fn agent_cancel(State(ctx): State<Ctx>, Authed(d): Authed, Path((task, key)): Path<(String, String)>) -> ApiResult<Value> {
    let ok = orchestrator::cancel_agent(&ctx.st, &task, &key);
    audit::record(&ctx.st.db, "remote", "remote.agent.cancel", if ok { "ok" } else { "missing" }, json!({ "device": d.name, "key": key }));
    Ok(Json(json!({ "cancelled": ok })))
}

async fn agent_message(State(ctx): State<Ctx>, Authed(d): Authed, Path((task, key)): Path<(String, String)>, Json(req): Json<TextReq>) -> ApiResult<Value> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "보낼 내용이 없습니다".into()));
    }
    if !orchestrator::message_agent(&ctx.app, &ctx.st, &task, &key, text) {
        return Err(ApiError(StatusCode::CONFLICT, "이미 끝난 에이전트입니다".into()));
    }
    audit::record(&ctx.st.db, "remote", "remote.agent.message", "ok", json!({ "device": d.name, "key": key }));
    Ok(Json(json!({ "sent": true })))
}

/// 총괄에게 전체 지시를 보내거나 특정 워커(about)의 문제를 알린다.
async fn lead_send(State(ctx): State<Ctx>, Authed(d): Authed, Path(task): Path<String>, Json(req): Json<TextReq>) -> ApiResult<Value> {
    let text = req.text.trim().to_string();
    if text.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "보낼 내용이 없습니다".into()));
    }
    let lead = ctx.st.leads.lock().unwrap_or_else(|p| p.into_inner()).get(&task).cloned().ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "총괄이 있는 작업이 아닙니다".into()))?;
    audit::record(&ctx.st.db, "remote", "remote.lead.send", "ok", json!({ "device": d.name, "about": req.about }));
    crate::leadrun::user_message(crate::leadrun::Ctx { app: ctx.app.clone(), st: ctx.st.clone(), lead }, text, req.about, false);
    Ok(Json(json!({ "sent": true })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SideReq {
    side_id: String,
    #[serde(default)]
    conversation_id: Option<String>,
    question: String,
    #[serde(default)]
    thread: Vec<orchestrator::SideTurn>,
}

/// 곁질문: 대화 기록에 남기지 않는 짧은 질문. 답은 side:* 이벤트로 온다(멈추기는 /api/tasks/{sideId}/cancel).
async fn side(State(ctx): State<Ctx>, Authed(d): Authed, Json(req): Json<SideReq>) -> ApiResult<Value> {
    if req.side_id.len() > 80 || !req.side_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(ApiError(StatusCode::BAD_REQUEST, "잘못된 곁질문 번호입니다".into()));
    }
    audit::record(&ctx.st.db, "remote", "side.asked", "ok", json!({ "device": d.name }));
    orchestrator::side_ask(&ctx.app, &ctx.st, req.side_id, req.conversation_id, None, req.question, None, req.thread).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "started": true })))
}

async fn approvals(State(ctx): State<Ctx>, _: Authed) -> ApiResult<Vec<store::Approval>> {
    Ok(Json(store::pending_approvals(&ctx.st.db)?))
}

#[derive(Deserialize)]
struct DecideReq {
    approve: bool,
}

async fn decide(State(ctx): State<Ctx>, Authed(d): Authed, Path(id): Path<String>, Json(req): Json<DecideReq>) -> ApiResult<store::Approval> {
    if !load_config(&ctx.st.db)?.allow_approve {
        return Err(ApiError(StatusCode::FORBIDDEN, "PC 설정에서 폰의 허락을 막아 두었습니다. PC에서 허락하세요".into()));
    }
    Ok(Json(orchestrator::decide_approval(&ctx.app, &ctx.st, &id, req.approve, &format!("remote:{}", d.name))?))
}

async fn questions(State(ctx): State<Ctx>, _: Authed) -> ApiResult<Vec<Value>> {
    Ok(Json(ctx.st.questions.list()))
}

#[derive(Deserialize)]
struct AnswerReq {
    text: String,
}

/// 총괄의 질문에 폰에서 답한다. 먼저 온 답을 쓰고, 이미 PC에서 답했으면 오류를 돌려준다.
async fn answer(State(ctx): State<Ctx>, Authed(d): Authed, Path(id): Path<String>, Json(req): Json<AnswerReq>) -> ApiResult<Value> {
    if req.text.trim().is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "답을 입력하세요".into()));
    }
    crate::leadrun::answer_question(&ctx.app, &ctx.st, &id, req.text, &format!("remote:{}", d.name)).map_err(|e| ApiError(StatusCode::CONFLICT, e))?;
    Ok(Json(json!({ "answered": true })))
}

#[derive(Deserialize)]
struct UsageQ {
    range: Option<String>,
}

async fn usage(State(ctx): State<Ctx>, _: Authed, Query(q): Query<UsageQ>) -> ApiResult<store::UsageSummary> {
    Ok(Json(store::usage_summary(&ctx.st.db, q.range.as_deref().unwrap_or("7d"))?))
}

async fn events(_: Authed, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(stream_events)
}

/// 화면 이벤트(task:*, message:created, approval:* 등)를 한 줄 JSON으로 흘려보낸다.
async fn stream_events(mut socket: WebSocket) {
    let mut rx = bus().subscribe();
    let _ = socket.send(WsMessage::Text(json!({ "event": "hello", "payload": { "pcName": pc_name() } }).to_string().into())).await;
    let mut ping = tokio::time::interval(Duration::from_secs(25));
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Ok(text) => if socket.send(WsMessage::Text(text.into())).await.is_err() { break },
                // 너무 밀리면 폰에 다시 읽으라고 알린다.
                Err(broadcast::error::RecvError::Lagged(_)) => if socket.send(WsMessage::Text(json!({ "event": "resync", "payload": {} }).to_string().into())).await.is_err() { break },
                Err(_) => break,
            },
            _ = ping.tick() => if socket.send(WsMessage::Ping(Vec::new().into())).await.is_err() { break },
            r = socket.recv() => match r {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_adapters_are_left_out() {
        for n in ["vEthernet (WSL)", "VirtualBox Host-Only Network", "VMware Network Adapter VMnet8", "Docker NAT"] {
            assert!(virtual_adapter(n), "{n}");
        }
        for n in ["Wi-Fi", "이더넷", "Ethernet 2", "Tailscale"] {
            assert!(!virtual_adapter(n), "{n}");
        }
    }

    #[test]
    fn pairing_is_one_shot_and_locks_after_failures() {
        let (code, ttl) = pair_start();
        assert_eq!((code.len(), ttl), (20, 300));
        assert!(!pair_check("wrong"));
        assert!(pair_check(&code), "맞는 코드는 한 번 통과");
        assert!(!pair_check(&code), "두 번째는 막힘");
        let (code, _) = pair_start();
        for _ in 0..PAIR_MAX_FAILURES {
            assert!(!pair_check("nope"));
        }
        assert!(!pair_check(&code), "많이 틀리면 창이 닫힘");
    }

    #[test]
    fn tokens_are_stored_hashed_and_revocable() {
        let db = Db::open_in_memory().unwrap();
        let id = add_device(&db, "Pixel", "tok-123").unwrap();
        let raw: String = db.with(|c| c.query_row("SELECT token_hash FROM remote_devices", [], |r| r.get(0))).unwrap();
        assert_ne!(raw, "tok-123");
        assert_eq!(device_for_token(&db, "tok-123").unwrap().map(|d| d.name), Some("Pixel".into()));
        assert!(device_for_token(&db, "tok-124").unwrap().is_none());
        revoke(&db, &id).unwrap();
        assert!(device_for_token(&db, "tok-123").unwrap().is_none());
    }

    #[test]
    fn identity_is_stable_and_link_has_everything() {
        let dir = tempfile::tempdir().unwrap();
        let a = identity(dir.path()).unwrap();
        let b = identity(dir.path()).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint, "두 번째부터는 같은 인증서");
        assert_eq!(a.fingerprint.len(), 64);
        let link = pair_link(&[Address { ip: "192.168.0.5".into(), kind: "lan".into(), name: "Wi-Fi".into() }], 47820, &a.fingerprint, "abc", Some("ab12"));
        assert!(link.starts_with("elysium://pair?v=1&h=192.168.0.5&p=47820&f=") && link.contains("&c=abc&n=") && link.ends_with("&i=ab12"));
        assert!(qr_svg(&link).unwrap().contains("<svg"));
    }

    #[test]
    fn config_defaults_off() {
        let db = Db::open_in_memory().unwrap();
        let c = load_config(&db).unwrap();
        assert!(!c.enabled && c.allow_approve && c.relay && c.port == DEFAULT_PORT);
    }
}

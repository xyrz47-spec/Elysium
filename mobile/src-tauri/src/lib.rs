//! Elysium Remote(폰 앱)의 Rust 쪽: PC의 엘리시움과 TLS로만 이야기한다.
//!
//! - 페어링 링크(QR)에 담긴 SHA-256 지문과 같은 인증서만 믿는다(인증 기관·이름 검사는 하지 않음).
//! - 받은 기기 토큰은 앱 전용 저장소(다른 앱은 못 읽음)에 둔다. 화면(JS)에는 토큰을 돌려주지 않는다.
//! - PC 이벤트는 WebSocket으로 받아 "remote:event"로 화면에 넘기고, 끊기면 다시 붙는다.

mod link;
mod pin;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    hosts: Vec<String>,
    port: u16,
    fingerprint: String,
    token: String,
    pc_name: String,
}

/// 화면에 보여 주는 연결 정보(토큰은 뺀다)
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionView {
    pc_name: String,
    hosts: Vec<String>,
    port: u16,
    fingerprint: String,
}

#[derive(Default)]
struct Inner {
    saved: Option<Saved>,
    /// 마지막으로 붙은 주소(먼저 시도한다)
    good: Option<String>,
    events: Option<tauri::async_runtime::JoinHandle<()>>,
}

struct Remote(Mutex<Inner>);

fn file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("connection.json"))
}

fn client(fp: &str) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .use_preconfigured_tls(pin::config(fp))
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

fn order(saved: &Saved, good: Option<&String>) -> Vec<String> {
    let mut v: Vec<String> = good.into_iter().cloned().collect();
    v.extend(saved.hosts.iter().filter(|h| Some(*h) != good).cloned());
    v
}

fn api_error(code: u16, body: &str) -> String {
    serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| format!("PC가 요청을 거절했습니다({code})"))
}

/// 페어링: 링크의 주소를 차례로 시도해 코드를 내고 토큰을 받는다.
#[tauri::command]
async fn pair(app: AppHandle, st: State<'_, Remote>, link: String, device: String) -> Result<ConnectionView, String> {
    let l = link::parse(&link)?;
    let http = client(&l.fingerprint)?;
    let mut last = "PC에 연결할 수 없습니다. 같은 Wi-Fi(또는 Tailscale)에 있는지, PC에서 원격 제어가 켜져 있는지 확인하세요".to_string();
    for host in &l.hosts {
        let r = http.post(format!("https://{host}:{}/pair", l.port)).json(&json!({ "code": l.code, "device": device })).send().await;
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                if pin::is_mismatch(&e) {
                    return Err("PC의 인증서가 QR과 다릅니다. 다른 기기가 끼어들었을 수 있어 연결하지 않았습니다".into());
                }
                continue;
            }
        };
        let code = r.status().as_u16();
        let body = r.text().await.unwrap_or_default();
        if code != 200 {
            last = api_error(code, &body);
            break;
        }
        let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let saved = Saved {
            hosts: l.hosts.clone(),
            port: l.port,
            fingerprint: l.fingerprint.clone(),
            token: v["token"].as_str().unwrap_or_default().to_string(),
            pc_name: v["pcName"].as_str().unwrap_or(&l.name).to_string(),
        };
        std::fs::write(file(&app)?, serde_json::to_vec(&saved).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let view = ConnectionView { pc_name: saved.pc_name.clone(), hosts: saved.hosts.clone(), port: saved.port, fingerprint: saved.fingerprint.clone() };
        let mut g = st.0.lock().unwrap();
        g.good = Some(host.clone());
        g.saved = Some(saved);
        return Ok(view);
    }
    Err(last)
}

#[tauri::command]
fn connection(app: AppHandle, st: State<'_, Remote>) -> Option<ConnectionView> {
    let mut g = st.0.lock().unwrap();
    if g.saved.is_none() {
        g.saved = file(&app).ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok());
    }
    g.saved.as_ref().map(|s| ConnectionView { pc_name: s.pc_name.clone(), hosts: s.hosts.clone(), port: s.port, fingerprint: s.fingerprint.clone() })
}

/// 이 폰에서 연결 정보를 지운다(PC 쪽 기기 목록에서도 끊는 것이 안전하다).
#[tauri::command]
fn forget(app: AppHandle, st: State<'_, Remote>) {
    let mut g = st.0.lock().unwrap();
    if let Some(h) = g.events.take() {
        h.abort();
    }
    g.saved = None;
    g.good = None;
    if let Ok(p) = file(&app) {
        let _ = std::fs::remove_file(p);
    }
}

/// PC API 호출: GET/POST /api/... 응답 JSON을 그대로 돌려준다.
#[tauri::command]
async fn call(st: State<'_, Remote>, method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    if !path.starts_with("/api/") {
        return Err("잘못된 경로".into());
    }
    let (saved, good) = {
        let g = st.0.lock().unwrap();
        (g.saved.clone().ok_or("PC와 연결돼 있지 않습니다")?, g.good.clone())
    };
    let http = client(&saved.fingerprint)?;
    let mut last = format!("{}에 연결할 수 없습니다", saved.pc_name);
    for host in order(&saved, good.as_ref()) {
        let url = format!("https://{host}:{}{path}", saved.port);
        let req = if method == "POST" { http.post(&url).json(&body.clone().unwrap_or(json!({}))) } else { http.get(&url) };
        match req.bearer_auth(&saved.token).send().await {
            Ok(r) => {
                st.0.lock().unwrap().good = Some(host.clone());
                let code = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                if code == 200 {
                    return serde_json::from_str(&text).map_err(|e| e.to_string());
                }
                return Err(api_error(code, &text));
            }
            Err(e) => {
                if pin::is_mismatch(&e) {
                    return Err("PC의 인증서가 바뀌었습니다. 안전을 위해 멈췄습니다. PC에서 다시 페어링하세요".into());
                }
                last = format!("{}에 연결할 수 없습니다(주소 {host})", saved.pc_name);
            }
        }
    }
    Err(last)
}

/// PC 이벤트 받기 시작(이미 받고 있으면 그대로). 끊기면 2→30초 간격으로 다시 붙는다.
#[tauri::command]
fn events_start(app: AppHandle, st: State<'_, Remote>) -> Result<(), String> {
    let mut g = st.0.lock().unwrap();
    if g.events.as_ref().is_some_and(|h| !h.inner().is_finished()) {
        return Ok(());
    }
    let saved = g.saved.clone().ok_or("PC와 연결돼 있지 않습니다")?;
    let good = g.good.clone();
    g.events = Some(tauri::async_runtime::spawn(async move {
        let mut wait = 2;
        loop {
            for host in order(&saved, good.as_ref()) {
                if stream(&app, &saved, &host).await {
                    wait = 2;
                }
            }
            let _ = app.emit("remote:status", json!({ "connected": false }));
            tokio::time::sleep(Duration::from_secs(wait)).await;
            wait = (wait * 2).min(30);
        }
    }));
    Ok(())
}

/// 한 주소로 붙어 끊길 때까지 이벤트를 넘긴다. 한 번이라도 붙었으면 true.
async fn stream(app: &AppHandle, saved: &Saved, host: &str) -> bool {
    let Ok(mut req) = format!("wss://{host}:{}/api/events", saved.port).into_client_request() else { return false };
    let Ok(auth) = format!("Bearer {}", saved.token).parse() else { return false };
    req.headers_mut().insert("authorization", auth);
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(pin::config(&saved.fingerprint)));
    let conn = tokio::time::timeout(Duration::from_secs(6), tokio_tungstenite::connect_async_tls_with_config(req, None, false, Some(connector))).await;
    let Ok(Ok((ws, _))) = conn else { return false };
    let _ = app.emit("remote:status", json!({ "connected": true, "host": host }));
    let (mut tx, mut rx) = ws.split();
    while let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(70), rx.next()).await {
        match m {
            Ok(Message::Text(t)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    let _ = app.emit("remote:event", v);
                }
            }
            Ok(Message::Ping(p)) => {
                let _ = tx.send(Message::Pong(p)).await;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let builder = tauri::Builder::default().manage(Remote(Mutex::new(Inner::default())));
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    builder
        .invoke_handler(tauri::generate_handler![pair, connection, forget, call, events_start])
        .run(tauri::generate_context!())
        .expect("Elysium Remote를 시작할 수 없습니다");
}

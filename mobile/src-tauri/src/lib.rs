//! Elysium Remote(폰 앱)의 Rust 쪽: PC의 엘리시움과 TLS로만 이야기한다.
//!
//! - 페어링 링크(QR)에 담긴 SHA-256 지문과 같은 인증서만 믿는다(인증 기관·이름 검사는 하지 않음).
//! - 받은 기기 토큰은 앱 전용 저장소(다른 앱은 못 읽음)에 둔다. 화면(JS)에는 토큰을 돌려주지 않는다.
//! - PC 이벤트는 WebSocket으로 받아 "remote:event"로 화면에 넘기고, 끊기면 다시 붙는다.
//! - 한 번 페어링하면 다시 인증하지 않는다: PC가 알려 주는 지금 주소를 저장하고, 저장된 주소가 모두 안 되면
//!   같은 네트워크(/24)에서 PC를 다시 찾는다. 찾은 PC는 지문 고정 TLS와 기기 토큰으로 확인한다.
//! - 다른 네트워크에서는 PC의 터널 공개키로 iroh 터널(tunnel.rs)을 열어 붙는다. 포트 포워딩이 필요 없다.

mod link;
mod pin;
mod tunnel;

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
    /// PC의 터널 공개키(예전에 페어링한 폰은 /api/info로 받는다)
    #[serde(default)]
    node: Option<String>,
}

/// 저장된 주소 목록 속 터널 길의 이름
const RELAY: &str = "relay";

/// 화면에 보여 주는 연결 정보(토큰은 뺀다)
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionView {
    pc_name: String,
    hosts: Vec<String>,
    port: u16,
    fingerprint: String,
    /// 다른 네트워크에서도 붙을 수 있는지
    relay: bool,
}

impl ConnectionView {
    fn of(s: &Saved) -> Self {
        ConnectionView { pc_name: s.pc_name.clone(), hosts: s.hosts.clone(), port: s.port, fingerprint: s.fingerprint.clone(), relay: s.node.is_some() }
    }
}

#[derive(Default)]
struct Inner {
    saved: Option<Saved>,
    /// 마지막으로 붙은 주소(먼저 시도한다)
    good: Option<String>,
    events: Option<tauri::async_runtime::JoinHandle<()>>,
    /// 마지막으로 PC를 다시 찾은 시각(너무 자주 훑지 않는다)
    searched: Option<std::time::Instant>,
}

/// 저장해 두는 주소 수
const MAX_HOSTS: usize = 8;
/// 다시 찾기 사이의 최소 간격
const SEARCH_GAP: Duration = Duration::from_secs(20);

struct Remote(Mutex<Inner>);

fn file(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("connection.json"))
}

fn save(app: &AppHandle, saved: &Saved) {
    if let (Ok(p), Ok(b)) = (file(app), serde_json::to_vec(saved)) {
        let _ = std::fs::write(p, b);
    }
}

/// PC가 알려 준 지금 주소를 앞에, 예전 주소를 뒤에 둔다(IPv4만).
fn merge_hosts(now: &[String], old: &[String]) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for h in now.iter().chain(old) {
        if h.parse::<std::net::Ipv4Addr>().is_ok() && !v.contains(h) {
            v.push(h.clone());
        }
    }
    v.truncate(MAX_HOSTS);
    v
}

/// 다시 찾을 /24 대역: 저장된 사설 주소와 이 폰의 지금 주소. Tailscale·공인 주소는 훑지 않는다.
fn search_prefixes(saved: &[String], own: Option<std::net::Ipv4Addr>) -> Vec<[u8; 3]> {
    let mut out: Vec<[u8; 3]> = vec![];
    for ip in own.into_iter().chain(saved.iter().filter_map(|h| h.parse::<std::net::Ipv4Addr>().ok())) {
        let o = ip.octets();
        let private = o[0] == 10 || (o[0] == 172 && (16..32).contains(&o[1])) || (o[0] == 192 && o[1] == 168);
        if private && !out.contains(&[o[0], o[1], o[2]]) {
            out.push([o[0], o[1], o[2]]);
        }
    }
    out.truncate(4);
    out
}

/// 이 폰의 지금 LAN 주소(패킷을 보내지 않고 경로만 묻는다)
fn own_ip() -> Option<std::net::Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("10.255.255.255:9").or_else(|_| s.connect("192.168.255.255:9")).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v) if !v.is_unspecified() && !v.is_loopback() => Some(v),
        _ => None,
    }
}

/// 같은 네트워크에서 PC를 다시 찾는다: 포트가 열린 주소마다 지문 고정 TLS + 토큰으로 /api/info를 불러 본다.
async fn search(saved: &Saved) -> Option<(String, Vec<String>)> {
    use futures_util::stream::{self, StreamExt as _};
    let prefixes = search_prefixes(&saved.hosts, own_ip());
    let port = saved.port;
    let candidates: Vec<String> = prefixes.iter().flat_map(|p| (1..255u8).map(move |d| format!("{}.{}.{}.{d}", p[0], p[1], p[2]))).filter(|h| !saved.hosts.contains(h)).collect();
    let open: Vec<String> = stream::iter(candidates)
        .map(|h| async move {
            let ok = tokio::time::timeout(Duration::from_millis(700), tokio::net::TcpStream::connect((h.as_str(), port))).await.is_ok_and(|r| r.is_ok());
            ok.then_some(h)
        })
        .buffer_unordered(64)
        .filter_map(|x| async move { x })
        .collect()
        .await;
    let http = client(&saved.fingerprint).ok()?;
    for host in open {
        let Ok(r) = http.get(format!("https://{host}:{port}/api/info")).bearer_auth(&saved.token).timeout(Duration::from_secs(4)).send().await else { continue };
        if r.status().as_u16() != 200 {
            continue;
        }
        let now: Vec<String> = r.json::<Value>().await.ok().and_then(|v| serde_json::from_value(v["hosts"].clone()).ok()).unwrap_or_default();
        return Some((host.clone(), merge_hosts(&[vec![host], now].concat(), &saved.hosts)));
    }
    None
}

/// 저장된 주소가 모두 안 될 때 PC를 다시 찾고, 찾으면 주소를 저장한다. 찾은 주소를 돌려준다.
async fn rediscover(app: &AppHandle) -> Option<String> {
    let st = app.state::<Remote>();
    let saved = {
        let mut g = st.0.lock().unwrap();
        if g.searched.is_some_and(|t| t.elapsed() < SEARCH_GAP) {
            return None;
        }
        g.searched = Some(std::time::Instant::now());
        g.saved.clone()?
    };
    let _ = app.emit("remote:status", json!({ "connected": false, "searching": true }));
    let (host, hosts) = search(&saved).await?;
    let mut g = st.0.lock().unwrap();
    let s = g.saved.as_mut()?;
    s.hosts = hosts;
    let copy = s.clone();
    g.good = Some(host.clone());
    drop(g);
    save(app, &copy);
    Some(host)
}

fn valid_node(v: &Value) -> Option<String> {
    v["node"].as_str().map(str::to_ascii_lowercase).filter(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// /api/info 답의 지금 주소와 터널 공개키로 저장된 정보를 갱신한다.
fn learn_hosts(app: &AppHandle, v: &Value) {
    let now = serde_json::from_value::<Vec<String>>(v["hosts"].clone()).unwrap_or_default();
    let st = app.state::<Remote>();
    let mut g = st.0.lock().unwrap();
    let Some(s) = g.saved.as_mut() else { return };
    let next = merge_hosts(&now, &s.hosts);
    // PC에서 외부 연결을 끄면 null이 온다: 그때는 터널 길을 지운다(예전 PC는 이 칸이 없다).
    let node = if v.get("node").is_some() { valid_node(v) } else { s.node.clone() };
    if next != s.hosts || node != s.node {
        s.hosts = next;
        s.node = node;
        let copy = s.clone();
        drop(g);
        save(app, &copy);
    }
}

fn client(fp: &str) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .use_preconfigured_tls(pin::config(fp))
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

/// 시도 순서: 마지막으로 붙은 길 → (집 네트워크면) 저장된 주소 → 터널. 밖에서는 터널을 먼저 써서
/// 닿지 않는 집 주소의 연결 시간 초과를 기다리지 않는다.
fn order(saved: &Saved, good: Option<&String>, own: Option<std::net::Ipv4Addr>) -> Vec<String> {
    let relay = saved.node.is_some();
    // 터널 키가 사라졌으면 마지막 길이 터널이었어도 쓰지 않는다.
    let good = good.filter(|g| relay || g.as_str() != RELAY);
    let mut v: Vec<String> = good.into_iter().cloned().collect();
    let relay = relay && good.map(String::as_str) != Some(RELAY);
    let home = own.is_some_and(|ip| {
        let o = ip.octets();
        saved.hosts.iter().filter_map(|h| h.parse::<std::net::Ipv4Addr>().ok()).any(|h| h.octets()[..3] == o[..3])
    });
    if relay && !home {
        v.push(RELAY.into());
    }
    v.extend(saved.hosts.iter().filter(|h| Some(*h) != good).cloned());
    if relay && home {
        v.push(RELAY.into());
    }
    v
}

/// 길 하나의 주소. 터널은 PC와 연결이 맺어진 뒤에 로컬 포트를 준다(못 맺으면 None).
async fn base(saved: &Saved, host: &str, scheme: &str) -> Option<String> {
    if host == RELAY {
        let port = tunnel::ready(saved.node.as_deref()?).await.ok()?;
        return Some(format!("{scheme}://127.0.0.1:{port}"));
    }
    Some(format!("{scheme}://{host}:{}", saved.port))
}

fn api_error(code: u16, body: &str) -> String {
    serde_json::from_str::<Value>(body).ok().and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or_else(|| format!("PC가 요청을 거절했습니다({code})"))
}

/// 페어링: 링크의 주소를 차례로 시도해 코드를 내고 토큰을 받는다.
#[tauri::command]
async fn pair(app: AppHandle, st: State<'_, Remote>, link: String, device: String) -> Result<ConnectionView, String> {
    let l = link::parse(&link)?;
    let http = client(&l.fingerprint)?;
    let mut last = "PC에 연결할 수 없습니다. PC에서 원격 제어가 켜져 있는지 확인하세요".to_string();
    // 같은 Wi-Fi 주소가 안 되면 터널로도 페어링한다(밖에서 링크를 받은 경우).
    let probe = Saved { hosts: l.hosts.clone(), port: l.port, fingerprint: l.fingerprint.clone(), token: String::new(), pc_name: l.name.clone(), node: l.node.clone() };
    let mut ways = l.hosts.clone();
    if l.node.is_some() {
        ways.push(RELAY.into());
    }
    for host in &ways {
        let Some(url) = base(&probe, host, "https").await else { continue };
        let r = http.post(format!("{url}/pair")).json(&json!({ "code": l.code, "device": device })).send().await;
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
            node: valid_node(&v).or(l.node.clone()),
        };
        std::fs::write(file(&app)?, serde_json::to_vec(&saved).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let view = ConnectionView::of(&saved);
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
    g.saved.as_ref().map(ConnectionView::of)
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
    tauri::async_runtime::spawn(tunnel::close());
}

/// PC API 호출: GET/POST /api/... 응답 JSON을 그대로 돌려준다. 저장된 주소가 모두 안 되면 PC를 다시 찾아 한 번 더 부른다.
#[tauri::command]
async fn call(app: AppHandle, st: State<'_, Remote>, method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    if !path.starts_with("/api/") {
        return Err("잘못된 경로".into());
    }
    match call_once(&st, &method, &path, body.as_ref()).await {
        Err(Unreachable(msg)) => {
            if rediscover(&app).await.is_none() {
                return Err(msg);
            }
            call_once(&st, &method, &path, body.as_ref()).await.map_err(|Unreachable(m)| m)?
        }
        Ok(r) => r,
    }
    .inspect(|v| {
        if path == "/api/info" {
            learn_hosts(&app, v);
        }
    })
}

/// 어느 주소로도 닿지 못했다(다시 찾아볼 만하다).
struct Unreachable(String);

/// 저장된 주소를 차례로 시도한다. 닿았으면 Ok(그 응답의 결과), 하나도 닿지 못했으면 Err.
async fn call_once(st: &State<'_, Remote>, method: &str, path: &str, body: Option<&Value>) -> Result<Result<Value, String>, Unreachable> {
    let (saved, good) = {
        let g = st.0.lock().unwrap();
        match g.saved.clone() {
            Some(s) => (s, g.good.clone()),
            None => return Ok(Err("PC와 연결돼 있지 않습니다".into())),
        }
    };
    let http = match client(&saved.fingerprint) {
        Ok(h) => h,
        Err(e) => return Ok(Err(e)),
    };
    let mut last = format!("{}에 연결할 수 없습니다. PC가 켜져 있고 원격 제어가 켜져 있는지 확인하세요", saved.pc_name);
    for host in order(&saved, good.as_ref(), own_ip()) {
        let Some(url) = base(&saved, &host, "https").await else { continue };
        let url = format!("{url}{path}");
        let req = match method {
            "POST" => http.post(&url).json(&body.cloned().unwrap_or(json!({}))),
            "DELETE" => http.delete(&url),
            _ => http.get(&url),
        };
        // 파일을 주고받는 요청은 느린 망에서도 끝나도록 시간을 넉넉히 준다.
        let req = if path.starts_with("/api/attachments") { req.timeout(Duration::from_secs(300)) } else { req };
        match req.bearer_auth(&saved.token).send().await {
            Ok(r) => {
                st.0.lock().unwrap().good = Some(host.clone());
                let code = r.status().as_u16();
                let text = r.text().await.unwrap_or_default();
                if code == 200 {
                    return Ok(serde_json::from_str(&text).map_err(|e| e.to_string()));
                }
                if code == 401 {
                    return Ok(Err("PC에서 이 폰의 연결이 해제되었습니다. PC의 설정 → 폰 연결에서 다시 페어링하세요".into()));
                }
                return Ok(Err(api_error(code, &text)));
            }
            Err(e) => {
                if pin::is_mismatch(&e) {
                    // 같은 주소를 이제 다른 기기가 쓰고 있을 수 있다(DHCP). 다른 주소를 계속 시도하고, 다 안 되면 다시 찾는다.
                    last = format!("{}의 주소({host})에 다른 기기가 응답했습니다. PC를 다시 찾는 중입니다", saved.pc_name);
                    continue;
                }
            }
        }
    }
    Err(Unreachable(last))
}

/// PC 이벤트 받기 시작(이미 받고 있으면 그대로). 끊기면 2→15초 간격으로 다시 붙고, 주소가 모두 안 되면 PC를 다시 찾는다.
#[tauri::command]
fn events_start(app: AppHandle, st: State<'_, Remote>) -> Result<(), String> {
    let mut g = st.0.lock().unwrap();
    if g.events.as_ref().is_some_and(|h| !h.inner().is_finished()) {
        return Ok(());
    }
    if g.saved.is_none() {
        return Err("PC와 연결돼 있지 않습니다".into());
    }
    g.events = Some(tauri::async_runtime::spawn(async move {
        let mut wait = 2;
        loop {
            // 다시 찾기로 주소가 바뀌었을 수 있으니 매번 새로 읽는다.
            let (saved, good) = {
                let st = app.state::<Remote>();
                let g = st.0.lock().unwrap();
                match g.saved.clone() {
                    Some(s) => (s, g.good.clone()),
                    None => return,
                }
            };
            let mut reached = false;
            for host in order(&saved, good.as_ref(), own_ip()) {
                if stream(&app, &saved, &host).await {
                    reached = true;
                    wait = 2;
                    break;
                }
            }
            let _ = app.emit("remote:status", json!({ "connected": false }));
            if !reached && rediscover(&app).await.is_some() {
                continue;
            }
            tokio::time::sleep(Duration::from_secs(wait)).await;
            wait = (wait * 2).min(15);
        }
    }));
    Ok(())
}

/// 앱이 다시 화면에 나왔을 때: 잠든 사이 끊긴 연결을 기다리지 않고 바로 다시 붙는다.
#[tauri::command]
fn reconnect(app: AppHandle, st: State<'_, Remote>) -> Result<(), String> {
    {
        let mut g = st.0.lock().unwrap();
        if let Some(h) = g.events.take() {
            h.abort();
        }
        g.searched = None;
    }
    events_start(app, st)
}

/// 한 주소로 붙어 끊길 때까지 이벤트를 넘긴다. 한 번이라도 붙었으면 true.
async fn stream(app: &AppHandle, saved: &Saved, host: &str) -> bool {
    let Some(url) = base(saved, host, "wss").await else { return false };
    let Ok(mut req) = format!("{url}/api/events").into_client_request() else { return false };
    let Ok(auth) = format!("Bearer {}", saved.token).parse() else { return false };
    req.headers_mut().insert("authorization", auth);
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(pin::config(&saved.fingerprint)));
    let conn = tokio::time::timeout(Duration::from_secs(6), tokio_tungstenite::connect_async_tls_with_config(req, None, false, Some(connector))).await;
    let Ok(Ok((ws, _))) = conn else { return false };
    let _ = app.emit("remote:status", json!({ "connected": true, "host": host }));
    if let Some(st) = app.try_state::<Remote>() {
        st.0.lock().unwrap().good = Some(host.to_string());
    }
    let (mut tx, mut rx) = ws.split();
    // PC는 25초마다 ping을 보낸다. 그 두 배 넘게 조용하면 끊긴 것으로 본다.
    while let Ok(Some(m)) = tokio::time::timeout(Duration::from_secs(55), rx.next()).await {
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
        .invoke_handler(tauri::generate_handler![pair, connection, forget, call, events_start, reconnect])
        .run(tauri::generate_context!())
        .expect("Elysium Remote를 시작할 수 없습니다");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_addresses_come_first_and_stay_bounded() {
        let old: Vec<String> = ["192.168.0.5", "100.80.1.2"].map(String::from).to_vec();
        let now: Vec<String> = ["192.168.0.23", "evil.example", "192.168.0.5"].map(String::from).to_vec();
        assert_eq!(merge_hosts(&now, &old), vec!["192.168.0.23", "192.168.0.5", "100.80.1.2"]);
        let many: Vec<String> = (1..30).map(|i| format!("10.0.0.{i}")).collect();
        assert_eq!(merge_hosts(&many, &old).len(), MAX_HOSTS);
    }

    fn saved(hosts: &[&str], node: bool) -> Saved {
        Saved { hosts: hosts.iter().map(|h| h.to_string()).collect(), port: 1, fingerprint: String::new(), token: String::new(), pc_name: String::new(), node: node.then(|| "a".repeat(64)) }
    }

    #[test]
    fn tunnel_goes_first_away_from_home_and_last_at_home() {
        let s = saved(&["192.168.0.5", "100.80.1.2"], true);
        let home = "192.168.0.31".parse().ok();
        let away = "10.20.30.40".parse().ok();
        assert_eq!(order(&s, None, home), vec!["192.168.0.5", "100.80.1.2", RELAY]);
        assert_eq!(order(&s, None, away), vec![RELAY, "192.168.0.5", "100.80.1.2"]);
        let relay = RELAY.to_string();
        assert_eq!(order(&s, Some(&relay), home), vec![RELAY, "192.168.0.5", "100.80.1.2"], "마지막으로 붙은 길이 먼저");
        assert_eq!(order(&saved(&["192.168.0.5"], false), Some(&relay), away), vec!["192.168.0.5"], "터널 키가 없으면 주소만");
    }

    #[test]
    fn only_private_lans_are_searched() {
        let saved: Vec<String> = ["192.168.0.5", "100.80.1.2", "8.8.8.8", "192.168.0.9", "10.1.2.3"].map(String::from).to_vec();
        let own = "172.20.5.7".parse().ok();
        assert_eq!(search_prefixes(&saved, own), vec![[172, 20, 5], [192, 168, 0], [10, 1, 2]]);
    }
}

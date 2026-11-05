//! 다른 네트워크의 폰이 포트 포워딩 없이 붙는 길.
//!
//! - 이 PC는 고정 키(remote/tunnel.key)를 가진 iroh 엔드포인트를 띄운다. 폰은 페어링 때 받은 공개키로 연결한다.
//! - iroh는 먼저 직접 연결(홀펀칭)을 시도하고, 안 되면 공용 릴레이로 중계한다. 릴레이는 암호화된 패킷만 본다.
//! - 받은 스트림은 이 PC의 원격 서버(127.0.0.1:포트)로 그대로 잇는다. 그 안에서 지문 고정 TLS와 기기 토큰을
//!   그대로 쓰므로, 공개키를 아는 것만으로는 아무것도 할 수 없다.

use crate::error::{AppError, AppResult};
use iroh::endpoint::presets;
use iroh::{Endpoint, SecretKey};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

pub const ALPN: &[u8] = b"elysium/remote-tunnel/1";

/// 지금 켜진 엔드포인트와 받는 루프
static RUNNING: Mutex<Option<(Endpoint, tokio::task::JoinHandle<()>)>> = Mutex::new(None);
/// "starting" | "online" | "error" (꺼져 있으면 None)
static STATE: Mutex<Option<&'static str>> = Mutex::new(None);

fn set_state(s: Option<&'static str>) {
    *STATE.lock().unwrap_or_else(|p| p.into_inner()) = s;
}

pub fn state() -> Option<&'static str> {
    *STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// 이 PC의 터널 키. 처음 한 번 만들고 계속 쓴다(바뀌면 폰이 다시 페어링해야 한다).
fn secret(data_dir: &Path) -> AppResult<SecretKey> {
    let dir = data_dir.join("remote");
    let p = dir.join("tunnel.key");
    if let Ok(text) = std::fs::read_to_string(&p) {
        if let Ok(k) = text.trim().parse::<SecretKey>() {
            return Ok(k);
        }
    }
    std::fs::create_dir_all(&dir)?;
    let k = SecretKey::generate();
    let hex: String = k.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&p, hex)?;
    Ok(k)
}

/// 폰이 이 PC를 찾는 공개키(64자 hex)
pub fn endpoint_id(data_dir: &Path) -> AppResult<String> {
    Ok(secret(data_dir)?.public().to_string())
}

pub fn stop() {
    set_state(None);
    if let Some((ep, task)) = RUNNING.lock().unwrap_or_else(|p| p.into_inner()).take() {
        task.abort();
        tauri::async_runtime::spawn(async move { ep.close().await });
    }
}

/// 터널을 켠다. 원격 서버(127.0.0.1:port)가 먼저 떠 있어야 한다.
pub async fn start(data_dir: &Path, port: u16) -> AppResult<()> {
    stop();
    set_state(Some("starting"));
    let ep = match Endpoint::builder(presets::N0).secret_key(secret(data_dir)?).alpns(vec![ALPN.to_vec()]).bind().await {
        Ok(ep) => ep,
        Err(e) => {
            set_state(Some("error"));
            return Err(AppError::msg(format!("외부 연결을 준비할 수 없습니다: {e}")));
        }
    };
    let task = tokio::spawn(serve(ep.clone(), port));
    *RUNNING.lock().unwrap_or_else(|p| p.into_inner()) = Some((ep.clone(), task));
    // 릴레이에 붙으면 밖에서도 찾을 수 있다. 오래 걸려도 서버 켜기를 막지 않는다.
    tokio::spawn(async move {
        let ok = tokio::time::timeout(Duration::from_secs(30), ep.online()).await.is_ok();
        if state() == Some("starting") {
            set_state(Some(if ok { "online" } else { "error" }));
        }
    });
    Ok(())
}

async fn serve(ep: Endpoint, port: u16) {
    while let Some(incoming) = ep.accept().await {
        tokio::spawn(async move {
            let Ok(accepting) = incoming.accept() else { return };
            let Ok(conn) = accepting.await else { return };
            // 연결 하나에 스트림 여러 개(HTTP 요청·이벤트 스트림마다 하나)
            while let Ok((send, recv)) = conn.accept_bi().await {
                tokio::spawn(pipe(send, recv, port));
            }
        });
    }
}

/// 스트림 하나를 원격 서버의 TCP 연결 하나로 잇는다.
async fn pipe<S, R>(send: S, recv: R, port: u16)
where
    S: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
{
    let Ok(mut tcp) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else { return };
    let _ = tcp.set_nodelay(true);
    let mut quic = tokio::io::join(recv, send);
    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut quic).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn key_is_created_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = endpoint_id(dir.path()).unwrap();
        let b = endpoint_id(dir.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[tokio::test]
    async fn pipe_joins_stream_to_local_server() {
        // 로컬 에코 서버
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = [0u8; 5];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        });
        // 스트림 대신 메모리 파이프
        let (mut near, far) = tokio::io::duplex(64);
        let (r, w) = tokio::io::split(far);
        let t = tokio::spawn(pipe(w, r, port));
        near.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        near.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        drop(near);
        let _ = tokio::time::timeout(Duration::from_secs(2), t).await;
    }
}

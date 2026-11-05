//! 다른 네트워크에서 PC에 붙는 길: PC의 터널 공개키로 iroh 연결을 맺고, 폰 안의 127.0.0.1 포트를 그 연결로 잇는다.
//! reqwest·WebSocket은 이 포트로 평소처럼 지문 고정 TLS를 쓴다(터널은 바이트만 나른다).
//! iroh는 직접 연결(홀펀칭)을 먼저 시도하고, 안 되면 공용 릴레이로 중계한다. 포트 포워딩이 필요 없다.

use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub const ALPN: &[u8] = b"elysium/remote-tunnel/1";
/// PC를 찾고 연결하는 데 기다리는 시간
const CONNECT_WAIT: Duration = Duration::from_secs(20);

/// PC 하나로 가는 연결. 끊기면 다음 요청 때 다시 맺는다.
#[derive(Clone)]
struct Link {
    ep: Endpoint,
    to: EndpointAddr,
    conn: Arc<Mutex<Option<Connection>>>,
}

impl Link {
    async fn get(&self) -> Result<Connection, String> {
        let mut g = self.conn.lock().await;
        if let Some(c) = g.as_ref() {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        let c = tokio::time::timeout(CONNECT_WAIT, self.ep.connect(self.to.clone(), ALPN))
            .await
            .map_err(|_| "PC를 찾지 못했습니다(시간 초과)".to_string())?
            .map_err(|e| format!("PC에 연결할 수 없습니다: {e}"))?;
        *g = Some(c.clone());
        Ok(c)
    }
}

struct Tunnel {
    node: EndpointId,
    link: Link,
    port: u16,
    accept: tokio::task::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

static TUNNEL: Mutex<Option<Arc<Tunnel>>> = Mutex::const_new(None);

/// 터널의 로컬 포트를 돌려준다. PC와 연결이 맺어진 뒤에만 Ok(못 맺으면 다른 길을 쓰도록 Err).
pub async fn ready(node: &str) -> Result<u16, String> {
    let id: EndpointId = node.parse().map_err(|_| "잘못된 PC 키".to_string())?;
    let t = {
        let mut g = TUNNEL.lock().await;
        match g.as_ref() {
            Some(t) if t.node == id => t.clone(),
            _ => {
                let ep = Endpoint::builder(presets::N0).bind().await.map_err(|e| format!("외부 연결을 준비할 수 없습니다: {e}"))?;
                let t = Arc::new(open(ep, id.into()).await?);
                *g = Some(t.clone());
                t
            }
        }
    };
    t.link.get().await?;
    Ok(t.port)
}

/// 터널을 닫는다(연결 정보를 지울 때).
pub async fn close() {
    if let Some(t) = TUNNEL.lock().await.take() {
        t.link.ep.close().await;
    }
}

/// 로컬 포트를 열고, 들어오는 연결마다 PC 쪽 스트림 하나로 잇는다.
async fn open(ep: Endpoint, to: EndpointAddr) -> Result<Tunnel, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let node = to.id;
    let link = Link { ep, to, conn: Arc::new(Mutex::new(None)) };
    let l2 = link.clone();
    let accept = tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let link = l2.clone();
            tokio::spawn(async move {
                let Ok(conn) = link.get().await else { return };
                let Ok((send, recv)) = conn.open_bi().await else { return };
                pipe(tcp, send, recv).await;
            });
        }
    });
    Ok(Tunnel { node, link, port, accept })
}

async fn pipe<S, R>(mut tcp: tokio::net::TcpStream, send: S, recv: R)
where
    S: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncRead + Unpin,
{
    let _ = tcp.set_nodelay(true);
    let mut quic = tokio::io::join(recv, send);
    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut quic).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::RelayMode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// PC 쪽 흉내: 받은 스트림을 그대로 되돌려 준다(실제 PC는 원격 서버로 잇는다).
    async fn echo_pc() -> Endpoint {
        let ep = Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled).alpns(vec![ALPN.to_vec()]).bind().await.unwrap();
        let e2 = ep.clone();
        tokio::spawn(async move {
            while let Some(inc) = e2.accept().await {
                tokio::spawn(async move {
                    let conn = inc.accept().unwrap().await.unwrap();
                    while let Ok((mut s, mut r)) = conn.accept_bi().await {
                        tokio::spawn(async move {
                            let _ = tokio::io::copy(&mut r, &mut s).await;
                            let _ = s.finish();
                        });
                    }
                });
            }
        });
        ep
    }

    /// 실제 인터넷(n0 릴레이·DNS)이 필요하다: cargo test -- --ignored
    /// PC가 직접 연결을 못 받는 상황(릴레이만)에서 공개키 문자열만으로 붙는다.
    #[tokio::test]
    #[ignore]
    async fn reaches_pc_by_key_through_public_relay() {
        let pc = Endpoint::builder(presets::N0).clear_ip_transports().alpns(vec![ALPN.to_vec()]).bind().await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), pc.online()).await.expect("릴레이에 붙지 못함");
        let e2 = pc.clone();
        tokio::spawn(async move {
            while let Some(inc) = e2.accept().await {
                tokio::spawn(async move {
                    let conn = inc.accept().unwrap().await.unwrap();
                    while let Ok((mut s, mut r)) = conn.accept_bi().await {
                        tokio::spawn(async move {
                            let _ = tokio::io::copy(&mut r, &mut s).await;
                            let _ = s.finish();
                        });
                    }
                });
            }
        });
        let t0 = std::time::Instant::now();
        let port = ready(&pc.id().to_string()).await.unwrap();
        eprintln!("터널 연결 {:?}", t0.elapsed());
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s.write_all(b"via relay").await.unwrap();
        let mut back = [0u8; 9];
        tokio::time::timeout(Duration::from_secs(15), s.read_exact(&mut back)).await.unwrap().unwrap();
        assert_eq!(&back, b"via relay");
        eprintln!("왕복 완료 {:?}", t0.elapsed());
    }

    #[tokio::test]
    async fn local_port_carries_bytes_to_pc_and_back() {
        let pc = echo_pc().await;
        let me = Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled).bind().await.unwrap();
        let t = open(me, pc.addr()).await.unwrap();
        t.link.get().await.unwrap();
        // 같은 연결 위로 스트림 두 개(HTTP 요청과 이벤트 스트림처럼)
        for msg in [&b"first"[..], &b"second"[..]] {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
            s.write_all(msg).await.unwrap();
            let mut back = vec![0u8; msg.len()];
            tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut back)).await.unwrap().unwrap();
            assert_eq!(back, msg);
        }
    }
}

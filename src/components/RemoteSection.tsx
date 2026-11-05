import { useCallback, useEffect, useState } from "react";
import { api, errorText, type PairInfo, type RemoteStatus } from "../lib/api";
import { useTauriEvent } from "../lib/events";
import { ask } from "../lib/confirm";
import { useApp } from "../store";

function when(iso: string | null): string {
  if (!iso) return "아직 접속 안 함";
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 90) return "방금 접속";
  if (s < 3600) return `${Math.floor(s / 60)}분 전 접속`;
  if (s < 86400) return `${Math.floor(s / 3600)}시간 전 접속`;
  return `${new Date(iso).toLocaleDateString("ko-KR", { month: "short", day: "numeric" })} 접속`;
}

/** 다른 네트워크 연결 상태 표시 */
const RELAY_TAG: Record<"starting" | "online" | "error", string> = {
  starting: "외부 연결 준비 중",
  online: "외부 연결 가능",
  error: "외부 연결 불가 (로컬 가능)",
};

/** 설정 → 폰 연결: 원격 제어 켜기, 페어링 QR, 연결된 기기. */
export default function RemoteSection() {
  const { toast } = useApp();
  const [st, setSt] = useState<RemoteStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [pair, setPair] = useState<(PairInfo & { until: number }) | null>(null);
  const [left, setLeft] = useState(0);

  const load = useCallback(() => api.remoteStatus().then(setSt).catch((e) => toast(errorText(e), "error")), [toast]);
  useEffect(() => {
    load();
    return () => {
      // 설정 창을 닫으면 열어 둔 페어링 창도 닫는다.
      api.remotePairCancel().catch(() => {});
    };
  }, [load]);

  // 외부 연결을 준비하는 동안은 상태를 다시 읽는다.
  useEffect(() => {
    if (st?.relay !== "starting") return;
    const t = setTimeout(load, 2500);
    return () => clearTimeout(t);
  }, [st, load]);

  // 폰이 페어링을 마치면 QR을 닫고 목록을 새로 읽는다.
  useTauriEvent("remote:changed", () => {
    if (pair) toast("폰을 연결했습니다.");
    setPair(null);
    load();
  });

  useEffect(() => {
    if (!pair) return;
    const tick = () => {
      const s = Math.max(0, Math.round((pair.until - Date.now()) / 1000));
      setLeft(s);
      if (s === 0) setPair(null);
    };
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [pair]);

  if (!st) return null;
  const on = st.config.enabled;

  async function configure(enabled: boolean, allowApprove: boolean, relay?: boolean) {
    setBusy(true);
    try {
      setSt(await api.remoteConfigure(enabled, allowApprove, relay));
      if (!enabled) setPair(null);
    } catch (e) {
      toast(errorText(e), "error");
      load();
    } finally {
      setBusy(false);
    }
  }

  async function openPair() {
    try {
      const p = await api.remotePairStart();
      setPair({ ...p, until: Date.now() + p.expiresIn * 1000 });
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  const tailscale = st.addresses.some((a) => a.kind === "tailscale");
  return (
    <>
      <label className="toggle-row">
        <span>
          원격 제어
          <small>폰의 Elysium Remote 앱에서 이 PC의 세션을 보고, 질문하고, 멈출 수 있습니다. 암호화(TLS)로만 연결하고, 페어링한 폰만 들어옵니다.</small>
        </span>
        <input type="checkbox" className="switch" role="switch" checked={on} disabled={busy} onChange={(e) => configure(e.target.checked, st.config.allowApprove)} />
      </label>
      {on && (
        <>
          <label className="toggle-row">
            <span>
              폰에서 작업 모드 허락
              <small>끄면 파일 수정 허락은 PC 화면에서만 합니다.</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={st.config.allowApprove} disabled={busy} onChange={(e) => configure(true, e.target.checked)} />
          </label>
          <label className="toggle-row">
            <span>
              {"외부 네트워크 연결 허용"}
              <small>{"포트 포워딩 없이 작동하며, 직접 연결이 어려울 때는 암호화된 데이터만 전달하는 공용 릴레이를 통해 연결됩니다. 기존과 동일한 인증서 및 기기 토큰 검증이 적용되어 안전합니다."}</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={st.config.relay} disabled={busy} onChange={(e) => configure(true, st.config.allowApprove, e.target.checked)} />
          </label>

          <div className="remote-addr">
            {st.running ? (
              st.addresses.map((a) => (
                <span key={a.ip} className={`tag ${a.kind === "tailscale" ? "ok" : ""}`} title={a.name}>
                  {a.kind === "tailscale" ? "Tailscale" : "같은 Wi-Fi"} {a.ip}:{st.running}
                </span>
              ))
            ) : (
              <span className="tag warn">서버가 꺼져 있습니다</span>
            )}
            {st.running && st.relay && <span className={`tag ${st.relay === "online" ? "ok" : st.relay === "error" ? "warn" : ""}`}>{RELAY_TAG[st.relay]}</span>}
          </div>
          <p className="help">
            {st.relay === "online"
              ? "페어링된 휴대전화는 동일한 Wi-Fi는 물론 외부에서도 자동으로 연결됩니다. 이전에 등록한 기기는 재페어링 없이 동일한 Wi-Fi에 한 번만 연결하면 새 경로를 자동으로 인식합니다."
              : tailscale
                ? "밖에서는 Tailscale 주소로 연결합니다(폰에도 Tailscale을 켜 두세요)."
                : "포트 포워딩 없이 외부에서도 휴대전화를 연결하려면 위의 '외부 네트워크 연결 허용'을 켜세요."}{" "}
            처음 켤 때 Windows 방화벽이 묻으면 개인 네트워크를 허용하세요.
          </p>

          {pair ? (
            <div className="pair-box">
              <img className="pair-qr" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(pair.qrSvg)}`} alt="페어링 QR 코드" />
              <div className="pair-side">
                <b>폰의 Elysium Remote에서 QR을 찍으세요</b>
                <span>
                  {Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")} 동안 한 번만 쓸 수 있습니다.
                </span>
                {st.fingerprint && <span className="pair-fp">인증서 지문 {st.fingerprint.slice(0, 16).replace(/(.{4})/g, "$1 ").trim()}</span>}
                <div className="row">
                  <button className="btn sm" onClick={() => navigator.clipboard.writeText(pair.link).then(() => toast("연결 링크를 복사했습니다. 다른 사람에게 보내지 마세요."))}>
                    링크 복사
                  </button>
                  <button
                    className="btn sm ghost"
                    onClick={() => {
                      api.remotePairCancel().catch(() => {});
                      setPair(null);
                    }}
                  >
                    닫기
                  </button>
                </div>
              </div>
            </div>
          ) : (
            <button className="btn sm primary" disabled={!st.running} onClick={openPair}>
              폰 연결하기
            </button>
          )}

          {st.devices.length > 0 && (
            <ul className="device-list">
              {st.devices.map((d) => (
                <li key={d.id}>
                  <span>
                    <b>{d.name}</b>
                    <small>{when(d.lastSeenAt)}</small>
                  </span>
                  <button
                    className="btn sm danger"
                    onClick={async () => {
                      const ok = await ask({ title: `'${d.name}' 연결을 끊을까요?`, body: "그 폰은 바로 이 PC에 접속할 수 없게 되고, 다시 쓰려면 QR로 새로 페어링해야 합니다.", confirmLabel: "연결 끊기", danger: true });
                      if (ok) api.remoteRevoke(d.id).then(setSt).catch((e) => toast(errorText(e), "error"));
                    }}
                  >
                    연결 끊기
                  </button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </>
  );
}

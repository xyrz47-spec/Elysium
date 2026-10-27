import { useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { listen } from "@tauri-apps/api/event";
import ParticleOrb, { type OrbMode } from "../../src/components/ParticleOrb";
import Segmented from "../../src/components/Segmented";
import { ENGINE_LABEL, type EngineId } from "../../src/lib/api";
import Markdown from "./Markdown";
import ConfirmDialog from "../../src/components/ConfirmDialog";
import { ask } from "../../src/lib/confirm";
import { deviceName, errorText, remote, type Approval, type Connection, type Conversation, type EngineInfo, type Message, type PcInfo } from "./remote";

interface Live {
  taskId: string;
  conversationId: string;
  work: boolean;
  phase: string | null;
  label: string | null;
  text: string;
  activity: string | null;
}

type Toast = { id: number; text: string; kind: "info" | "error" };

function useToasts() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toast = useCallback((text: string, kind: Toast["kind"] = "info") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, text, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 3800);
  }, []);
  return { toasts, toast };
}

// ---------------------------------------------------------------------------
// 페어링

async function scanQr(): Promise<string | null> {
  // QR 스캐너는 폰에서만 있다(PC 미리보기에서는 링크 붙여넣기만).
  const m = await import("@tauri-apps/plugin-barcode-scanner").catch(() => null);
  if (!m) return null;
  let p = await m.checkPermissions();
  if (p !== "granted") p = await m.requestPermissions();
  if (p !== "granted") throw "카메라 권한이 없어 QR을 찍을 수 없습니다. 링크를 붙여 넣으세요";
  const r = await m.scan({ windowed: false, formats: [m.Format.QRCode] });
  return r.content;
}

function Pair({ onPaired, toast }: { onPaired: (c: Connection) => void; toast: (t: string, k?: Toast["kind"]) => void }) {
  const [link, setLink] = useState("");
  const [busy, setBusy] = useState(false);

  async function connect(l: string) {
    setBusy(true);
    try {
      onPaired(await remote.pair(l.trim(), deviceName()));
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="pair main">
      <div className="pair-orb" data-orb-anchor aria-hidden />
      <h1>Elysium Remote</h1>
      <p className="lead">PC의 엘리시움에서 설정 → 폰 연결 → “폰 연결하기”를 누르고, 뜬 QR을 찍으세요.</p>
      <button
        className="cta"
        disabled={busy}
        onClick={async () => {
          try {
            const r = await scanQr();
            if (r) connect(r);
            else toast("이 기기에서는 QR을 찍을 수 없습니다. 링크를 붙여 넣으세요");
          } catch (e) {
            toast(errorText(e), "error");
          }
        }}
      >
        {busy ? "연결하는 중…" : "QR 찍기"}
      </button>
      <details className="manual">
        <summary>링크로 연결</summary>
        <textarea value={link} onChange={(e) => setLink(e.target.value)} placeholder="elysium://pair?…" rows={3} spellCheck={false} />
        <button className="ghost-btn" disabled={busy || !link.trim()} onClick={() => connect(link)}>
          연결
        </button>
      </details>
      <p className="fine">암호화(TLS)로만 연결하고, QR에 담긴 PC 인증서만 믿습니다.</p>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 세션

function ago(iso: string): string {
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 60) return "방금";
  if (s < 3600) return `${Math.floor(s / 60)}분 전`;
  if (s < 86400) return `${Math.floor(s / 3600)}시간 전`;
  return new Date(iso).toLocaleDateString("ko-KR", { month: "short", day: "numeric" });
}

const engineLabel = (id: string | undefined | null) => (id ? (ENGINE_LABEL[id as EngineId] ?? id) : "");

function Exchange({ ask, answer }: { ask: Message; answer?: Message }) {
  const d = answer?.data;
  return (
    <article className="ex">
      <div className="ex-ask">{ask.content}</div>
      {answer && (
        <div className="ex-answer">
          <Markdown text={answer.content} />
          {(d?.draftedBy || d?.reviewedBy) && (
            <div className="ex-by">
              {engineLabel(d?.draftedBy)}
              {d?.reviewedBy ? ` 초안, ${engineLabel(d.reviewedBy)} 검토` : ""}
            </div>
          )}
        </div>
      )}
    </article>
  );
}

function pair(messages: Message[]): { ask: Message; answer?: Message }[] {
  const out: { ask: Message; answer?: Message }[] = [];
  for (const m of messages) {
    if (m.role === "user") out.push({ ask: m });
    else if (out.length && !out[out.length - 1].answer) out[out.length - 1].answer = m;
  }
  return out;
}

export default function App() {
  const { toasts, toast } = useToasts();
  const [conn, setConn] = useState<Connection | null | undefined>(undefined);
  const [info, setInfo] = useState<PcInfo | null>(null);
  const [online, setOnline] = useState(false);
  const [engines, setEngines] = useState<EngineInfo[]>([]);
  const [convs, setConvs] = useState<Conversation[]>([]);
  const [cid, setCid] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [live, setLive] = useState<Record<string, Live>>({});
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [drawer, setDrawer] = useState(false);
  const [text, setText] = useState("");
  const [engine, setEngine] = useState("auto");
  const [work, setWork] = useState(false);
  const [sending, setSending] = useState(false);
  const cidRef = useRef(cid);
  cidRef.current = cid;
  const scroller = useRef<HTMLDivElement>(null);

  useEffect(() => {
    remote.connection().then(setConn).catch(() => setConn(null));
  }, []);

  const refreshAll = useCallback(async () => {
    try {
      const [i, e, c, a] = await Promise.all([remote.info(), remote.engines(), remote.conversations(), remote.approvals()]);
      setInfo(i);
      setEngines(e.filter((x) => x.installed));
      setConvs(c);
      setApprovals(a);
      setOnline(true);
      if (cidRef.current) setMessages(await remote.messages(cidRef.current));
    } catch (e) {
      setOnline(false);
      toast(errorText(e), "error");
    }
  }, [toast]);

  const loadMessages = useCallback((id: string | null) => {
    if (!id) return setMessages([]);
    remote.messages(id).then(setMessages).catch((e) => toast(errorText(e), "error"));
  }, [toast]);

  // 연결되면 목록을 읽고 PC 이벤트를 받기 시작한다.
  useEffect(() => {
    if (!conn) return;
    refreshAll();
    remote.eventsStart().catch(() => {});
    const offs = [
      listen<{ connected: boolean }>("remote:status", (e) => {
        setOnline(e.payload.connected);
        if (e.payload.connected) refreshAll();
      }),
      listen<{ event: string; payload: any }>("remote:event", ({ payload: { event, payload: p } }) => {
        const patch = (f: (l: Live) => Live) => setLive((m) => (m[p.taskId] ? { ...m, [p.taskId]: f(m[p.taskId]) } : m));
        switch (event) {
          case "task:started":
            setLive((m) => ({ ...m, [p.taskId]: m[p.taskId] ?? { taskId: p.taskId, conversationId: p.conversationId, work: p.work, phase: null, label: null, text: "", activity: null } }));
            break;
          case "task:phase":
            patch((l) => ({ ...l, phase: p.phase, label: p.label, text: "" }));
            break;
          case "task:delta":
            patch((l) => ({ ...l, text: l.text + p.delta }));
            break;
          case "task:activity":
            patch((l) => ({ ...l, activity: p.text }));
            break;
          case "task:approval":
            setApprovals((x) => [...x.filter((y) => y.id !== p.id), p]);
            break;
          case "approval:decided":
            setApprovals((x) => x.filter((y) => y.id !== p.id));
            break;
          case "message:created":
            if (p.conversationId === cidRef.current) loadMessages(p.conversationId);
            break;
          case "task:done":
            setLive(({ [p.taskId]: _, ...rest }) => rest);
            remote.conversations().then(setConvs).catch(() => {});
            if (p.conversationId === cidRef.current) loadMessages(p.conversationId);
            break;
          case "engines:changed":
            remote.engines().then((e) => setEngines(e.filter((x) => x.installed))).catch(() => {});
            break;
          case "resync":
            refreshAll();
            break;
        }
      }),
    ];
    return () => offs.forEach((o) => o.then((f) => f()));
  }, [conn, refreshAll, loadMessages]);

  useEffect(() => loadMessages(cid), [cid, loadMessages]);

  const mine = Object.values(live).filter((l) => l.conversationId === cid);
  const pending = approvals.filter((a) => a.conversationId === cid);
  useEffect(() => {
    scroller.current?.scrollTo({ top: scroller.current.scrollHeight, behavior: "smooth" });
  }, [messages.length, mine.map((l) => l.text.length).join(), pending.length]);

  if (conn === undefined) return <div className="boot" />;
  if (!conn)
    return (
      <>
        <ParticleOrb mode="idle" />
        <Pair onPaired={(c) => setConn(c)} toast={toast} />
        <Toasts toasts={toasts} />
      </>
    );

  const isApi = engines.find((e) => e.id === engine)?.kind === "api";
  const current = convs.find((c) => c.id === cid);
  const orb: OrbMode = !online ? "off" : approvals.length ? "approval" : Object.keys(live).length ? "thinking" : "idle";

  async function send() {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true);
    try {
      const r = await remote.send(cid, t, engine, work && !isApi);
      setText("");
      setLive((m) => ({ ...m, [r.taskId]: m[r.taskId] ?? { taskId: r.taskId, conversationId: r.conversationId, work, phase: null, label: null, text: "", activity: null } }));
      if (r.conversationId !== cid) setCid(r.conversationId);
      else loadMessages(cid);
      remote.conversations().then(setConvs).catch(() => {});
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setSending(false);
    }
  }

  return (
    <div className="shell">
      <ParticleOrb mode={orb} />
      <header className="top">
        <button className="icon-btn" aria-label="세션 목록" onClick={() => setDrawer(true)}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden>
            <path d="M4 7h16M4 12h16M4 17h10" />
          </svg>
        </button>
        <div className="top-title">
          <b>{current?.title ?? "새 세션"}</b>
          <span className={`pc ${online ? "on" : ""}`}>
            <i /> {info?.pcName ?? conn.pcName} {online ? "연결됨" : "다시 연결하는 중"}
          </span>
        </div>
        <button className="icon-btn" aria-label="새 세션" onClick={() => setCid(null)}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden>
            <path d="M12 5v14M5 12h14" />
          </svg>
        </button>
      </header>

      <main className="main" ref={scroller}>
        {!cid && messages.length === 0 && mine.length === 0 && (
          <div className="hello">
            <div className="hello-orb" data-orb-anchor aria-hidden />
            <h2>{conn.pcName}의 엘리시움</h2>
            <p>여기서 보낸 질문은 PC에서 실행되고, PC 화면에도 같이 보입니다.</p>
          </div>
        )}
        {pair(messages).map((x) => (
          <Exchange key={x.ask.id} ask={x.ask} answer={x.answer} />
        ))}
        {mine.map((l) => (
          <article key={l.taskId} className="ex live">
            <div className="ex-answer">
              <div className="live-head">
                <i className="spark" /> {l.label ? `${l.label} ${l.phase === "review" ? "검토 중" : l.phase === "judge" ? "토론을 판정하는 중" : "쓰는 중"}` : (l.activity ?? "JEV가 판단하는 중")}
              </div>
              {l.text ? <Markdown text={l.text} /> : <div className="live-act">{l.activity ?? "준비 중"}</div>}
              <button className="stop" onClick={() => remote.cancel(l.taskId).catch((e) => toast(errorText(e), "error"))}>
                중지
              </button>
            </div>
          </article>
        ))}
        {pending.map((a) => (
          <article key={a.id} className="approval">
            <b>파일을 고쳐도 될까요?</b>
            <p>{a.summary}</p>
            <code>{a.workspace}</code>
            {info?.allowApprove ? (
              <div className="row">
                <button className="cta sm" onClick={() => remote.decide(a.id, true).catch((e) => toast(errorText(e), "error"))}>
                  허락
                </button>
                <button className="ghost-btn" onClick={() => remote.decide(a.id, false).catch((e) => toast(errorText(e), "error"))}>
                  거절
                </button>
              </div>
            ) : (
              <span className="fine">PC 설정에서 폰의 허락을 막아 두었습니다. PC에서 허락하세요.</span>
            )}
          </article>
        ))}
      </main>

      <footer className={`composer ${work && !isApi ? "work" : ""}`}>
        <textarea value={text} onChange={(e) => setText(e.target.value)} placeholder={online ? "PC의 엘리시움에게 물어보세요" : "PC와 다시 연결하는 중"} rows={1} disabled={!online} />
        <div className="bar">
          <select value={engine} onChange={(e) => setEngine(e.target.value)} aria-label="AI 고르기">
            <option value="auto">자동</option>
            {engines.map((e) => (
              <option key={e.id} value={e.id}>
                {e.label}
              </option>
            ))}
          </select>
          <Segmented
            label="모드"
            className={`mode ${work && !isApi ? "is-work" : ""}`}
            value={work && !isApi ? "work" : "chat"}
            options={[
              { value: "chat", label: "대화" },
              { value: "work", label: "작업" },
            ]}
            onChange={(v) => {
              if (v === "work" && isApi) return toast("API로는 파일을 고칠 수 없습니다. 자동이나 CLI를 고르세요");
              setWork(v === "work");
            }}
          />
          <button className="send" aria-label="보내기" disabled={!text.trim() || sending || !online} onClick={send}>
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden>
              <path d="M12 19V5M6 11l6-6 6 6" />
            </svg>
          </button>
        </div>
      </footer>

      <div className={`drawer-back ${drawer ? "open" : ""}`} onClick={() => setDrawer(false)} />
      <aside className={`drawer ${drawer ? "open" : ""}`} aria-hidden={!drawer} inert={!drawer}>
        <div className="drawer-head">
          <span className="word">Elysium</span>
          <span className="fine">{convs.length}개 세션</span>
        </div>
        <button
          className="cta sm wide"
          onClick={() => {
            setCid(null);
            setDrawer(false);
          }}
        >
          새 세션
        </button>
        <nav className="sessions">
          {convs.map((c, i) => (
            <button
              key={c.id}
              className={`session ${c.id === cid ? "on" : ""} ${Object.values(live).some((l) => l.conversationId === c.id) ? "busy" : ""}`}
              style={{ "--i": i } as CSSProperties}
              onClick={() => {
                setCid(c.id);
                setDrawer(false);
              }}
            >
              <b>{c.title}</b>
              <span>{ago(c.updatedAt)}</span>
            </button>
          ))}
        </nav>
        <button
          className="ghost-btn danger"
          onClick={async () => {
            const ok = await ask({ title: "이 폰에서 PC 연결을 지울까요?", body: "다시 쓰려면 QR로 새로 페어링해야 합니다. PC의 설정 → 폰 연결에서도 이 기기를 끊어 두세요.", confirmLabel: "연결 지우기", danger: true });
            if (ok) {
              remote.forget().then(() => {
                setConn(null);
                setDrawer(false);
              });
            }
          }}
        >
          이 폰에서 연결 지우기
        </button>
      </aside>
      <Toasts toasts={toasts} />
      <ConfirmDialog />
    </div>
  );
}

function Toasts({ toasts }: { toasts: Toast[] }) {
  return (
    <div className="toasts" role="status">
      {toasts.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`}>
          {t.text}
        </div>
      ))}
    </div>
  );
}

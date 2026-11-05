import { QuestionCard, type LeadQuestion } from "./Question";
import { memo, useCallback, useEffect, useRef, useState, type CSSProperties } from "react";
import { listen } from "@tauri-apps/api/event";
import ParticleOrb, { type OrbMode } from "../../src/components/ParticleOrb";
import Segmented from "../../src/components/Segmented";
import { ENGINE_LABEL, formatSize, stripFileMarkers, toBase64, type EngineId } from "../../src/lib/api";
import Markdown from "./Markdown";
import ConfirmDialog from "../../src/components/ConfirmDialog";
import { ask } from "../../src/lib/confirm";
import { deviceName, errorText, remote, type Approval, type Attachment, type Connection, type Conversation, type EngineInfo, type Message, type PcInfo } from "./remote";
import { AgentRow, AgentSheet, SideSheet, T, TokenChip, agentTitle, type AgentState, type SideTurn, type TaskAgents, ProgressBox } from "./Live";
import { liveTurn, sumUsage, turnEvent } from "../../src/lib/usage";

const ASK = {
  you: "요청",
  more: "더보기",
  less: "접기",
};

interface Live extends TaskAgents {
  conversationId: string;
  work: boolean;
  phase: string | null;
  label: string | null;
  text: string;
  activity: string | null;
  /** 총괄이 지운 워커 key(늦게 온 이벤트로 다시 생기지 않게) */
  gone: Record<string, true>;
}

/** 에이전트 글은 끝부분만 둔다(폰 메모리) */
const STREAM_KEEP = 20000;
/** 실시간 이벤트를 모아 화면에 반영하는 간격(ms). 글자 조각마다 다시 그리면 폰이 버벅이고 뜨거워진다. */
const LIVE_FLUSH_MS = 220;
/** PC와 에이전트 상태를 맞추는 간격(ms) */
const SYNC_MS = 6000;

type LiveMap = Record<string, Live>;
type LiveFn = (m: LiveMap) => LiveMap;

/** 실시간 이벤트를 모았다가 한 번에 반영한다. 글자 조각은 실행마다 이어 붙여 한 번만 자른다. 순서는 그대로 지킨다. */
function useLiveQueue(setLive: (f: LiveFn) => void) {
  const q = useRef<{ fns: LiveFn[]; streams: Record<string, Record<string, string>>; text: Record<string, string>; timer: number | null }>({ fns: [], streams: {}, text: {}, timer: null });
  const seal = () => {
    const { streams, text } = q.current;
    if (!Object.keys(streams).length && !Object.keys(text).length) return;
    q.current.streams = {};
    q.current.text = {};
    q.current.fns.push((m) => {
      const out = { ...m };
      for (const [id, keys] of Object.entries(streams)) {
        const l = out[id];
        if (!l) continue;
        const next = { ...l.streams };
        for (const [k, d] of Object.entries(keys)) next[k] = ((next[k] ?? "") + d).slice(-STREAM_KEEP);
        out[id] = { ...l, streams: next };
      }
      for (const [id, d] of Object.entries(text)) if (out[id]) out[id] = { ...out[id], text: out[id].text + d };
      return out;
    });
  };
  const flush = () => {
    const c = q.current;
    if (c.timer != null) window.clearTimeout(c.timer);
    c.timer = null;
    seal();
    const fns = c.fns;
    c.fns = [];
    if (fns.length) setLive((m) => fns.reduce((acc, f) => f(acc), m));
  };
  const later = () => {
    if (q.current.timer == null) q.current.timer = window.setTimeout(flush, LIVE_FLUSH_MS);
  };
  return {
    push(f: LiveFn) {
      seal();
      q.current.fns.push(f);
      later();
    },
    delta(taskId: string, key: string, d: string) {
      const keys = (q.current.streams[taskId] ??= {});
      keys[key] = (keys[key] ?? "") + d;
      later();
    },
    text(taskId: string, d: string) {
      q.current.text[taskId] = (q.current.text[taskId] ?? "") + d;
      later();
    },
    flush,
  };
}

function blankLive(taskId: string, conversationId: string, work: boolean): Live {
  return { taskId, conversationId, work, phase: null, label: null, text: "", activity: null, lead: false, agents: {}, parts: {}, usage: {}, streams: {}, timeline: [], commits: 0, gone: {} };
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

/** 폰에서 첨부 보기: 이미지·글은 눌러서 펼치고, 그 밖의 파일은 PC에서 연다 */
function AttachmentItem({ a, toast }: { a: Attachment; toast: (t: string, k?: Toast["kind"]) => void }) {
  const [state, setState] = useState<{ image?: string; text?: string } | null>(null);
  const [busy, setBusy] = useState(false);
  async function toggle() {
    if (state) return setState(null);
    if (a.kind !== "image" && a.kind !== "text") return toast("이 파일은 PC에서 열 수 있습니다.");
    setBusy(true);
    try {
      const r = await remote.attachment(a.id);
      if (!r.dataBase64) return toast("파일이 너무 커서 모바일에서 미리 볼 수 없으며 PC에서 열 수 있습니다.");
      const bytes = Uint8Array.from(atob(r.dataBase64), (c) => c.charCodeAt(0));
      setState(a.kind === "image" ? { image: `data:${a.mime};base64,${r.dataBase64}` } : { text: new TextDecoder().decode(bytes) });
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="file">
      <button className="file-chip" onClick={toggle} disabled={busy} aria-expanded={!!state}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
          <path d="m21 11-9.2 9.2a5.5 5.5 0 0 1-7.8-7.8l9.2-9.2a3.7 3.7 0 0 1 5.2 5.2l-9.2 9.2a1.8 1.8 0 0 1-2.6-2.6l8.5-8.5" />
        </svg>
        <b>{a.name}</b>
        <span>{formatSize(a.size)}</span>
      </button>
      {state?.image && <img className="file-image" src={state.image} alt={a.name} />}
      {state?.text !== undefined && <pre className="file-text">{state.text}</pre>}
    </div>
  );
}

function Files({ items, toast }: { items?: Attachment[]; toast: (t: string, k?: Toast["kind"]) => void }) {
  if (!items?.length) return null;
  return (
    <div className="files">
      {items.map((a) => (
        <AttachmentItem key={a.id} a={a} toast={toast} />
      ))}
    </div>
  );
}

/** 보낸 요청: 길면 몇 줄만 보이고 눌러서 펼친다. 진행 중이면 바로 아래 진행 카드와 이어 붙는다. */
function Ask({ m, toast }: { m: Message; toast: (t: string, k?: Toast["kind"]) => void }) {
  const [open, setOpen] = useState(false);
  const long = m.content.length > 160 || m.content.split("\n").length > 4;
  return (
    <div className={`ex-ask ${long && !open ? "clamped" : ""}`}>
      <span className="ex-ask-label">{ASK.you}</span>
      <div className="ex-ask-text">{m.content}</div>
      <Files items={m.data?.attachments} toast={toast} />
      {long && (
        <button className="ex-ask-more" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          {open ? ASK.less : ASK.more}
        </button>
      )}
    </div>
  );
}

/** 지난 대화 한 쌍. 실시간 글이 바뀌어도 다시 그리지 않는다(마크다운을 다시 읽지 않게). */
const Exchange = memo(function Exchange({ ask, answer, toast, live = false }: { ask: Message; answer?: Message; toast: (t: string, k?: Toast["kind"]) => void; live?: boolean }) {
  const d = answer?.data;
  return (
    <article className={`ex ${live ? "asking" : ""}`}>
      <Ask m={ask} toast={toast} />
      {answer && (
        <div className={`ex-answer ${answer.content.startsWith("⚠") ? "failed" : ""}`}>
          <Markdown text={answer.content} />
          <Files items={d?.attachments} toast={toast} />
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
});

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
  /** 다른 네트워크에서 터널로 붙었는지 */
  const [viaRelay, setViaRelay] = useState(false);
  const [engines, setEngines] = useState<EngineInfo[]>([]);
  const [convs, setConvs] = useState<Conversation[]>([]);
  const [cid, setCid] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [live, setLive] = useState<Record<string, Live>>({});
  const liveQ = useLiveQueue(setLive);
  const liveQRef = useRef(liveQ);
  liveQRef.current = liveQ;
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [questions, setQuestions] = useState<LeadQuestion[]>([]);
  const [drawer, setDrawer] = useState(false);
  const [sheet, setSheet] = useState<null | "agents" | "side">(null);
  const [side, setSide] = useState<SideTurn[]>([]);
  const [text, setText] = useState("");
  const [engine, setEngine] = useState("auto");
  const [work, setWork] = useState(false);
  const [sending, setSending] = useState(false);
  const [files, setFiles] = useState<Attachment[]>([]);
  const [uploading, setUploading] = useState(0);
  const picker = useRef<HTMLInputElement>(null);
  const cidRef = useRef(cid);
  cidRef.current = cid;
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  useEffect(() => {
    remote.connection().then(setConn).catch(() => setConn(null));
  }, []);

  // PC가 따라가는 작업·에이전트 상태로 맞춘다. 에이전트 상태와 토큰은 PC 값을 믿는다(폰은 출력 끝부분이 없을 때만 채운다).
  const syncAgents = useCallback(() => {
    remote
      .agents()
      .then((list) =>
        setLive((m) => {
          const out = { ...m };
          for (const t of list) {
            const l = out[t.taskId] ?? blankLive(t.taskId, t.conversationId, t.work);
            const agents = { ...l.agents };
            const usage = { ...l.usage };
            const streams = { ...l.streams };
            for (const a of Object.values(t.agents)) {
              agents[a.key] = { key: a.key, engine: a.engine as AgentState["engine"], label: a.label, role: a.role, state: a.state, wait: a.wait, last: a.last, stopped: a.stopped, tools: Math.max(a.tools ?? 0, l.agents[a.key]?.tools ?? 0), bg: a.bg ?? 0 };
              usage[a.key] = { done: a.done, cur: a.cur };
              if (!streams[a.key] && a.tail) streams[a.key] = a.tail;
            }
            const timeline = (t.timeline ?? []).length >= l.timeline.length ? (t.timeline ?? []) : l.timeline;
            out[t.taskId] = { ...l, conversationId: l.conversationId || t.conversationId, lead: l.lead || t.lead, parts: { ...t.parts, ...l.parts }, agents, usage, streams, timeline, commits: Math.max(l.commits, t.commits ?? 0) };
          }
          return out;
        }),
      )
      .catch(() => {});
  }, []);

  const refreshAll = useCallback(async () => {
    try {
      const [i, e, c, a] = await Promise.all([remote.info(), remote.engines(), remote.conversations(), remote.approvals()]);
      remote.questions().then(setQuestions).catch(() => {});
      setInfo(i);
      setEngines(e.filter((x) => x.installed));
      setConvs(c);
      setApprovals(a);
      setOnline(true);
      const want = cidRef.current;
      if (want) {
        const m = await remote.messages(want);
        if (cidRef.current === want) setMessages(m);
      }
      syncAgents();
    } catch (e) {
      setOnline(false);
      toast(errorText(e), "error");
    }
  }, [toast, syncAgents]);

  // 늦게 온 다른 세션의 답이 지금 화면을 덮지 않게 한다.
  const loadMessages = useCallback(
    (id: string | null) => {
      if (!id) return setMessages([]);
      return remote
        .messages(id)
        .then((m) => {
          if (cidRef.current === id) setMessages(m);
        })
        .catch((e) => toast(errorText(e), "error"));
    },
    [toast],
  );

  // 연결되면 목록을 읽고 PC 이벤트를 받기 시작한다.
  useEffect(() => {
    if (!conn) return;
    refreshAll();
    remote.eventsStart().catch(() => {});
    // 폰이 잠들었다 깨면 끊긴 연결을 기다리지 않고 바로 다시 붙고 목록을 새로 읽는다.
    const onVisible = () => {
      if (document.visibilityState !== "visible") return;
      remote.reconnect().catch(() => {});
      refreshAll();
    };
    document.addEventListener("visibilitychange", onVisible);
    const offs = [
      listen<{ connected: boolean; host?: string }>("remote:status", (e) => {
        setOnline(e.payload.connected);
        if (e.payload.connected) setViaRelay(e.payload.host === "relay");
        if (e.payload.connected) refreshAll();
      }),
      listen<{ event: string; payload: any }>("remote:event", ({ payload: { event, payload: p } }) => {
        const lq = liveQRef.current;
        const patch = (f: (l: Live) => Live) => lq.push((m) => (m[p.taskId] ? { ...m, [p.taskId]: f(m[p.taskId]) } : m));
        // 폰이 늦게 붙었으면 에이전트 이벤트로 작업을 만든다.
        const upsert = (f: (l: Live) => Live) => lq.push((m) => ({ ...m, [p.taskId]: f(m[p.taskId] ?? blankLive(p.taskId, p.conversationId ?? "", true)) }));
        const agent = (l: Live, key: string, f: (a: AgentState) => AgentState) => (l.agents[key] ? { ...l, agents: { ...l.agents, [key]: f(l.agents[key]) } } : l);
        const sidePatch = (f: (t: SideTurn) => SideTurn) => setSide((x) => x.map((t) => (t.id === p.sideId ? f(t) : t)));
        switch (event) {
          case "task:started":
            lq.push((m) => ({ ...m, [p.taskId]: { ...(m[p.taskId] ?? blankLive(p.taskId, p.conversationId, p.work)), conversationId: p.conversationId, work: p.work } }));
            break;
          case "task:agent":
            upsert((l) => {
              if (l.gone[p.key]) return l;
              const prev = l.agents[p.key];
              const a: AgentState = { key: p.key, engine: p.engine, label: p.label, role: p.role, state: p.state, wait: null, last: p.state === "run" ? (prev?.last ?? null) : null, stopped: !!p.stopped, tools: prev?.tools ?? 0, bg: prev?.bg ?? 0 };
              return { ...l, conversationId: l.conversationId || p.conversationId, agents: { ...l.agents, [p.key]: a }, usage: { ...l.usage, [p.key]: turnEvent(l.usage[p.key], p) } };
            });
            break;
          case "task:agent-activity":
            patch((l) => agent(l, p.key, (a) => ({ ...a, last: p.text, tools: (a.tools ?? 0) + 1 })));
            break;
          case "task:agent-wait":
            patch((l) => agent(l, p.key, (a) => ({ ...a, state: "run", wait: p.note })));
            break;
          case "task:agent-usage":
            patch((l) => ({ ...l, usage: { ...l.usage, [p.key]: liveTurn(l.usage[p.key], p) } }));
            break;
          case "task:agent-delta":
            lq.delta(p.taskId, p.key, p.delta);
            break;
          case "task:parts":
            upsert((l) => ({ ...l, parts: { ...l.parts, ...Object.fromEntries(p.parts.map((x: { key: string; title: string }) => [x.key, x.title])) } }));
            break;
          case "task:part-added":
            upsert((l) => ({ ...l, parts: { ...l.parts, [p.key]: p.title } }));
            break;
          case "task:agent-removed":
            patch((l) => {
              const drop = <T,>(r: Record<string, T>) => Object.fromEntries(Object.entries(r).filter(([k]) => k !== p.key));
              return { ...l, agents: drop(l.agents), parts: drop(l.parts), streams: drop(l.streams), usage: drop(l.usage), gone: { ...l.gone, [p.key]: true } };
            });
            break;
          case "lead:started":
            upsert((l) => ({ ...l, lead: true }));
            break;
          case "task:agent-bg":
            patch((l) => agent(l, p.key, (a) => ({ ...a, bg: p.count })));
            break;
          case "lead:activity":
            upsert((l) => ({ ...l, timeline: [...l.timeline, { kind: p.kind, text: p.text, at: p.at }].slice(-40) }));
            break;
          case "lead:commit":
            upsert((l) => ({ ...l, commits: l.commits + 1 }));
            break;
          case "side:started":
            sidePatch((t) => ({ ...t, text: "", activity: null }));
            break;
          case "side:activity":
            sidePatch((t) => ({ ...t, activity: p.text }));
            break;
          case "side:delta":
            sidePatch((t) => ({ ...t, text: t.text + p.delta }));
            break;
          case "side:done":
            sidePatch((t) => ({ ...t, state: p.ok ? "ok" : "fail", text: p.text || t.text || p.error || "", activity: null }));
            break;
          case "task:phase":
            patch((l) => ({ ...l, phase: p.phase, label: p.label, text: "" }));
            break;
          case "task:delta":
            lq.text(p.taskId, p.delta);
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
          case "lead:question":
            setQuestions((x) => [...x.filter((y) => y.id !== p.id), p]);
            break;
          case "lead:question-closed":
            setQuestions((x) => x.filter((y) => y.id !== p.id));
            break;
          case "message:created":
            if (p.conversationId === cidRef.current) loadMessages(p.conversationId);
            break;
          case "task:done": {
            // 저장된 답을 받은 뒤에 진행 카드를 지워 카드가 잠깐 사라졌다 다시 생기지 않게 한다. 모아 둔 이벤트를 먼저 반영한다.
            lq.flush();
            const drop = () => setLive(({ [p.taskId]: _, ...rest }) => rest);
            remote.conversations().then(setConvs).catch(() => {});
            if (p.conversationId === cidRef.current) Promise.resolve(loadMessages(p.conversationId)).finally(drop);
            else drop();
            break;
          }
          case "conversation:deleted":
            lq.flush();
            setConvs((x) => x.filter((c) => c.id !== p.conversationId));
            setLive((m) => Object.fromEntries(Object.entries(m).filter(([, l]) => l.conversationId !== p.conversationId)));
            if (cidRef.current === p.conversationId) {
              setCid(null);
              setMessages([]);
            }
            break;
          case "data:cleared":
            lq.flush();
            setCid(null);
            setMessages([]);
            setLive({});
            refreshAll();
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
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      offs.forEach((o) => o.then((f) => f()));
    };
  }, [conn, refreshAll, loadMessages]);

  const hasLive = Object.keys(live).length > 0;
  useEffect(() => {
    if (!hasLive || !online) return;
    // 화면이 꺼져 있으면 묻지 않는다(깨어나면 바로 새로 읽는다).
    const t = window.setInterval(() => document.visibilityState === "visible" && syncAgents(), SYNC_MS);
    return () => window.clearInterval(t);
  }, [hasLive, online, syncAgents]);

  useEffect(() => {
    // 세션을 바꾸면 이전 대화를 바로 지우고 맨 아래부터 본다.
    setMessages([]);
    stick.current = true;
    loadMessages(cid);
  }, [cid, loadMessages]);

  const mine = Object.values(live).filter((l) => l.conversationId === cid);
  const pairs = pair(messages);
  const pending = approvals.filter((a) => a.conversationId === cid);
  // 맨 아래에 있을 때만 새 글을 따라간다. 위로 올려 읽는 중이면 그대로 둔다.
  useEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [messages.length, mine.map((l) => l.text.length).join(), pending.length]);

  if (conn === undefined) return <div className="boot" />;
  if (!conn)
    return (
      <>
        <ParticleOrb mode="idle" lite />
        <Pair onPaired={(c) => setConn(c)} toast={toast} />
        <Toasts toasts={toasts} />
      </>
    );

  const current = convs.find((c) => c.id === cid);
  const tasks = Object.values(live).filter((l) => Object.keys(l.agents).length > 0);
  const runningAgents = tasks.reduce((n, l) => n + Object.values(l.agents).filter((a) => a.state === "run").length, 0);
  const askSide = (q: string) => {
    const id = crypto.randomUUID();
    const before = side.filter((t) => t.state !== "fail").map((t) => ({ role: t.role, text: t.text }));
    setSide((x) => [...x, { id: `${id}:q`, role: "user", text: q }, { id, role: "assistant", text: "", state: "run", activity: null }]);
    remote.side(id, cid, q, before).catch((e) => setSide((x) => x.map((t) => (t.id === id ? { ...t, state: "fail", text: errorText(e) } : t))));
  };
  const orb: OrbMode = !online ? "off" : approvals.length ? "approval" : Object.keys(live).length ? "thinking" : "idle";

  async function upload(list: FileList | null) {
    const picked = Array.from(list ?? []);
    if (picker.current) picker.current.value = "";
    for (const f of picked) {
      if (f.size > 50 * 1024 * 1024) {
        toast("{name} 파일이 50MB를 초과하여 첨부할 수 없습니다.".replace("{name}", f.name), "error");
        continue;
      }
      setUploading((n) => n + 1);
      try {
        const a = await remote.upload(f.name, toBase64(await f.arrayBuffer()));
        setFiles((cur) => [...cur, a]);
      } catch (e) {
        toast(errorText(e), "error");
      } finally {
        setUploading((n) => n - 1);
      }
    }
  }

  async function send() {
    const t = text.trim();
    if ((!t && files.length === 0) || sending || uploading > 0) return;
    setSending(true);
    try {
      const r = await remote.send(cid, t, engine, work, files.map((f) => f.id));
      setText("");
      setFiles([]);
      stick.current = true;
      setLive((m) => ({ ...m, [r.taskId]: m[r.taskId] ?? blankLive(r.taskId, r.conversationId, work) }));
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
      <ParticleOrb mode={orb} lite />
      <header className="top">
        <button className="icon-btn" aria-label="세션 목록" onClick={() => setDrawer(true)}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden>
            <path d="M4 7h16M4 12h16M4 17h10" />
          </svg>
        </button>
        <div className="top-title">
          <b>{current?.title ?? "새 세션"}</b>
          <span className={`pc ${online ? "on" : ""}`}>
            <i /> {info?.pcName ?? conn.pcName} {online ? (viaRelay ? "연결됨 (원격)" : "연결됨") : "다시 연결하는 중"}
          </span>
        </div>
        <button className="icon-btn" aria-label={T.side} onClick={() => setSheet("side")} disabled={!online}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <path d="M5 18.5V6.5A2.5 2.5 0 0 1 7.5 4h9A2.5 2.5 0 0 1 19 6.5v6a2.5 2.5 0 0 1-2.5 2.5H9z" />
            <path d="M9 9.5h6M9 12h3.5" />
          </svg>
        </button>
        <button className={`icon-btn ${runningAgents ? "busy" : ""}`} aria-label={T.agents} onClick={() => setSheet("agents")}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <circle cx="8" cy="9" r="3" />
            <circle cx="16.5" cy="10" r="2.5" />
            <path d="M3 19c.6-2.8 2.6-4.5 5-4.5s4.4 1.7 5 4.5M13.5 18.5c.5-2 1.6-3.2 3-3.2s2.5 1.2 3 3.2" />
          </svg>
          {runningAgents > 0 && <span className="badge">{runningAgents}</span>}
        </button>
      </header>

      <main
        className="main"
        ref={scroller}
        onScroll={() => {
          const el = scroller.current;
          if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
      >
        {!cid && messages.length === 0 && mine.length === 0 && (
          <div className="hello">
            <div className="hello-orb" data-orb-anchor aria-hidden />
            <h2>{conn.pcName}의 엘리시움</h2>
            <p>여기서 보낸 질문은 PC에서 실행되고, PC 화면에도 같이 보입니다.</p>
          </div>
        )}
        {pairs.map((x, i) => (
          <Exchange key={x.ask.id} ask={x.ask} answer={x.answer} toast={toast} live={i === pairs.length - 1 && !x.answer && mine.length > 0} />
        ))}
        {mine.map((l, i) => (
          <article key={l.taskId} className={`ex live ${i === 0 && pairs.length > 0 && !pairs[pairs.length - 1].answer ? "joined" : ""}`}>
            <div className="ex-answer">
              <div className="live-head">
                <i className="spark" /> {l.label ? `${l.label} ${l.phase === "review" ? "검토 중" : l.phase === "judge" ? "토론을 판정하는 중" : "쓰는 중"}` : Object.keys(l.agents).length > 0 ? T.teamWorking.replace("{n}", String(Object.values(l.agents).filter((a) => a.state === "run").length)) : (l.activity ?? T.preparing)}
              </div>
              <TokenChip t={sumUsage(l.usage)} live />
              {Object.keys(l.agents).length > 1 && <ProgressBox t={l} />}
              {Object.keys(l.agents).length > 0 && (
                <div className="live-agents">
                  {Object.values(l.agents).map((a) => (
                    <AgentRow key={a.key} a={a} title={agentTitle(a, l.parts)} usage={l.usage[a.key]} onOpen={() => setSheet("agents")} />
                  ))}
                </div>
              )}
              {l.text ? <Markdown text={stripFileMarkers(l.text)} /> : <div className="live-act">{l.activity ?? "준비 중"}</div>}
              <button className="stop" onClick={() => remote.cancel(l.taskId).catch((e) => toast(errorText(e), "error"))}>
                중지
              </button>
            </div>
          </article>
        ))}
        {questions.map((q) => (
          <QuestionCard key={q.id} q={q} onAnswer={(id, text) => remote.answerQuestion(id, text).catch((e) => toast(errorText(e), "error"))} />
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

      <footer className={`composer ${work ? "work" : ""}`}>
        {(files.length > 0 || uploading > 0) && (
          <div className="draft-files">
            {files.map((f) => (
              <span key={f.id} className="draft-file">
                <b>{f.name}</b>
                <button
                  aria-label={"첨부 파일 삭제"}
                  onClick={() => {
                    setFiles((cur) => cur.filter((x) => x.id !== f.id));
                    remote.discard(f.id).catch(() => {});
                  }}
                >
                  ×
                </button>
              </span>
            ))}
            {uploading > 0 && <span className="draft-file busy">{"파일 업로드 중..."}</span>}
          </div>
        )}
        <input ref={picker} type="file" multiple hidden onChange={(e) => upload(e.target.files)} />
        <textarea value={text} onChange={(e) => setText(e.target.value)} placeholder={online ? "PC의 엘리시움에게 물어보세요" : "PC와 다시 연결하는 중"} rows={1} disabled={!online} />
        <div className="bar">
          <button
            className="icon-btn attach"
            disabled={!online}
            aria-label={"파일 첨부"}
            onClick={() => picker.current?.click()}
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m21 11-9.2 9.2a5.5 5.5 0 0 1-7.8-7.8l9.2-9.2a3.7 3.7 0 0 1 5.2 5.2l-9.2 9.2a1.8 1.8 0 0 1-2.6-2.6l8.5-8.5" />
            </svg>
          </button>
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
            className={`mode ${work ? "is-work" : ""}`}
            value={work ? "work" : "chat"}
            options={[
              { value: "chat", label: "대화" },
              { value: "work", label: "작업" },
            ]}
            onChange={(v) => setWork(v === "work")}
          />
          <button className="send" aria-label="보내기" disabled={(!text.trim() && files.length === 0) || sending || uploading > 0 || !online} onClick={send}>
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
      <AgentSheet open={sheet === "agents"} onClose={() => setSheet(null)} tasks={tasks} toast={toast} />
      <SideSheet open={sheet === "side"} onClose={() => setSheet(null)} thread={side} onAsk={askSide} onStop={(id) => remote.cancel(id).catch((e) => toast(errorText(e), "error"))} />
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

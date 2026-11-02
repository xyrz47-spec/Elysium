import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { api, baseName, ENGINE_LABEL, errorText, type EngineId, type MemoryView, type Message, type Settings } from "../lib/api";
import { useTauriEvent } from "../lib/events";
import { useApp, type AgentRun, type Live } from "../store";
import Segmented from "./Segmented";
import { AgentsIcon, ArrowDownIcon, ArrowUpIcon, ChatIcon, ChevronIcon, CloseIcon, DownloadIcon, FolderIcon, InfoIcon, ListIcon, SearchIcon, SendIcon, StopIcon, TrashIcon } from "./Icons";
import Markdown from "./Markdown";
import { INTEGRATE, pair, ROLE_LABEL } from "./Thread";

export type ToolTab = "info" | "agents" | "side" | "outline" | "find" | "export";

/** 에이전트 탭 문구(설정 창도 같이 쓴다) */
export const A = {
  title: "에이전트",
  tip: "에이전트: 실행 중인 AI 현황 및 진행 상황",
  running: "실행 현황",
  idle: "현재 실행 중인 에이전트가 없습니다. 요청을 보내면 초안, 검토, 토론 등 작업 중인 AI의 상태가 여기에 표시됩니다.",
  roster: "사용 가능한 엔진",
  preset: "동시 실행 수",
  presetHelp: "2개 이상 설정 시 질문 답변과 파일 작업이 여러 에이전트에 분할되어 병렬 진행 후 통합되며, 토큰 소모량이 증가합니다. 메시지에 '에이전트 3개로'나 'opus랑 gpt-5로'처럼 입력하여 바로 지정할 수도 있습니다.",
  off: "꺼짐",
  auto: "자동",
  autoHelp: "JEV가 요청마다 판단해, 독립된 부분으로 나뉘는 파일 작업일 때만 여러 에이전트를 실행하고 각 부분의 AI와 강도를 정합니다. 숫자는 개수를 고정합니다.",
  open: "이동",
  stop: "중지",
  stopTask: "강제 종료",
  stopAgent: "이 에이전트 강제 종료",
  stopAll: "모두 강제 종료",
  talk: "에이전트에게 메시지 입력 후 Enter",
  talkSend: "이 에이전트에 보내기",
  sent: "보낸 메시지",
  stopped: "중지됨",
  output: "생성 내용",
  noOutput: "아직 생성된 내용이 없습니다.",
  activity: "도구 활동",
  files: "담당 파일",
  progress: "{done}/{total} 완료",
  modeWork: "병렬 작업",
  modeTeam: "답변 병합",
  modeCouncil: "회의",
  modeDebate: "토론",
  preparing: "AI 선택하는 중",
  approval: "승인 대기 중",
  busy: "{n}개 실행 중",
  ready: "대기 중",
  none: "설정에서 AI CLI 또는 API 키를 연결해 주십시오.",
  newSession: "새 대화",
};

/** 곁질문 탭 문구 */
const S = {
  title: "곁질문",
  tip: "대화에 영향 없이 진행 상황이나 프로젝트를 질문합니다",
  empty: "진행 상황이나 프로젝트를 물어보십시오. 대화에 저장되지 않으며 진행 중인 작업을 방해하지 않습니다.",
  placeholder: "진행 상황이나 프로젝트 질문",
  send: "질문 보내기",
  stop: "답변 중지",
  clear: "질문 지우기",
  thinking: "답변 준비 중",
  stopped: "답변 중지됨",
  note: "저장되지 않으며 대화에 영향 없음",
};

const AGENT_AUTO = "auto";
/** 동시 실행 수 고르기(도구 패널·설정 창이 같이 쓴다) */
export const agentChoice = {
  value: (s: Settings) => (s.agentsAuto ? AGENT_AUTO : String(s.agents ?? 1)),
  options: [AGENT_AUTO, "1", "2", "3", "4", "5"].map((v) => ({ value: v, label: v === AGENT_AUTO ? A.auto : v === "1" ? A.off : v })),
  apply: (s: Settings, v: string): Settings => (v === AGENT_AUTO ? { ...s, agentsAuto: true } : { ...s, agentsAuto: false, agents: Number(v) }),
  help: (s: Settings) => (s.agentsAuto ? A.autoHelp : A.presetHelp),
};

const STATE_LABEL: Record<AgentRun["state"], string> = {
  run: "실행 중",
  ok: "완료",
  fail: "중단",
};

function since(ms: number) {
  const s = Math.max(0, Math.round(ms / 1000));
  return s >= 60 ? `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}` : `${s}s`;
}

const progressText = (done: number, total: number) => A.progress.replace("{done}", String(done)).replace("{total}", String(total));

/** 에이전트 한 줄: 누르면 맡은 파일·최근 활동·지금까지 쓴 글을 펼친다. 실행 중이면 이 에이전트만 멈출 수 있다. */
function AgentRow({ l, a, now }: { l: Live; a: AgentRun; now: number }) {
  const { cancelAgent, toast } = useApp();
  const [open, setOpen] = useState(false);
  const [msg, setMsg] = useState("");
  const talk = async () => {
    const t = msg.trim();
    if (!t) return;
    setMsg("");
    await api.agentMessage(l.taskId, a.key, t).catch((e) => {
      setMsg(t);
      toast(errorText(e), "error");
    });
  };
  const part = l.parts[a.key];
  // 동시에 쓰는 실행은 key별 글을, 한 줄로 흘리는 실행(초안·검토·종합)은 지금 단계의 글을 보여 준다.
  const text = l.streams[a.key] ?? (a.role === l.phase ? l.text : "");
  const outRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = outRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text, open]);
  const name = part?.title ?? a.label;
  return (
    <li className={`agent-run ${a.state} ${open ? "open" : ""}`}>
      <div className="agent-row">
        <button className="agent-toggle" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          <i className="agent-dot" aria-hidden />
          <div className="agent-main">
            <div className="agent-line">
              <b>{name}</b>
              <span className="agent-role">{part ? a.label : l.work && l.team && a.role === "synth" ? INTEGRATE : (ROLE_LABEL[a.role as keyof typeof ROLE_LABEL] ?? a.role)}</span>
              <span className="agent-state">{a.stopped ? A.stopped : STATE_LABEL[a.state]}</span>
              <span className="agent-time">{a.state === "run" ? since(now - a.startedAt) : a.ms != null ? since(a.ms) : ""}</span>
            </div>
            {!open && a.state === "run" && a.activities.length > 0 && <div className="agent-act">{a.activities[a.activities.length - 1]}</div>}
            {!open && a.state === "fail" && a.error && !a.stopped && <div className="agent-err">{a.error}</div>}
          </div>
          <ChevronIcon open={open} />
        </button>
        {a.state === "run" && (
          <button className="agent-stop" onClick={() => cancelAgent(l.taskId, a.key)} title={A.stopAgent} aria-label={`${A.stopAgent} · ${name}`}>
            <StopIcon />
          </button>
        )}
      </div>
      {open && (
        <div className="agent-detail">
          {(a.model || a.effort) && <div className="agent-sub">{[a.model, a.effort].filter(Boolean).join(" · ")}</div>}
          {part && part.files.length > 0 && (
            <div className="agent-block">
              <h5>{A.files}</h5>
              <div className="agent-files">
                {part.files.map((f) => (
                  <code key={f}>{f}</code>
                ))}
              </div>
            </div>
          )}
          {a.activities.length > 0 && (
            <div className="agent-block">
              <h5>{A.activity}</h5>
              {a.activities.map((x, i) => (
                <div key={i} className="agent-act full">
                  {x}
                </div>
              ))}
            </div>
          )}
          <div className="agent-block">
            <h5>{A.output}</h5>
            {text ? (
              <pre ref={outRef} className="agent-out">
                {text.length > 4000 ? `…${text.slice(-4000)}` : text}
              </pre>
            ) : (
              <div className="agent-sub">{A.noOutput}</div>
            )}
          </div>
          {a.state === "fail" && a.error && !a.stopped && <div className="agent-err">{a.error}</div>}
          {a.sent.length > 0 && (
            <div className="agent-block">
              <h5>{A.sent}</h5>
              {a.sent.map((x, i) => (
                <div key={i} className="agent-sent">
                  {x}
                </div>
              ))}
            </div>
          )}
          {a.state === "run" && (
            <div className="agent-talk">
              <input type="text" value={msg} onChange={(e) => setMsg(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && (e.preventDefault(), talk())} placeholder={A.talk} aria-label={`${A.talkSend} · ${name}`} />
              <button className="icon" onClick={talk} disabled={!msg.trim()} aria-label={A.talkSend} title={A.talkSend}>
                <SendIcon />
              </button>
            </div>
          )}
        </div>
      )}
    </li>
  );
}

function modeOf(l: Live): string | null {
  if (l.council) return A.modeCouncil;
  if (l.team) return l.work ? A.modeWork : A.modeTeam;
  if (l.debate) return A.modeDebate;
  return null;
}

function AgentsTab({ onClose }: { onClose: () => void }) {
  const { live, engines, conversations, conversationId, openConversation, cancel, cancelAll, approvals, settings, saveSettings } = useApp();
  const [, tick] = useState(0);
  // 실행 중이면 경과 시간을 1초마다 다시 그린다.
  useEffect(() => {
    if (!live.length) return;
    const t = setInterval(() => tick((x) => x + 1), 1000);
    return () => clearInterval(t);
  }, [live.length]);
  const tasks = [...live].sort((a, b) => a.startedAt - b.startedAt);
  const busy = new Map<string, number>();
  for (const l of live) for (const a of Object.values(l.agents)) if (a.state === "run") busy.set(a.engine, (busy.get(a.engine) ?? 0) + 1);
  const installed = engines.filter((e) => e.installed);
  const now = Date.now();
  return (
    <div className="tool-body">
      <section className="tool-sec">
        <h4>
          {A.running}
          {tasks.length > 0 && <span className="agent-count">{tasks.length}</span>}
          {tasks.length > 1 && (
            <button className="btn sm danger agent-stop-all" onClick={cancelAll}>
              {A.stopAll}
            </button>
          )}
        </h4>
        {tasks.length === 0 && <Empty>{A.idle}</Empty>}
        {tasks.map((l) => {
          const c = conversations.find((x) => x.id === l.conversationId);
          const runs = Object.values(l.agents).sort((a, b) => a.startedAt - b.startedAt);
          const waiting = approvals.some((a) => a.taskId === l.taskId);
          const done = runs.filter((a) => a.state !== "run").length;
          const mode = modeOf(l);
          return (
            <div key={l.taskId} className={`agent-task ${l.conversationId === conversationId ? "here" : ""}`}>
              <div className="agent-task-head">
                <b title={c?.title}>{c?.title ?? A.newSession}</b>
                {mode && <span className="tag">{mode}</span>}
                <span className="agent-time">{since(now - l.startedAt)}</span>
              </div>
              {runs.length > 1 && (
                <div className="agent-progress">
                  <i style={{ "--w": `${(done / runs.length) * 100}%` } as CSSProperties} />
                  <span>{progressText(done, runs.length)}</span>
                </div>
              )}
              <ul className="agent-runs">
                {runs.length === 0 && <li className="agent-run wait">{waiting ? A.approval : A.preparing}</li>}
                {runs.map((a) => (
                  <AgentRow key={a.key} l={l} a={a} now={now} />
                ))}
              </ul>
              <div className="row">
                {l.conversationId !== conversationId && (
                  <button
                    className="btn sm ghost"
                    onClick={() => {
                      openConversation(l.conversationId);
                      if (window.innerWidth < 1560) onClose();
                    }}
                  >
                    {A.open}
                  </button>
                )}
                <div className="spacer" />
                <button className="btn sm danger" onClick={() => cancel(l.taskId)}>
                  {A.stopTask}
                </button>
              </div>
            </div>
          );
        })}
      </section>
      <section className="tool-sec">
        <h4>{A.roster}</h4>
        {installed.length === 0 ? (
          <Empty>{A.none}</Empty>
        ) : (
          <ul className="tool-engines">
            {installed.map((e) => (
              <li key={e.id} className={busy.get(e.id) ? "busy" : ""}>
                <b>
                  <i className="agent-dot" aria-hidden /> {e.label}
                </b>
                <span title={e.version ?? undefined}>{busy.get(e.id) ? A.busy.replace("{n}", String(busy.get(e.id))) : `${A.ready}${e.version ? ` · ${e.version}` : ""}`}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
      {settings && (
        <section className="tool-sec">
          <h4>{A.preset}</h4>
          <Segmented
            label={A.preset}
            className="wide"
            value={agentChoice.value(settings)}
            options={agentChoice.options}
            onChange={(v) => saveSettings(agentChoice.apply(settings, v))}
          />
          <p className="tool-help">{agentChoice.help(settings)}</p>
        </section>
      )}
    </div>
  );
}

const TABS: { id: ToolTab; label: string; icon: ReactNode; tip: string }[] = [
  { id: "info", label: "세션 정보", icon: <InfoIcon />, tip: "세션 정보: 폴더·AI·토큰·비용·기억" },
  { id: "agents", label: A.title, icon: <AgentsIcon />, tip: A.tip },
  { id: "side", label: S.title, icon: <ChatIcon />, tip: S.tip },
  { id: "outline", label: "목차", icon: <ListIcon />, tip: "목차: 질문으로 바로 이동" },
  { id: "find", label: "찾기", icon: <SearchIcon />, tip: "세션에서 찾기 (Ctrl+F)" },
  { id: "export", label: "내보내기", icon: <DownloadIcon />, tip: "마크다운으로 복사·저장" },
];

const label = (id: unknown) => (typeof id === "string" ? (ENGINE_LABEL[id as EngineId] ?? id) : null);
const answeredBy = (m: Message | null) => (m?.data ? label(m.data.judgedBy) ?? label(m.data.reviewedBy) ?? label(m.data.draftedBy) : null);

/** 본문의 그 카드로 부드럽게 옮기고 잠깐 빛나게 한다. */
function jumpTo(key: string) {
  const el = document.getElementById(`ex-${key}`);
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: "start" });
  el.classList.remove("flash");
  void el.offsetWidth;
  el.classList.add("flash");
}

function scrollThread(to: "top" | "bottom") {
  const t = document.querySelector<HTMLElement>(".thread");
  t?.scrollTo({ top: to === "top" ? 0 : t.scrollHeight, behavior: "smooth" });
}

function tokens(n: number) {
  return n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n);
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="tool-empty">{children}</p>;
}

// ---------------------------------------------------------------------------

function InfoTab() {
  const { conversations, conversationId, messages, chooseFolder, toast } = useApp();
  const [mem, setMem] = useState<MemoryView | null>(null);
  const c = conversations.find((x) => x.id === conversationId);
  const load = () => {
    if (conversationId) api.memoryGet(conversationId).then(setMem).catch(() => setMem(null));
  };
  useEffect(load, [conversationId, messages.length]);
  useTauriEvent<{ conversationId: string }>("memory:updated", (p) => p.conversationId === conversationId && load());

  const stats = useMemo(() => {
    const by = new Map<string, { label: string; calls: number; tokens: number; cost: number; roles: Set<string> }>();
    let tokensAll = 0;
    let cost = 0;
    let ms = 0;
    for (const m of messages) {
      for (const s of m.data?.steps ?? []) {
        const t = (s.inputTokens ?? 0) + (s.outputTokens ?? 0);
        const e = by.get(s.engine) ?? { label: s.label, calls: 0, tokens: 0, cost: 0, roles: new Set<string>() };
        e.calls += 1;
        e.tokens += t;
        e.cost += s.costUsd ?? 0;
        e.roles.add(s.role);
        by.set(s.engine, e);
        tokensAll += t;
        cost += s.costUsd ?? 0;
        ms += s.ms;
      }
      cost += m.data?.jev?.costUsd ?? 0;
    }
    return { engines: [...by.values()].sort((a, b) => b.calls - a.calls), tokensAll, cost, ms };
  }, [messages]);

  if (!c) return <Empty>세션을 열면 폴더, 쓰인 AI, 토큰과 비용, 세션 기억이 여기에 보입니다.</Empty>;
  const asks = messages.filter((m) => m.role === "user").length;
  return (
    <div className="tool-body">
      <section className="tool-sec">
        <h4>작업 폴더</h4>
        <div className="tool-folder" title={c.workspaceDir ?? "기본 작업 폴더"}>
          <FolderIcon /> <span>{c.workspaceDir ? baseName(c.workspaceDir) : "기본 작업 폴더"}</span>
        </div>
        <div className="row">
          <button className="btn sm" onClick={() => api.openPath(`session:${c.id}`).catch((e) => toast(errorText(e), "error"))}>
            폴더 열기
          </button>
          <button className="btn sm ghost" onClick={() => chooseFolder(c.id)}>
            바꾸기
          </button>
        </div>
      </section>
      <section className="tool-sec">
        <h4>이 세션</h4>
        <div className="tool-stats">
          <div>
            <b>{asks}</b>
            <span>질문</span>
          </div>
          <div>
            <b>{tokens(stats.tokensAll)}</b>
            <span>토큰</span>
          </div>
          <div>
            <b>{stats.cost ? `$${stats.cost < 0.01 ? stats.cost.toFixed(4) : stats.cost.toFixed(2)}` : "$0"}</b>
            <span>추정 비용</span>
          </div>
          <div>
            <b>{stats.ms >= 60000 ? `${(stats.ms / 60000).toFixed(1)}분` : `${Math.round(stats.ms / 1000)}초`}</b>
            <span>AI 시간</span>
          </div>
        </div>
        {stats.engines.length > 0 && (
          <ul className="tool-engines">
            {stats.engines.map((e) => (
              <li key={e.label}>
                <b>{e.label}</b>
                <span>
                  {e.calls}회 · {[...e.roles].map((r) => ROLE_LABEL[r as keyof typeof ROLE_LABEL] ?? r).join("/")}
                  {e.tokens > 0 && ` · ${tokens(e.tokens)}`}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="tool-sec">
        <h4>세션 기억</h4>
        {mem?.summary ? <div className="tool-mem">{mem.summary}</div> : <Empty>아직 요약이 없습니다. 대화가 길어지면 오래된 부분을 요약해 모든 AI가 같이 씁니다.</Empty>}
      </section>
    </div>
  );
}

function OutlineTab() {
  const { messages, conversationId } = useApp();
  const ex = pair(messages).filter((e) => e.user);
  if (!conversationId || ex.length === 0) return <Empty>질문을 보내면 여기에 목차가 생깁니다. 누르면 그 답으로 이동합니다.</Empty>;
  return (
    <div className="tool-body">
      <div className="row tool-jump">
        <button className="btn sm ghost" onClick={() => scrollThread("top")}>
          <ArrowUpIcon /> 맨 위
        </button>
        <button className="btn sm ghost" onClick={() => scrollThread("bottom")}>
          <ArrowDownIcon /> 맨 아래
        </button>
      </div>
      <ol className="tool-outline">
        {ex.map((e, i) => (
          <li key={e.key} style={{ "--i": i } as CSSProperties}>
            <button onClick={() => jumpTo(e.key)}>
              <span className="tool-num">{i + 1}</span>
              <span className="tool-q">{e.user!.content}</span>
              <span className="tool-by">{e.answer ? answeredBy(e.answer) ?? "답함" : "답 기다리는 중"}</span>
            </button>
          </li>
        ))}
      </ol>
    </div>
  );
}

function highlight(text: string, q: string): ReactNode {
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return text.slice(0, 90);
  const start = Math.max(0, i - 30);
  return (
    <>
      {start > 0 && "…"}
      {text.slice(start, i)}
      <mark>{text.slice(i, i + q.length)}</mark>
      {text.slice(i + q.length, i + q.length + 60)}…
    </>
  );
}

function FindTab({ focusKey }: { focusKey: number }) {
  const { messages, conversationId } = useApp();
  const [q, setQ] = useState("");
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), [focusKey]);
  const ex = pair(messages);
  const hits = q.trim().length < 1 ? [] : ex.flatMap((e) => [e.user, e.answer].filter((m): m is Message => !!m && m.content.toLowerCase().includes(q.trim().toLowerCase())).map((m) => ({ key: e.key, m })));
  return (
    <div className="tool-body">
      <div className="tool-search">
        <SearchIcon />
        <input ref={input} type="text" value={q} onChange={(e) => setQ(e.target.value)} placeholder="이 세션에서 찾기" aria-label="세션에서 찾기" onKeyDown={(e) => e.key === "Enter" && hits[0] && jumpTo(hits[0].key)} />
      </div>
      {!conversationId ? (
        <Empty>세션을 열면 그 세션의 질문과 답에서 찾습니다.</Empty>
      ) : q.trim() === "" ? (
        <Empty>찾을 낱말을 입력하세요. Enter를 누르면 첫 결과로 이동합니다.</Empty>
      ) : hits.length === 0 ? (
        <Empty>"{q}"가 이 세션에 없습니다.</Empty>
      ) : (
        <>
          <p className="tool-count">{hits.length}곳</p>
          <ul className="tool-hits">
            {hits.map((h, i) => (
              <li key={`${h.m.id}-${i}`}>
                <button onClick={() => jumpTo(h.key)}>
                  <span className="tool-by">{h.m.role === "user" ? "질문" : `답${answeredBy(h.m) ? ` · ${answeredBy(h.m)}` : ""}`}</span>
                  <span className="tool-snip">{highlight(h.m.content, q.trim())}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

function SideTab() {
  const { side, askSide, stopSide, clearSide, engines, toast } = useApp();
  const [q, setQ] = useState("");
  const list = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const running = side.some((t) => t.state === "run");
  useEffect(() => input.current?.focus(), []);
  useEffect(() => {
    const el = list.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [side]);
  const submit = () => {
    const t = q.trim();
    if (!t || running) return;
    if (!engines.some((e) => e.installed)) return toast(A.none, "error");
    setQ("");
    void askSide(t);
  };
  return (
    <div className="tool-body side">
      <div className="side-list" ref={list}>
        {side.length === 0 && <Empty>{S.empty}</Empty>}
        {side.map((t) =>
          t.role === "user" ? (
            <div key={t.id} className="side-q">
              {t.text}
            </div>
          ) : (
            <div key={t.id} className={`side-a ${t.state ?? ""}`}>
              {t.engine && <div className="side-by">{label(t.engine)}</div>}
              {t.text && <Markdown text={t.text} />}
              {t.state === "run" && <div className="side-act">{t.activity ?? S.thinking}</div>}
              {t.state === "fail" && <div className={t.stopped ? "side-act" : "agent-err"}>{t.stopped ? S.stopped : t.error}</div>}
            </div>
          ),
        )}
      </div>
      <div className="side-box">
        <textarea
          ref={input}
          rows={2}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder={S.placeholder}
          aria-label={S.placeholder}
        />
        {running ? (
          <button className="icon" onClick={stopSide} aria-label={S.stop} title={S.stop}>
            <StopIcon />
          </button>
        ) : (
          <button className="icon" onClick={submit} disabled={!q.trim()} aria-label={S.send} title={S.send}>
            <SendIcon />
          </button>
        )}
      </div>
      <div className="side-foot">
        <span>{S.note}</span>
        {side.length > 0 && (
          <button className="btn sm ghost" onClick={clearSide}>
            <TrashIcon /> {S.clear}
          </button>
        )}
      </div>
    </div>
  );
}

function ExportTab() {
  const { conversationId, conversations, toast } = useApp();
  const c = conversations.find((x) => x.id === conversationId);
  const [busy, setBusy] = useState(false);
  if (!c) return <Empty>세션을 열면 그 세션의 질문과 답을 마크다운으로 복사하거나 파일로 저장할 수 있습니다.</Empty>;
  return (
    <div className="tool-body">
      <p className="tool-help">질문, 답(누가 답했는지), 세션 요약을 마크다운으로 만듭니다.</p>
      <button
        className="btn wide"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            await navigator.clipboard.writeText(await api.conversationMarkdown(c.id));
            toast("마크다운을 복사했습니다.");
          } catch (e) {
            toast(errorText(e), "error");
          } finally {
            setBusy(false);
          }
        }}
      >
        마크다운으로 복사
      </button>
      <button
        className="btn wide primary"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            const p = await api.conversationExport(c.id);
            toast(`저장했습니다: ${baseName(p)}`);
          } catch (e) {
            toast(errorText(e), "error");
          } finally {
            setBusy(false);
          }
        }}
      >
        작업 폴더에 .md로 저장
      </button>
      <p className="tool-help">저장 위치: 이 세션의 작업 폴더 안 "Elysium 내보내기". 저장하면 탐색기에서 파일을 보여 줍니다.</p>
    </div>
  );
}

// ---------------------------------------------------------------------------

/** 오른쪽 도구 도크(왼쪽 도크와 대칭) */
export function ToolDock({ tab, onTab }: { tab: ToolTab | null; onTab: (t: ToolTab | null) => void }) {
  const { live } = useApp();
  // 지금 돌고 있는 AI 수(에이전트 탭 배지)
  const running = live.reduce((n, l) => n + Object.values(l.agents).filter((a) => a.state === "run").length, 0);
  return (
    <nav className="dock tools-dock" aria-label="도구">
      {TABS.map((t) => (
        <button key={t.id} className={`dock-btn ${tab === t.id ? "on" : ""}`} onClick={() => onTab(tab === t.id ? null : t.id)} aria-label={t.label} aria-pressed={tab === t.id} data-tip={t.tip}>
          {t.icon}
          {t.id === "agents" && running > 0 && <span className="dock-count">{running}</span>}
        </button>
      ))}
    </nav>
  );
}

/** 오른쪽 도구 패널: 본문을 밀지 않고 위에 떠 있다. */
export function ToolPanel({ tab, onTab, focusKey }: { tab: ToolTab | null; onTab: (t: ToolTab | null) => void; focusKey: number }) {
  const [shown, setShown] = useState<ToolTab>(tab ?? "info");
  useEffect(() => {
    if (tab) setShown(tab);
  }, [tab]);
  const t = TABS.find((x) => x.id === shown)!;
  return (
    <aside className={`tools-panel ${tab ? "open" : ""}`} aria-label={t.label} aria-hidden={!tab} inert={!tab}>
      <div className="tools-inner" key={shown}>
        <div className="tools-head">
          <span className="tools-title">
            {t.icon} {t.label}
          </span>
          <button className="icon" onClick={() => onTab(null)} aria-label="도구 패널 닫기">
            <CloseIcon />
          </button>
        </div>
        {shown === "info" && <InfoTab />}
        {shown === "agents" && <AgentsTab onClose={() => onTab(null)} />}
        {shown === "side" && <SideTab />}
        {shown === "outline" && <OutlineTab />}
        {shown === "find" && <FindTab focusKey={focusKey} />}
        {shown === "export" && <ExportTab />}
      </div>
    </aside>
  );
}

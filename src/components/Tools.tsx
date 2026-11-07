import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { api, baseName, ENGINE_LABEL, errorText, type EngineId, type MemoryView } from "../lib/api";
import { useTauriEvent } from "../lib/events";
import { useApp, type AgentRun, type Live, type SideAsk } from "../store";
import { ChartIcon, ChatIcon, ChevronIcon, CloseIcon, FolderIcon, InfoIcon, SendIcon, StopIcon, TerminalIcon, TrashIcon } from "./Icons";
import TerminalTab, { T as TERM } from "./TerminalTab";
import Markdown from "./Markdown";
import { LeadSection, QueueCard, nowText } from "./LeadPanel";
import { AgentBoard, Grip, GroupMenu, useAgentShown, useBoardFocus } from "./AgentBoard";
import { groupOps } from "../lib/agentGroups";
import { BgJobs } from "./BgJobs";
import { PowerToggle } from "./PowerBanner";
import LimitsTab, { LIMIT_TEXT } from "./LimitsTab";
import { tailText } from "../lib/panel";
import { INTEGRATE, ROLE_LABEL } from "./Thread";

export type ToolTab = "info" | "limits" | "side" | "terminal";

/** 에이전트 탭 문구(설정 창도 같이 쓴다) */
export const A = {
  title: "에이전트",
  tip: "에이전트: 실행 중인 AI 현황 및 진행 상황",
  running: "실행 현황",
  idle: "현재 실행 중인 에이전트가 없습니다. 요청을 보내면 초안, 검토, 토론 등 작업 중인 AI의 상태가 여기에 표시됩니다.",
  roster: "사용 가능한 엔진",
  autoCount: "AI가 요청과 대화를 분석해 에이전트 수를 최대 64개까지 자동 결정합니다. 수를 고정하려면 '에이전트 3개로'처럼 메시지에 입력하시면 됩니다.",
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
  asked: "확인 중인 워커",
};

/** Status of one worker the lead asked */
const ASK_STATE: Record<SideAsk["state"], string> = {
  ask: "응답 대기 중",
  ok: "응답 완료",
  fail: "미응답 로그 참조",
  log: "활동 로그 참조",
  done: "작업 완료",
};

/** Workers the lead asked for this answer; each reply opens on click. */
function SideAsks({ asks }: { asks: SideAsk[] }) {
  return (
    <div className="side-asks">
      <div className="side-by">{S.asked}</div>
      {asks.map((a) => (
        <details key={a.key} className={`side-ask ${a.state}`}>
          <summary>
            <span className="side-ask-name">{a.title}</span>
            <span className="tag">{ASK_STATE[a.state]}</span>
          </summary>
          {a.state === "ok" && a.text && <Markdown text={a.text} />}
          {a.state === "fail" && a.text && <div className="agent-err">{a.text}</div>}
        </details>
      ))}
    </div>
  );
}

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
  const row = useRef<HTMLLIElement>(null);
  const shown = useAgentShown(a.key);
  useBoardFocus(a.key, row, () => setOpen(true));
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
  if (!shown) return null;
  return (
    <li ref={row} className={`agent-run ${a.state} ${open ? "open" : ""}`}>
      <div className="agent-row">
        <Grip keys={[a.key]} label={name} />
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
        <GroupMenu keys={[a.key]} label={name} />
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
              {a.activities.slice(-6).map((x, i) => (
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
                {tailText(text, 4000)}
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

/** Agent status board. Shown in the side panel, or as the main view next to the chat (`main`). */
export function AgentsTab({ onClose, main = false }: { onClose: () => void; main?: boolean }) {
  const { live, engines, conversations, conversationId, openConversation, cancel, cancelAll, approvals } = useApp();
  const [, tick] = useState(0);
  // 끝난 작업의 그룹은 지운다.
  useEffect(() => groupOps.prune(live.map((l) => l.taskId)), [live]);
  // 실행 중이면 경과 시간을 1초마다 다시 그린다.
  useEffect(() => {
    if (!live.length) return;
    const t = setInterval(() => tick((x) => x + 1), 1000);
    return () => clearInterval(t);
  }, [live.length]);
  const tasks = [...live].sort((a, b) => (main ? Number(b.conversationId === conversationId) - Number(a.conversationId === conversationId) : 0) || a.startedAt - b.startedAt);
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
          {main && <PowerToggle />}
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
              {l.lead && <LeadSection l={l} />}
              <AgentBoard
                l={l}
                runs={l.lead ? [] : runs}
                name={(a) => l.parts[a.key]?.title ?? a.label}
                now={nowText}
                row={(a) => (
                  <ul className="agent-runs">
                    <AgentRow l={l} a={a} now={now} />
                  </ul>
                )}
              >
                <ul className="agent-runs">
                  {runs.length === 0 && <li className="agent-run wait">{waiting ? A.approval : A.preparing}</li>}
                  {runs
                    .filter((a) => !l.lead || (!a.key.startsWith("agent:") && !a.key.startsWith("lead")))
                    .map((a) => (
                      <AgentRow key={a.key} l={l} a={a} now={now} />
                    ))}
                </ul>
              </AgentBoard>
              <BgJobs l={l} />
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
      <QueueCard />
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
        <p className="tool-help">{A.autoCount}</p>
      </section>
    </div>
  );
}

const TABS: { id: ToolTab; label: string; icon: ReactNode; tip: string }[] = [
  { id: "info", label: "세션 정보", icon: <InfoIcon />, tip: "세션 정보: 폴더·AI·토큰·비용·기억" },
  { id: "limits", label: LIMIT_TEXT.title, icon: <ChartIcon />, tip: LIMIT_TEXT.tip },
  { id: "side", label: S.title, icon: <ChatIcon />, tip: S.tip },
  { id: "terminal", label: TERM.title, icon: <TerminalIcon />, tip: TERM.tip },
];

const label = (id: unknown) => (typeof id === "string" ? (ENGINE_LABEL[id as EngineId] ?? id) : null);

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
              {t.asks && t.asks.length > 0 && <SideAsks asks={t.asks} />}
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

// ---------------------------------------------------------------------------

/** 오른쪽 도구 도크(왼쪽 도크와 대칭) */
export function ToolDock({ tab, onTab }: { tab: ToolTab | null; onTab: (t: ToolTab | null) => void }) {
  return (
    <nav className="dock tools-dock" aria-label="도구">
      {TABS.map((t) => (
        <button key={t.id} className={`dock-btn ${tab === t.id ? "on" : ""}`} onClick={() => onTab(tab === t.id ? null : t.id)} aria-label={t.label} aria-pressed={tab === t.id} data-tip={t.tip}>
          {t.icon}
        </button>
      ))}
    </nav>
  );
}

/** 오른쪽 도구 패널: 본문을 밀지 않고 위에 떠 있다. */
export function ToolPanel({ tab, onTab, onResizeStart, onResizeKey }: { tab: ToolTab | null; onTab: (t: ToolTab | null) => void; onResizeStart: (e: React.PointerEvent) => void; onResizeKey: (e: React.KeyboardEvent) => void }) {
  const [shown, setShown] = useState<ToolTab>(tab ?? "info");
  useEffect(() => {
    if (tab) setShown(tab);
  }, [tab]);
  const t = TABS.find((x) => x.id === shown)!;
  return (
    <aside className={`tools-panel ${tab ? "open" : ""}`} aria-label={t.label} aria-hidden={!tab} inert={!tab}>
      <div className="tools-resize" role="separator" aria-orientation="vertical" tabIndex={0} onPointerDown={onResizeStart} onKeyDown={onResizeKey} />
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
        {shown === "limits" && <LimitsTab />}
        {shown === "side" && <SideTab />}
        {shown === "terminal" && <TerminalTab />}
      </div>
    </aside>
  );
}

// 폰에서 보는 실시간 작업: 에이전트(상태·지금 하는 일·토큰·멈추기·메시지), 곁질문, 토큰 칩.
import { useEffect, useRef, useState } from "react";
import { ENGINE_LABEL, type EngineId } from "../../src/lib/api";
import { shortTokens, sumUsage, total, usd, type RunUsage, type Tok } from "../../src/lib/usage";
import { hideBlocks } from "../../src/lib/panel";
import { doneRatio, phaseOf, workerCounts, type Phase } from "../../src/lib/progress";
import type { LeadNoteLite } from "./remote";
import Markdown from "./Markdown";
import { errorText, remote } from "./remote";

export const T = {
  tokens: "토큰 사용량",
  agents: "에이전트",
  noAgents: "실행 중인 에이전트가 없습니다.",
  stop: "중지",
  toAgent: "에이전트에게",
  toLead: "총괄에게",
  messagePlaceholder: "지시할 내용을 입력하세요",
  send: "전송",
  sent: "지시를 전달했습니다.",
  working: "작업 중",
  done: "완료",
  failed: "오류",
  idle: "메시지 대기 중",
  waiting: "대기 중",
  thinking: "생각 중",
  side: "곁질문",
  sideHint: "이곳의 질문은 대화에 저장되거나 공유되지 않습니다.",
  sidePlaceholder: "작업에 대해 궁금한 점을 물어보세요",
  close: "닫기",
  lead: "총괄",
  teamWorking: "{n}개 에이전트 작업 중",
  preparing: "준비 중",
  stopTask: "작업 중지",
  toAll: "전체에게",
  taskPlaceholder: "작업 지시사항을 입력해 주세요",
  task: "작업",
  progress: "진행 현황",
  phaseSplit: "작업 분할",
  phaseWork: "작업 수행",
  phaseIntegrate: "결과 통합",
  cntDone: "완료 {n}",
  cntRun: "진행 {n}",
  cntWait: "대기 {n}",
  cntFail: "실패 {n}",
  pctDone: "{p}% 완료",
  commits: "커밋 {n}개",
  leadRecent: "총괄 최근 활동",
  tools: "도구 {n}회",
  bgRunning: "명령 {n}개 실행 중",
};

export interface AgentState {
  key: string;
  engine: EngineId | null;
  label: string;
  role: string;
  state: "run" | "ok" | "fail";
  wait: string | null;
  last: string | null;
  stopped: boolean;
  /** 도구를 쓴 횟수 */
  tools?: number;
  /** 백그라운드에서 도는 명령 수 */
  bg?: number;
}

export interface SideTurn {
  id: string;
  role: "user" | "assistant";
  text: string;
  state?: "run" | "ok" | "fail";
  activity?: string | null;
}

export function TokenChip({ t, live }: { t: Tok; live?: boolean }) {
  if (t.input + t.output === 0) return null;
  return (
    <span className={`tok ${live ? "live" : ""}`}>
      {live && <i aria-hidden />}
      <b>{T.tokens}</b> ↑{shortTokens(t.input)} ↓{shortTokens(t.output)}
      {t.cost > 0 && <em>{usd(t.cost)}</em>}
    </span>
  );
}

/** 하위 작업이 아닌 실행의 이름(PC 화면과 같은 말) */
const ROLE_NAME: Record<string, string> = { plan: "분할자", roadmap: "배정자", synth: "통합자", lead: "총괄", draft: "초안", review: "검토", judge: "판정", debate: "토론", rebuttal: "반론", council: "회의", finish: "보완" };

export function agentTitle(a: AgentState, parts: Record<string, string>): string {
  return parts[a.key] ?? ROLE_NAME[a.role] ?? a.label;
}

/** 지금 상태 + 백그라운드 명령 수 */
export function agentStatus(a: AgentState): string {
  if (a.stopped || a.state === "fail") return T.failed;
  if (a.state === "ok") return T.done;
  const now = a.wait === "idle" ? T.idle : a.wait ? T.waiting : T.working;
  return a.bg ? `${now} · ${T.bgRunning.replace("{n}", String(a.bg))}` : now;
}

/** 작업 하나의 에이전트 줄(카드 안과 시트에서 같이 쓴다) */
export function AgentRow({ a, title, usage, onOpen }: { a: AgentState; title: string; usage?: RunUsage; onOpen?: () => void }) {
  const live = a.state === "run" && a.wait !== "idle";
  const [tool, detail] = a.last && a.last.indexOf(" · ") > 0 ? [a.last.slice(0, a.last.indexOf(" · ")), a.last.slice(a.last.indexOf(" · ") + 3)] : [null, a.last];
  return (
    <button className={`agent ${a.state} ${a.wait ? "waiting" : ""}`} onClick={onOpen} disabled={!onOpen}>
      <i className="dot" aria-hidden />
      <span className="agent-main">
        <span className="agent-top">
          <b>{title}</b>
          <span className={`agent-state ${a.state} ${a.wait ? "wait" : ""}`}>{agentStatus(a)}</span>
        </span>
        <span className="agent-sub">
          {ENGINE_LABEL[a.engine as EngineId] ?? a.label}
          {a.tools ? ` · ${T.tools.replace("{n}", String(a.tools))}` : ""}
        </span>
        {a.state === "run" && !a.wait && (
          <span className="agent-now">
            {tool && <em>{tool}</em>}
            <span>{detail ?? T.thinking}</span>
          </span>
        )}
        <TokenChip t={total(usage)} live={live} />
      </span>
    </button>
  );
}

export interface TaskAgents {
  taskId: string;
  lead: boolean;
  agents: Record<string, AgentState>;
  parts: Record<string, string>;
  usage: Record<string, RunUsage>;
  streams: Record<string, string>;
  timeline: LeadNoteLite[];
  commits: number;
}

const PHASES: Phase[] = ["split", "work", "integrate"];

/** 작업 전체의 진행: 단계, 워커 상태 막대, 커밋, 총괄 최근 활동 */
export function ProgressBox({ t }: { t: TaskAgents }) {
  const runs = Object.values(t.agents);
  if (!runs.length) return null;
  const c = workerCounts(runs);
  const phase = phaseOf(runs);
  const at = phase === "done" ? PHASES.length : PHASES.indexOf(phase);
  const name: Record<Phase, string> = { split: T.phaseSplit, work: T.phaseWork, integrate: T.phaseIntegrate, done: T.done };
  const w = (n: number) => `${c.total ? (n / c.total) * 100 : 0}%`;
  return (
    <div className="progress">
      <div className="progress-head">
        <b>{T.progress}</b>
        {t.lead && (
          <ol className="phase-steps">
            {PHASES.map((p, i) => (
              <li key={p} className={i < at ? "done" : i === at ? "now" : ""}>
                {name[p]}
              </li>
            ))}
          </ol>
        )}
        <span className="progress-pct">{T.pctDone.replace("{p}", String(Math.round(doneRatio(c) * 100)))}</span>
      </div>
      <div className="progress-bar">
        <i className="ok" style={{ width: w(c.ok) }} />
        <i className="run" style={{ width: w(c.run) }} />
        <i className="wait" style={{ width: w(c.wait) }} />
        <i className="fail" style={{ width: w(c.fail) }} />
      </div>
      <div className="progress-legend">
        <span className="ok">{T.cntDone.replace("{n}", String(c.ok))}</span>
        <span className="run">{T.cntRun.replace("{n}", String(c.run))}</span>
        <span className="wait">{T.cntWait.replace("{n}", String(c.wait))}</span>
        {c.fail > 0 && <span className="fail">{T.cntFail.replace("{n}", String(c.fail))}</span>}
        {t.lead && <span className="commit">{T.commits.replace("{n}", String(t.commits))}</span>}
      </div>
      {t.timeline.length > 0 && (
        <div className="progress-lead">
          <em>{T.leadRecent}</em>
          <ul>
            {t.timeline
              .slice(-4)
              .reverse()
              .map((n, i) => (
                <li key={`${n.at}-${i}`} className={`k-${n.kind}`}>
                  {n.text}
                </li>
              ))}
          </ul>
        </div>
      )}
    </div>
  );
}

/** 닫힌 뒤에도 닫히는 움직임이 끝날 때까지만 true */
function useLinger(open: boolean, ms = 500): boolean {
  const [on, setOn] = useState(open);
  useEffect(() => {
    if (open) return setOn(true);
    const t = window.setTimeout(() => setOn(false), ms);
    return () => window.clearTimeout(t);
  }, [open, ms]);
  return open || on;
}

/** 아래에서 올라오는 시트. 닫혀 있으면 안을 그리지 않는다(실시간 글이 바뀔 때마다 숨은 목록을 다시 그리지 않게). */
function Sheet({ title, open, onClose, children }: { title: string; open: boolean; onClose: () => void; children: React.ReactNode }) {
  const keep = useLinger(open);
  return (
    <>
      <div className={`sheet-back ${open ? "open" : ""}`} onClick={onClose} />
      <section className={`sheet ${open ? "open" : ""}`} aria-hidden={!open} inert={!open} role="dialog" aria-label={title}>
        <header className="sheet-head">
          <i className="sheet-grip" aria-hidden />
          <h3>{title}</h3>
          <button className="icon-btn sm" aria-label={T.close} onClick={onClose}>
            ×
          </button>
        </header>
        <div className="sheet-body">{keep ? children : null}</div>
      </section>
    </>
  );
}

/** 에이전트 시트: 모든 작업의 에이전트 → 하나를 고르면 지금 쓰는 글·멈추기·메시지 */
export function AgentSheet({ open, onClose, tasks, toast }: { open: boolean; onClose: () => void; tasks: TaskAgents[]; toast: (t: string, k?: "info" | "error") => void }) {
  const [pick, setPick] = useState<{ taskId: string; key: string } | null>(null);
  const [msg, setMsg] = useState("");
  const [toLead, setToLead] = useState(false);
  const out = useRef<HTMLPreElement>(null);
  const task = pick ? tasks.find((t) => t.taskId === pick.taskId) : undefined;
  const a = pick && task ? task.agents[pick.key] : undefined;
  const text = pick && task ? hideBlocks((task.streams[pick.key] ?? "").slice(-6000)) : "";
  useEffect(() => {
    if (out.current) out.current.scrollTop = out.current.scrollHeight;
  }, [text]);
  useEffect(() => {
    if (!open) setPick(null);
  }, [open]);
  const send = async () => {
    const t = msg.trim();
    if (!t || !pick) return;
    try {
      if (toLead) await remote.leadSend(pick.taskId, t, pick.key);
      else await remote.agentMessage(pick.taskId, pick.key, t);
      setMsg("");
      toast(T.sent);
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  const all = tasks.flatMap((t) => Object.values(t.agents).map((x) => ({ t, a: x })));
  return (
    <Sheet title={a && task ? agentTitle(a, task.parts) : T.agents} open={open} onClose={pick ? () => setPick(null) : onClose}>
      {!a || !task ? (
        all.length === 0 ? (
          <p className="sheet-empty">{T.noAgents}</p>
        ) : (
          <div className="agents">
            {tasks.map((t) => (
              <div key={t.taskId} className="agent-group">
                <div className="agent-group-head">
                  <b>{T.task}</b>
                  {t.lead && <span className="pill">{T.lead}</span>}
                  <TokenChip t={sumUsage(t.usage)} live />
                  <button className="ghost-btn sm danger push" onClick={() => remote.cancel(t.taskId).catch((e) => toast(errorText(e), "error"))}>
                    {T.stopTask}
                  </button>
                </div>
                <ProgressBox t={t} />
                {Object.values(t.agents).map((x) => (
                  <AgentRow key={x.key} a={x} title={agentTitle(x, t.parts)} usage={t.usage[x.key]} onOpen={() => setPick({ taskId: t.taskId, key: x.key })} />
                ))}
                <TaskBox t={t} toast={toast} />
              </div>
            ))}
          </div>
        )
      ) : (
        <div className="agent-detail">
          <AgentRow a={a} title={agentTitle(a, task.parts)} usage={task.usage[a.key]} />
          {text && (
            <pre ref={out} className="agent-out">
              {text}
            </pre>
          )}
          {a.state === "run" && (
            <button className="ghost-btn danger" onClick={() => remote.agentCancel(task.taskId, a.key).catch((e) => toast(errorText(e), "error"))}>
              {T.stop}
            </button>
          )}
          {task.lead && (
            <div className="seg-mini" role="radiogroup">
              <button className={!toLead ? "on" : ""} onClick={() => setToLead(false)} role="radio" aria-checked={!toLead}>
                {T.toAgent}
              </button>
              <button className={toLead ? "on" : ""} onClick={() => setToLead(true)} role="radio" aria-checked={toLead}>
                {T.toLead}
              </button>
            </div>
          )}
          <div className="sheet-input">
            <input value={msg} onChange={(e) => setMsg(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && send()} placeholder={T.messagePlaceholder} />
            <button className="send" aria-label={T.send} disabled={!msg.trim()} onClick={send}>
              ↑
            </button>
          </div>
        </div>
      )}
    </Sheet>
  );
}

/** 작업 전체에 보내기: 총괄이 있으면 총괄에게(기본) 또는 모든 에이전트에게 */
function TaskBox({ t, toast }: { t: TaskAgents; toast: (t: string, k?: "info" | "error") => void }) {
  const [msg, setMsg] = useState("");
  const [toLead, setToLead] = useState(t.lead);
  const send = async () => {
    const text = msg.trim();
    if (!text) return;
    try {
      if (toLead && t.lead) await remote.leadSend(t.taskId, text, null);
      else await remote.taskMessage(t.taskId, text);
      setMsg("");
      toast(T.sent);
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  return (
    <div className="task-box">
      {t.lead && (
        <div className="seg-mini" role="radiogroup">
          <button className={toLead ? "on" : ""} onClick={() => setToLead(true)} role="radio" aria-checked={toLead}>
            {T.toLead}
          </button>
          <button className={!toLead ? "on" : ""} onClick={() => setToLead(false)} role="radio" aria-checked={!toLead}>
            {T.toAll}
          </button>
        </div>
      )}
      <div className="sheet-input">
        <input value={msg} onChange={(e) => setMsg(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && send()} placeholder={T.taskPlaceholder} />
        <button className="send" aria-label={T.send} disabled={!msg.trim()} onClick={send}>
          ↑
        </button>
      </div>
    </div>
  );
}

/** 곁질문 시트: 대화에 남지 않는 짧은 질문. 답은 side:* 이벤트로 흘러온다. */
export function SideSheet({ open, onClose, thread, onAsk, onStop }: { open: boolean; onClose: () => void; thread: SideTurn[]; onAsk: (q: string) => void; onStop: (id: string) => void }) {
  const [q, setQ] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const running = thread.find((t) => t.state === "run");
  useEffect(() => {
    if (box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [thread]);
  const ask = () => {
    const t = q.trim();
    if (!t || running) return;
    setQ("");
    onAsk(t);
  };
  return (
    <Sheet title={T.side} open={open} onClose={onClose}>
      <p className="sheet-hint">{T.sideHint}</p>
      <div className="side-thread" ref={box}>
        {thread.map((t) =>
          t.role === "user" ? (
            <div key={t.id} className="side-q">
              {t.text}
            </div>
          ) : (
            <div key={t.id} className={`side-a ${t.state ?? ""}`}>
              {t.text ? <Markdown text={t.text} /> : <span className="agent-now">{t.activity ?? T.thinking}</span>}
              {t.state === "run" && (
                <button className="ghost-btn sm" onClick={() => onStop(t.id)}>
                  {T.stop}
                </button>
              )}
            </div>
          ),
        )}
      </div>
      <div className="sheet-input">
        <input value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && ask()} placeholder={T.sidePlaceholder} />
        <button className="send" aria-label={T.send} disabled={!q.trim() || !!running} onClick={ask}>
          ↑
        </button>
      </div>
    </Sheet>
  );
}

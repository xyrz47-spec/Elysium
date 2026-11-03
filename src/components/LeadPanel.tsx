// 병렬 작업의 총괄 화면: 총괄 카드, 워커·그룹 카드, 게임 테스트 대기열, 명령 판단 기록
import { useEffect, useRef, useState, type ReactNode } from "react";
import { api, errorText, type GroupNote, type LeadDecision } from "../lib/api";
import { hideBlocks } from "../lib/panel";
import { useApp, type AgentRun, type Live } from "../store";
import { SendIcon, StopIcon } from "./Icons";


export const LT = {
  lead: "총괄 AI",
  leadPlaceholder: "수정이나 제외할 작업을 입력하세요",
  send: "전송",
  asks: "질문이 있습니다",
  answerPlaceholder: "답변을 입력해 주세요",
  you: "사용자",
  report: "문제 보고하기",
  reportPlaceholder: "어떤 문제가 발생했는지 설명해 주세요",
  stop: "중지",
  openWindow: "새 창에서 열기",
  group: "작업그룹",
  member: "작업자",
  noTalk: "대화가 아직 시작되지 않았습니다.",
  waitReview: "검토 대기 중",
  waitQuestion: "답변 대기 중",
  waitTest: "테스트 대기 중",
  waitCommand: "명령 실행 중",
  working: "작업 중",
  finished: "완료",
  failed: "중단됨",
  queue: "게임 테스트 대기열",
  slots: "동시 게임 테스트 수",
  slotsHelp: "게임 창 간섭 방지를 위해 1개를 권장하며, 초과 테스트는 순서대로 대기합니다.",
  running: "실행 중",
  position: "대기 {n}번",
  stopTest: "테스트 중지",
  emptyQueue: "대기 중인 테스트가 없습니다.",
  decisions: "명령 검토 내역",
  noDecisions: "아직 요청된 명령이 없습니다.",
  once: "이번만 허용",
  deny: "거부",
  similar: "유사 명령 자동 허용",
  all: "모든 명령 자동 허용",
  byJev: "JEV 판단",
  byRule: "규칙에 따라 허용",
  byLead: "총괄 AI 결정",
  autoOn: "작업자 명령이 검토 없이 사용자 권한으로 실행됩니다.",
  revoke: "자동 허용 해제",
  winOutput: "작성 내용",
  winActivity: "사용 도구",
  winFiles: "담당 파일",
  winTalk: "그룹 대화",
  winNoOutput: "아직 작성된 내용이 없습니다.",
  winCut: "이전 출력 내용이 생략되었습니다.",
  winConnecting: "상태를 불러오는 중...",
  savedLog: "명령 검토 및 대화 내역",
  waitMail: "메시지 대기 중",
  winNow: "현재 작업",
  winThinking: "생각하는 중",
  winLive: "실시간 출력",
  winToBottom: "최신으로 이동",
  winToWorker: "워커에게",
  winToLead: "총괄에게",
  winWorkerPlaceholder: "워커에게 전달할 지시를 입력하세요",
  winTimeline: "활동 내역",
  winSendTarget: "메시지 수신 대상",
  elapsed: "작업 시간",
};


export const VERDICT: Record<LeadDecision["verdict"], string> = { once: LT.once, deny: LT.deny, similar: LT.similar, all: LT.all, ask_user: LT.once };
const WAIT: Record<string, string> = { review: LT.waitReview, question: LT.waitQuestion, test: LT.waitTest, command: LT.waitCommand, idle: LT.waitMail };

export function runState(a: AgentRun): string {
  if (a.state === "run") return (a.wait && WAIT[a.wait]) || LT.working;
  return a.state === "ok" ? LT.finished : LT.failed;
}

/** 1초마다 다시 그려 경과 시간을 보여 준다(실행 중일 때만 돈다). */
function useNow(on: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!on) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [on]);
  return now;
}

function clock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number) => String(n).padStart(2, "0");
  return s >= 3600 ? `${Math.floor(s / 3600)}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}` : `${two(Math.floor(s / 60))}:${two(s % 60)}`;
}

/** 워커가 일한 시간. 끝났으면 걸린 시간에서 멈춘다. */
export function Elapsed({ run }: { run: AgentRun }) {
  const going = run.state === "run" && run.wait !== "idle";
  const now = useNow(going);
  // 일하는 모습을 본 뒤 멈췄으면 그 시각까지의 전체 시간으로 멈춘다(여러 차례 일한 워커). 처음부터 끝난 채로 봤으면 기록된 시간을 쓴다.
  const stoppedAt = useRef<number | null>(null);
  const saw = useRef(false);
  if (going) {
    saw.current = true;
    stoppedAt.current = null;
  } else if (saw.current) stoppedAt.current ??= now;
  const end = stoppedAt.current;
  const ms = going ? now - run.startedAt : end != null ? end - run.startedAt : (run.ms ?? 0);
  return (
    <span className="worker-clock" title={LT.elapsed} aria-label={`${LT.elapsed} ${clock(ms)}`}>
      {clock(ms)}
    </span>
  );
}

/** "PowerShell · .\gradlew build" → 도구와 대상 */
export function splitActivity(a: string): { tool: string | null; detail: string } {
  const i = a.indexOf(" · ");
  return i > 0 && i <= 24 ? { tool: a.slice(0, i), detail: a.slice(i + 3) } : { tool: null, detail: a };
}

/** 지금 하는 일: 기다리는 이유, 마지막 도구 사용, 아니면 생각하는 중 */
export function NowLine({ run, big = false }: { run: AgentRun; big?: boolean }) {
  if (run.state !== "run") return null;
  const waiting = run.wait ? WAIT[run.wait] : null;
  const last = run.activities[run.activities.length - 1];
  const { tool, detail } = waiting ? { tool: null, detail: waiting } : last ? splitActivity(last) : { tool: null, detail: LT.winThinking };
  return (
    <div className={`worker-now ${big ? "big" : ""} ${waiting ? "wait" : ""}`} aria-live="polite">
      <i className="worker-now-pulse" aria-hidden />
      {tool && <span className="tag tool">{tool}</span>}
      <span className="worker-now-text" title={detail}>
        {detail}
      </span>
    </div>
  );
}

/** 실시간 출력의 마지막 몇 줄(긴 글 전체를 매번 다루지 않게 끝부분만 본다) */
export function tailLines(text: string | undefined, n: number): string {
  if (!text) return "";
  return hideBlocks(text.slice(-4000))
    .split("\n")
    .map((x) => x.trimEnd())
    .filter(Boolean)
    .slice(-n)
    .join("\n");
}

function byLabel(by: LeadDecision["by"]): string | null {
  if (by === "jev") return LT.byJev;
  if (by === "rule" || by === "all") return LT.byRule;
  if (by === "lead") return LT.byLead;
  return null;
}

/** 줄 하나짜리 입력과 보내기 버튼 */
function LineBox({ placeholder, label, onSend, autoFocus }: { placeholder: string; label: string; onSend: (t: string) => Promise<unknown>; autoFocus?: boolean }) {
  const { toast } = useApp();
  const [msg, setMsg] = useState("");
  const go = async () => {
    const t = msg.trim();
    if (!t) return;
    setMsg("");
    await onSend(t).catch((e) => {
      setMsg(t);
      toast(errorText(e), "error");
    });
  };
  return (
    <div className="agent-talk">
      <input type="text" autoFocus={autoFocus} value={msg} onChange={(e) => setMsg(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && (e.preventDefault(), go())} placeholder={placeholder} aria-label={label} />
      <button className="icon" onClick={go} disabled={!msg.trim()} aria-label={label} title={label}>
        <SendIcon />
      </button>
    </div>
  );
}

export function QuestionCard({ id, question, options }: { id: string; question: string; options: string[] }) {
  const { toast } = useApp();
  const answer = (text: string) => api.leadAnswer(id, text).catch((e) => toast(errorText(e), "error"));
  return (
    <div className="lead-q">
      <b>{LT.asks}</b>
      <p>{question}</p>
      {options.length > 0 && (
        <div className="lead-q-opts">
          {options.map((o) => (
            <button key={o} className="btn sm" onClick={() => answer(o)}>
              {o}
            </button>
          ))}
        </div>
      )}
      <LineBox placeholder={LT.answerPlaceholder} label={LT.send} onSend={answer} />
    </div>
  );
}

function LeadCard({ l }: { l: Live }) {
  const lead = l.lead!;
  return (
    <div className="lead-card">
      <div className="lead-line">
        <i className="agent-dot on" aria-hidden />
        <b>{LT.lead}</b>
        <span className="agent-role">{[lead.label, lead.model].filter(Boolean).join(" · ")}</span>
      </div>
      {lead.questions.map((q) => (
        <QuestionCard key={q.id} id={q.id} question={q.question} options={q.options} />
      ))}
      {lead.said.length > 0 && (
        <ul className="lead-said">
          {lead.said.slice(-6).map((s, i) => (
            <li key={i} className={s.who}>
              <b>{s.who === "user" ? LT.you : LT.lead}</b> {s.text}
            </li>
          ))}
        </ul>
      )}
      <LineBox placeholder={LT.leadPlaceholder} label={LT.send} onSend={(t) => api.leadSend(l.taskId, t, null)} />
    </div>
  );
}

function CardActions({ l, about, stopKeys, windowKey, title }: { l: Live; about: string; stopKeys: string[]; windowKey: string; title: string }) {
  const { cancelAgent, toast } = useApp();
  const [reporting, setReporting] = useState(false);
  return (
    <>
      <div className="lead-actions">
        <button className="btn sm ghost" onClick={() => setReporting((r) => !r)} aria-expanded={reporting}>
          {LT.report}
        </button>
        <button className="btn sm ghost" onClick={() => api.workerWindowOpen(l.taskId, windowKey, title).catch((e) => toast(errorText(e), "error"))}>
          {LT.openWindow}
        </button>
        <div className="spacer" />
        <button className="btn sm danger" onClick={() => stopKeys.forEach((k) => cancelAgent(l.taskId, k))} aria-label={`${LT.stop} · ${title}`}>
          <StopIcon /> {LT.stop}
        </button>
      </div>
      {reporting && (
        <LineBox
          autoFocus
          placeholder={LT.reportPlaceholder}
          label={LT.report}
          onSend={async (t) => {
            await api.leadSend(l.taskId, t, about);
            setReporting(false);
          }}
        />
      )}
    </>
  );
}

function WorkerCard({ l, a }: { l: Live; a: AgentRun }) {
  const title = l.parts[a.key]?.title ?? a.label;
  const last = a.activities[a.activities.length - 1];
  const tail = a.state === "run" && a.wait !== "idle" ? tailLines(l.streams[a.key], 3) : "";
  return (
    <div className={`lead-worker ${a.state}`}>
      <div className="lead-line">
        <i className="agent-dot" aria-hidden />
        <b title={title}>{title}</b>
        <div className="spacer" />
        <Elapsed run={a} />
      </div>
      <div className="lead-line sub">
        <span className="agent-role">{[a.label, a.effort].filter(Boolean).join(" · ")}</span>
        <span className={`agent-state ${a.wait ? "wait" : ""}`}>{a.stopped ? LT.failed : runState(a)}</span>
      </div>
      {a.state === "run" ? <NowLine run={a} /> : last && <div className="agent-act">{last}</div>}
      {tail && <pre className="live-tail">{tail}</pre>}
      {a.state === "fail" && a.error && !a.stopped && <div className="agent-err">{a.error}</div>}
      {a.state !== "fail" && <CardActions l={l} about={a.key} stopKeys={[a.key]} windowKey={a.key} title={title} />}
    </div>
  );
}

function GroupCard({ l, index, title, keys }: { l: Live; index: number; title: string; keys: string[] }) {
  const talk = l.lead!.groupLog.filter((g) => g.group === index).slice(-5);
  const members = keys.map((k) => l.agents[k]).filter(Boolean);
  return (
    <div className="lead-worker group">
      <div className="lead-line">
        <span className="tag">{LT.group}</span>
        <b title={title}>{title}</b>
      </div>
      <div className="lead-members">
        {members.map((a, i) => (
          <span key={a.key} className={`lead-member ${a.state}`}>
            <i className="agent-dot" aria-hidden /> {LT.member} {i + 1} · {a.stopped ? LT.failed : runState(a)}
          </span>
        ))}
      </div>
      {talk.length === 0 ? (
        <div className="agent-sub">{LT.noTalk}</div>
      ) : (
        <ul className="lead-said">
          {talk.map((g, i) => (
            <li key={i}>
              <b>{g.who}</b> {g.text}
            </li>
          ))}
        </ul>
      )}
      <CardActions l={l} about={keys[0]} stopKeys={keys} windowKey={`group:${index}:${keys.join(",")}`} title={title} />
    </div>
  );
}

function DecisionLog({ l }: { l: Live }) {
  const { toast } = useApp();
  const lead = l.lead!;
  const auto = lead.policy.allowAll || lead.policy.rules.length > 0;
  return (
    <div className="lead-log">
      <div className="lead-line">
        <b>{LT.decisions}</b>
        {auto && (
          <button className="btn sm danger lead-revoke" onClick={() => api.leadRevoke(l.taskId).catch((e) => toast(errorText(e), "error"))}>
            {LT.revoke}
          </button>
        )}
      </div>
      {auto && <p className="lead-warn">{LT.autoOn}</p>}
      {lead.decisions.length === 0 ? <div className="agent-sub">{LT.noDecisions}</div> : <DecisionRows items={lead.decisions.slice(-30).reverse()} />}
    </div>
  );
}

function DecisionRows({ items }: { items: LeadDecision[] }) {
  return (
    <ul className="lead-decisions">
      {items.map((d) => (
        <li key={d.id}>
          <span className={`tag verdict ${d.verdict}`}>{VERDICT[d.verdict]}</span>
          <code title={d.command}>{d.pattern ?? d.command}</code>
          {byLabel(d.by) && <span className="agent-sub">{byLabel(d.by)}</span>}
          {d.reason && <div className="agent-sub lead-reason">{d.reason}</div>}
        </li>
      ))}
    </ul>
  );
}

/** 총괄이 있는 작업의 내용 */
export function LeadSection({ l }: { l: Live }) {
  const lead = l.lead!;
  const inGroup = new Set(lead.groups.flatMap((g) => g.keys));
  const solo = Object.values(l.agents)
    .filter((a) => a.key.startsWith("agent:") && !inGroup.has(a.key))
    .sort((a, b) => a.key.localeCompare(b.key));
  return (
    <>
      <LeadCard l={l} />
      <div className="lead-workers">
        {lead.groups.map((g) => (
          <GroupCard key={g.index} l={l} index={g.index} title={g.title} keys={g.keys} />
        ))}
        {solo.map((a) => (
          <WorkerCard key={a.key} l={l} a={a} />
        ))}
      </div>
      <DecisionLog l={l} />
    </>
  );
}

const SLOT_CHOICES = [1, 2, 3, 4, 5, 6, 7, 8];

export function SlotsPicker() {
  const { settings, saveSettings } = useApp();
  if (!settings) return null;
  return (
    <label className="lead-slots">
      <span>{LT.slots}</span>
      <select value={settings.testSlots ?? 1} onChange={(e) => saveSettings({ ...settings, testSlots: Number(e.target.value) })} aria-label={LT.slots}>
        {SLOT_CHOICES.map((n) => (
          <option key={n} value={n}>
            {n}
          </option>
        ))}
      </select>
    </label>
  );
}

export function QueueCard() {
  const { queue, toast } = useApp();
  const stop = (id: number) => api.testCancel(id).catch((e) => toast(errorText(e), "error"));
  return (
    <section className="tool-sec">
      <h4 className="lead-queue-head">
        {LT.queue}
        <SlotsPicker />
      </h4>
      {queue.running.length + queue.waiting.length === 0 && <div className="agent-sub">{LT.emptyQueue}</div>}
      <ul className="lead-queue">
        {queue.running.map((t) => (
          <li key={t.id}>
            <span className="tag">{LT.running}</span>
            <code title={t.command}>{t.command}</code>
            <button className="btn sm danger" onClick={() => stop(t.id)}>
              {LT.stopTest}
            </button>
          </li>
        ))}
        {queue.waiting.map((t, i) => (
          <li key={t.id}>
            <span className="tag">{LT.position.replace("{n}", String(i + 1))}</span>
            <code title={t.command}>{t.command}</code>
            <button className="btn sm ghost" onClick={() => stop(t.id)}>
              {LT.stopTest}
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** 끝난 작업의 판단 기록과 그룹 대화 */
export function SavedLeadLog({ decisions, groupLog }: { decisions?: LeadDecision[]; groupLog?: GroupNote[] }): ReactNode {
  if (!decisions?.length && !groupLog?.length) return null;
  return (
    <details className="draft">
      <summary>{LT.savedLog}</summary>
      {decisions && decisions.length > 0 && <DecisionRows items={decisions} />}
      {groupLog && groupLog.length > 0 && (
        <ul className="lead-said">
          {groupLog.map((g, i) => (
            <li key={i}>
              <b>{g.who}</b> {hideBlocks(g.text)}
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}

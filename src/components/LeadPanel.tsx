// 병렬 작업의 총괄 화면: 총괄 카드, 워커·그룹 카드, 창 테스트 대기열, 명령 판단 기록
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { api, errorText, type GroupNote, type LeadCommit, type LeadDecision, type LeadNote, type OrgPost, type OrgUnit, type TestDone, type TestItem } from "../lib/api";
import { hideBlocks, tailText } from "../lib/panel";
import { LAYOUT_DEFAULT, move, usePanelLayout, type SectionId } from "../lib/layout";
import { useApp, type AgentRun, type Live } from "../store";
import { ArrowDownIcon, ArrowUpIcon, ChevronIcon, CloseIcon, GearIcon, SendIcon, StopIcon } from "./Icons";
import TokenMeter from "./TokenMeter";
import Markdown from "./Markdown";
import LiveMarkdown from "./LiveMarkdown";
import { total } from "../lib/usage";
import { doneRatio, phaseOf, workerCounts, type Phase } from "../lib/progress";


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
  waitCommand: "결과 대기 중",
  working: "작업 중",
  finished: "완료",
  failed: "중단됨",
  queue: "창 테스트 대기열",
  slots: "동시 창 테스트 수",
  slotsHelp: "창이 열리는 실행은 이 대기열을 거치며, 간섭 방지를 위해 창 테스트는 1개를 권장합니다. 초과 실행은 순서대로 대기합니다.",
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
  statTools: "도구 사용",
  waitCommit: "병합 및 검토 중",
  leadIdle: "작업 결과 및 요청 대기 중",
  leadJudging: "검토 중",
  leadWorking: "직접 편집 중",
  timeline: "활동 로그",
  timelineEmpty: "아직 활동 내역이 없습니다",
  showAll: "전체 보기 ({n}개)",
  showLess: "접기",
  direct: "직접 실행",
  directHint: "총괄 AI가 메인 폴더를 직접 수정하고 커밋합니다",
  directPlaceholder: "직접 수정할 작업을 입력하세요",
  commits: "커밋 내역",
  allowed: "허용 {n}",
  blocked: "차단 {n}",
  latest: "최근 검토",
  expand: "검토 기록 전체 보기",
  log: "로그 보기",
  logHide: "로그 닫기",
  logEmpty: "출력된 로그가 없습니다",
  logLines: "{n}줄",
  logFollow: "최신으로",
  recentTests: "최근 테스트",
  testPassed: "성공",
  testFailed: "실패",
  leadLog: "총괄 활동 기록",
  bgRunning: "명령 {n}개 실행 중",
  progress: "전체 진행 현황",
  phaseSplit: "작업 분할",
  phaseWork: "작업 수행",
  phaseIntegrate: "결과 통합",
  phaseDone: "작업 완료",
  cntDone: "완료 {n}",
  cntRun: "진행 {n}",
  cntWait: "대기 {n}",
  cntFail: "실패 {n}",
  pctDone: "{p}% 완료",
  statCommits: "커밋 {n}개",
  statConflicts: "충돌 {n}건",
  statTests: "창 테스트 {r}실행·{w}대기",
  toolCount: "도구 {n}회",
  fileCount: "파일/폴더 {n}개",
  cardMore: "펼치기",
  cardLess: "접기",
  layout: "패널 설정",
  secWorkers: "작업자 현황",
  moveUp: "위로 이동",
  moveDown: "아래로 이동",
  density: "카드 표시 방식",
  densityCompact: "요약",
  densityFull: "상세",
  briefToggle: "채팅 답변에 진행 요약 표시",
  layoutReset: "초기화",
  moreWorkers: "+{n}개",
  leader: "그룹장",
  memberN: "그룹원 {n}",
  tabOutput: "작성 내용",
  tabActivity: "사용 도구",
  tabFiles: "담당 파일",
  tabTalk: "그룹 대화",
  tabOrg: "팀장 보고",
  tabCommands: "명령 검토",
  emptyActivity: "아직 사용한 도구가 없습니다.",
  emptyFiles: "담당 파일이 없습니다.",
  emptyOrg: "아직 주고받은 보고가 없습니다.",
  orgSent: "발신",
  orgGot: "수신",
  orgDirect: "총괄직속",
  orgChart: "조직도",
  orgBoard: "팀장 회의",
  teamCount: "팀 {n}개",
  doneLine: "작업이 완료되었습니다",
  winTabs: "보기 선택",
  membersLabel: "그룹원 목록",
  round: "{n}라운드",
};

/** 총괄 활동 종류 이름 */
const KIND: Record<LeadNote["kind"], string> = {
  review: "검토",
  consult: "작업 지시",
  merge: "변경 적용",
  conflict: "충돌",
  commit: "커밋",
  hold: "보류",
  work: "직접 수정",
  test: "테스트",
  warn: "경고",
  info: "정보",
};


export const VERDICT: Record<LeadDecision["verdict"], string> = { once: LT.once, deny: LT.deny, similar: LT.similar, all: LT.all, ask_user: LT.once };
const WAIT: Record<string, string> = { review: LT.waitReview, question: LT.waitQuestion, test: LT.waitTest, command: LT.waitCommand, idle: LT.waitMail, commit: LT.waitCommit };

/** 지금 상태 + 백그라운드 명령 수("작업 중 · 명령 2개 실행 중") */
export function runState(a: AgentRun): string {
  if (a.state !== "run") return a.state === "ok" ? LT.finished : LT.failed;
  const now = (a.wait && WAIT[a.wait]) || LT.working;
  return a.bg ? `${now} · ${LT.bgRunning.replace("{n}", String(a.bg))}` : now;
}

/** 화면에 보일 상태: 작업 중(블루) · 대기(회색) · 할 일 끝남(민트 옅게) · 완료 · 실패 */
export type Shown = "working" | "waiting" | "idle" | "ok" | "fail";
export function shownState(a: AgentRun): Shown {
  if (a.state !== "run") return a.state === "ok" && !a.stopped ? "ok" : "fail";
  return a.wait === "idle" ? "idle" : a.wait ? "waiting" : "working";
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
function LineBox({ placeholder, label, onSend, autoFocus, extra }: { placeholder: string; label: string; onSend: (t: string) => Promise<unknown>; autoFocus?: boolean; extra?: ReactNode }) {
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
      {extra}
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

/** "2026-11-04T07:12:03Z" → "16:12" */
function hhmm(at: string): string {
  const d = new Date(at);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** 총괄이 지금 하는 일: 판단 중(명령·지시), 메인 폴더 직접 작업, 아니면 마지막 활동 */
function LeadLive({ l, onLog }: { l: Live; onLog: (key: string) => void }) {
  const judge = l.agents["lead"];
  const work = l.agents["lead-work"];
  const last = l.lead!.timeline[l.lead!.timeline.length - 1];
  const busy = [work?.state === "run" ? work : null, judge?.state === "run" ? judge : null].filter(Boolean) as AgentRun[];
  if (!busy.length) {
    return (
      <div className="lead-now idle">
        <i className="lead-now-star" aria-hidden />
        <span className="lead-now-text">{last ? last.text : LT.leadIdle}</span>
      </div>
    );
  }
  return (
    <>
      {busy.map((a) => {
        const tail = a.key === "lead-work" ? tailLines(l.streams[a.key], 4) : "";
        return (
          <div key={a.key} className="lead-now-wrap">
            <div className="lead-now-head">
              <span className={`tag lead-mode ${a.key === "lead-work" ? "work" : ""}`}>{a.key === "lead-work" ? LT.leadWorking : LT.leadJudging}</span>
              <div className="spacer" />
              <Elapsed run={a} />
              <TokenMeter t={total(l.usage[a.key])} live compact />
            </div>
            <NowLine run={a} big />
            {tail && <LiveTail text={tail} title={LT.log} onOpen={() => onLog(a.key)} />}
          </div>
        );
      })}
    </>
  );
}

const TIMELINE_SHOWN = 6;

function LeadTimeline({ items }: { items: LeadNote[] }) {
  const [all, setAll] = useState(false);
  if (!items.length) return <div className="agent-sub">{LT.timelineEmpty}</div>;
  const shown = (all ? items : items.slice(-TIMELINE_SHOWN)).slice().reverse();
  return (
    <div className="lead-timeline">
      <ol className={all ? "all" : ""}>
        {shown.map((n, i) => (
          <li key={`${n.at}-${i}`} className={`k-${n.kind}`}>
            <span className="lead-tl-time">{hhmm(n.at)}</span>
            <span className={`tag tl ${n.kind}`}>{KIND[n.kind] ?? n.kind}</span>
            <span className="lead-tl-text" title={n.text}>
              {n.text}
            </span>
          </li>
        ))}
      </ol>
      {items.length > TIMELINE_SHOWN && (
        <button className="link" onClick={() => setAll((x) => !x)} aria-expanded={all}>
          {all ? LT.showLess : LT.showAll.replace("{n}", String(items.length))}
        </button>
      )}
    </div>
  );
}

function LeadCommits({ commits }: { commits: LeadCommit[] }) {
  if (!commits.length) return null;
  return (
    <div className="lead-commits">
      <h5>
        {LT.commits} <span className="agent-count">{commits.length}</span>
      </h5>
      <ul>
        {commits
          .slice(-5)
          .reverse()
          .map((c) => (
            <li key={c.sha} title={c.files.join("\n")}>
              <code className="sha">{c.sha}</code>
              <span>{c.message}</span>
            </li>
          ))}
      </ul>
    </div>
  );
}

/** 사용자와 총괄이 나눈 말. 총괄의 답은 마크다운으로 그린다. */
function SaidList({ said }: { said: { who: string; text: string }[] }) {
  return (
    <ul className="lead-said">
      {said.map((s, i) => (
        <li key={i} className={s.who}>
          <b>{s.who === "user" ? LT.you : LT.lead}</b>
          {s.who === "user" ? <span className="said-text">{s.text}</span> : <Markdown text={hideBlocks(s.text)} />}
        </li>
      ))}
    </ul>
  );
}

/** 실시간 출력 미리보기. 누르면 전체 로그 창이 열린다. */
function LiveTail({ text, title, onOpen }: { text: string; title: string; onOpen: () => void }) {
  return (
    <button type="button" className="live-tail" onClick={onOpen} title={title} aria-label={title}>
      {text}
    </button>
  );
}

function LeadCard({ l, onLog }: { l: Live; onLog: (key: string) => void }) {
  const lead = l.lead!;
  const [direct, setDirect] = useState(false);
  const busy = l.agents["lead"]?.state === "run" || l.agents["lead-work"]?.state === "run";
  return (
    <div className={`lead-card ${busy ? "busy" : ""}`}>
      <div className="lead-line lead-head">
        <i className="lead-star" aria-hidden />
        <b>{LT.lead}</b>
        <span className="agent-role">{[lead.label, lead.model].filter(Boolean).join(" · ")}</span>
      </div>
      <LeadLive l={l} onLog={onLog} />
      {lead.questions.map((q) => (
        <QuestionCard key={q.id} id={q.id} question={q.question} options={q.options} />
      ))}
      {lead.said.length > 0 && <SaidList said={lead.said.slice(-6)} />}
      <LineBox
        placeholder={direct ? LT.directPlaceholder : LT.leadPlaceholder}
        label={LT.send}
        onSend={(t) => api.leadSend(l.taskId, t, null, direct)}
        extra={
          <button type="button" className={`lead-direct ${direct ? "on" : ""}`} aria-pressed={direct} title={LT.directHint} onClick={() => setDirect((d) => !d)}>
            {LT.direct}
          </button>
        }
      />
      <details className="lead-sub" open>
        <summary>{LT.timeline}</summary>
        <LeadTimeline items={lead.timeline} />
      </details>
      <LeadCommits commits={lead.commits} />
    </div>
  );
}

function CardActions({ l, about, stopKeys, windowKey, title, onLog }: { l: Live; about: string; stopKeys: string[]; windowKey: string; title: string; onLog?: () => void }) {
  const { cancelAgent, toast } = useApp();
  const [reporting, setReporting] = useState(false);
  return (
    <>
      <div className="lead-actions">
        <button className="btn sm ghost" onClick={() => setReporting((r) => !r)} aria-expanded={reporting}>
          {LT.report}
        </button>
        {onLog && (
          <button className="btn sm ghost" onClick={onLog}>
            {LT.log}
          </button>
        )}
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

/** 카드마다 펼침을 따로 정할 수 있고, 정하지 않았으면 패널 구성의 기본 밀도를 따른다. */
function useCardOpen(): [boolean, () => void] {
  const [layout] = usePanelLayout();
  const [own, setOwn] = useState<boolean | null>(null);
  const open = own ?? layout.density === "full";
  return [open, () => setOwn(!open)];
}

function FoldButton({ open, onClick }: { open: boolean; onClick: () => void }) {
  const label = open ? LT.cardLess : LT.cardMore;
  return (
    <button type="button" className="icon card-fold" onClick={onClick} aria-expanded={open} aria-label={label} title={label}>
      <ChevronIcon open={open} />
    </button>
  );
}

function WorkerCard({ l, a, onLog }: { l: Live; a: AgentRun; onLog: (key: string) => void }) {
  const [open, toggle] = useCardOpen();
  const title = l.parts[a.key]?.title ?? a.label;
  const last = a.activities[a.activities.length - 1];
  const st = shownState(a);
  const tail = open && st === "working" ? tailLines(l.streams[a.key], 3) : "";
  return (
    <div className={`lead-worker ${a.state} st-${st} ${open ? "open" : "compact"}`}>
      <div className="lead-line">
        <i className="agent-dot" aria-hidden />
        <b title={title}>{title}</b>
        <div className="spacer" />
        <Elapsed run={a} />
        <FoldButton open={open} onClick={toggle} />
      </div>
      <div className="lead-line sub">
        <span className="agent-role">{[a.label, a.effort].filter(Boolean).join(" · ")}</span>
        <span className={`agent-state st-${st}`}>{a.stopped ? LT.failed : runState(a)}</span>
        <TokenMeter t={total(l.usage[a.key])} live={st === "working"} compact />
      </div>
      {a.state === "fail" && a.error && !a.stopped ? (
        <div className="agent-err" title={a.error}>
          {a.error}
        </div>
      ) : a.state === "run" ? (
        <NowLine run={a} />
      ) : (
        <div className="agent-act" title={last}>
          {last ?? " "}
        </div>
      )}
      {open && (
        <>
          <div className="lead-line sub worker-stats">
            <span>{LT.toolCount.replace("{n}", a.activities.length >= 200 ? "200+" : String(a.activities.length))}</span>
            {(l.parts[a.key]?.files.length ?? 0) > 0 && <span title={l.parts[a.key]!.files.join("\n")}>{LT.fileCount.replace("{n}", String(l.parts[a.key]!.files.length))}</span>}
          </div>
          {tail && <LiveTail text={tail} title={LT.log} onOpen={() => onLog(a.key)} />}
          {a.state !== "fail" && <CardActions l={l} about={a.key} stopKeys={[a.key]} windowKey={a.key} title={title} onLog={() => onLog(a.key)} />}
        </>
      )}
    </div>
  );
}

function GroupCard({ l, index, title, keys }: { l: Live; index: number; title: string; keys: string[] }) {
  const [open, toggle] = useCardOpen();
  const talk = l.lead!.groupLog.filter((g) => g.group === index).slice(open ? -5 : -1);
  const members = keys.map((k) => l.agents[k]).filter(Boolean);
  return (
    <div className={`lead-worker group ${open ? "open" : "compact"}`}>
      <div className="lead-line">
        <span className="tag">{LT.group}</span>
        <b title={title}>{title}</b>
        <div className="spacer" />
        <FoldButton open={open} onClick={toggle} />
      </div>
      <div className="lead-members">
        {members.map((a, i) => (
          <span key={a.key} className={`lead-member ${a.state} st-${shownState(a)}`}>
            <i className="agent-dot" aria-hidden /> {LT.memberN.replace("{n}", String(i + 1))}
            {i === 0 && <span className="tag">{LT.leader}</span>} · {a.stopped ? LT.failed : runState(a)}
          </span>
        ))}
      </div>
      {talk.length === 0 ? (
        <div className="agent-sub">{LT.noTalk}</div>
      ) : (
        <ul className={`lead-said ${open ? "" : "one"}`}>
          {talk.map((g, i) => (
            <li key={i}>
              <b>{g.who}</b> {open ? <Markdown text={hideBlocks(g.text)} /> : <span className="said-text">{hideBlocks(g.text)}</span>}
            </li>
          ))}
        </ul>
      )}
      {open && <CardActions l={l} about={keys[0]} stopKeys={keys} windowKey={`group:${index}:${keys.join(",")}`} title={title} />}
    </div>
  );
}

/** 두 부서 경로의 같은 앞부분: 보고가 오간 회의실 */
function common(a: string[], b: string[]): string[] {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  return a.slice(0, i);
}

const same = (a: string[], b: string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** 이 회의실(부서 경로)에서 오간 팀장 보고 */
function roomPosts(units: OrgUnit[], log: OrgPost[], room: string[]): OrgPost[] {
  const at = (i: number) => units.find((u) => u.index === i)?.org ?? [];
  return log.filter((p) => p.to.some((t) => same(common(at(p.from), at(t)), room)));
}

const ROOM_SHOWN = 3;

/** 부서 안 팀장 회의: 최근 몇 개만, 누르면 전부 */
function Room({ posts }: { posts: OrgPost[] }) {
  const [all, setAll] = useState(false);
  if (!posts.length) return null;
  const shown = all ? posts : posts.slice(-ROOM_SHOWN);
  return (
    <div className="org-room">
      <h5>
        {LT.orgBoard} <span className="agent-count">{posts.length}</span>
      </h5>
      <ul className="lead-said">
        {shown.map((p, i) => (
          <li key={`${p.at}-${i}`}>
            <b>{p.title}</b> {all ? <Markdown text={p.text} /> : <span className="said-text">{p.text}</span>}
          </li>
        ))}
      </ul>
      {posts.length > ROOM_SHOWN && (
        <button className="link" onClick={() => setAll((x) => !x)} aria-expanded={all}>
          {all ? LT.showLess : LT.showAll.replace("{n}", String(posts.length))}
        </button>
      )}
    </div>
  );
}

/** 팀 하나: 둘 이상이면 그룹 카드, 하나면 워커 카드 */
function UnitCard({ l, u, onLog }: { l: Live; u: OrgUnit; onLog: (key: string) => void }) {
  if (u.keys.length > 1) return <GroupCard l={l} index={u.index} title={u.title} keys={u.keys} />;
  const a = l.agents[u.keys[0]];
  return a ? <WorkerCard l={l} a={a} onLog={onLog} /> : null;
}

/** 부서 하나와 그 아래(하위 부서, 팀). 접으면 팀 상태만 점으로 보인다. */
function Dept({ l, path, units, onLog }: { l: Live; path: string[]; units: OrgUnit[]; onLog: (key: string) => void }) {
  const [open, setOpen] = useState(true);
  const lead = l.lead!;
  const inside = units.filter((u) => path.every((d, i) => u.org[i] === d) && u.org.length >= path.length);
  const direct = inside.filter((u) => u.org.length === path.length);
  const subs = [...new Set(inside.filter((u) => u.org.length > path.length).map((u) => u.org[path.length]))];
  const runs = inside.flatMap((u) => u.keys.map((k) => l.agents[k]).filter(Boolean));
  const posts = roomPosts(lead.org, lead.orgLog, path);
  const body = (
    <>
      <Room posts={posts} />
      {subs.map((d) => (
        <Dept key={d} l={l} path={[...path, d]} units={units} onLog={onLog} />
      ))}
      {direct.map((u) => (
        <UnitCard key={u.index} l={l} u={u} onLog={onLog} />
      ))}
    </>
  );
  if (!path.length) return <div className="org-root">{body}</div>;
  return (
    <section className={`org-dept ${open ? "open" : ""}`} aria-label={path.join(" › ")}>
      <div className="lead-line">
        <span className="tag">{path.length > 1 ? path.slice(0, -1).join(" › ") : LT.orgChart}</span>
        <b title={path.join(" › ")}>{path[path.length - 1]}</b>
        <span className="agent-sub">{LT.teamCount.replace("{n}", String(inside.length))}</span>
        <div className="spacer" />
        <span className="org-dots" aria-hidden>
          {runs.map((a) => (
            <i key={a.key} className={`agent-dot st-${shownState(a)}`} />
          ))}
        </span>
        <FoldButton open={open} onClick={() => setOpen((o) => !o)} />
      </div>
      {open && <div className="org-dept-body">{body}</div>}
    </section>
  );
}

/** 조직도: 부서 → 하위 부서 → 팀(그룹·워커). 부서마다 그 안 팀장끼리 주고받은 보고가 보인다. */
function OrgTree({ l, onLog }: { l: Live; onLog: (key: string) => void }) {
  return <Dept l={l} path={[]} units={l.lead!.org} onLog={onLog} />;
}

/** 최근 판단 몇 개만 보이고, 펼치면 전체를 본다. 자동 허용 해제 버튼은 접어도 보인다. */
const DECISIONS_SHOWN = 3;

function DecisionLog({ l }: { l: Live }) {
  const { toast } = useApp();
  const [open, setOpen] = useState(false);
  const lead = l.lead!;
  const auto = lead.policy.allowAll || lead.policy.rules.length > 0;
  const denied = lead.decisions.filter((d) => d.verdict === "deny").length;
  const allowed = lead.decisions.length - denied;
  const recent = lead.decisions.slice(-(open ? 30 : DECISIONS_SHOWN)).reverse();
  return (
    <div className={`lead-log ${open ? "open" : ""}`}>
      <div className="lead-line">
        <button className="lead-fold" onClick={() => setOpen((o) => !o)} aria-expanded={open} aria-label={LT.expand} disabled={lead.decisions.length <= DECISIONS_SHOWN && !open}>
          <i className="fold-chev" aria-hidden />
          <b>{LT.decisions}</b>
        </button>
        {lead.decisions.length > 0 && (
          <>
            <span className="tag verdict once">{LT.allowed.replace("{n}", String(allowed))}</span>
            {denied > 0 && <span className="tag verdict deny">{LT.blocked.replace("{n}", String(denied))}</span>}
          </>
        )}
        {auto && (
          <button className="btn sm danger lead-revoke" onClick={() => api.leadRevoke(l.taskId).catch((e) => toast(errorText(e), "error"))}>
            {LT.revoke}
          </button>
        )}
      </div>
      {auto && <p className="lead-warn">{LT.autoOn}</p>}
      {lead.decisions.length === 0 ? <div className="agent-sub">{LT.noDecisions}</div> : <DecisionRows items={recent} compact={!open} />}
      {lead.decisions.length > DECISIONS_SHOWN && (
        <button className="link" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          {open ? LT.showLess : LT.showAll.replace("{n}", String(lead.decisions.length))}
        </button>
      )}
    </div>
  );
}

function DecisionRows({ items, compact = false }: { items: LeadDecision[]; compact?: boolean }) {
  return (
    <ul className={`lead-decisions ${compact ? "compact" : ""}`}>
      {items.map((d) => (
        <li key={d.id}>
          <span className={`tag verdict ${d.verdict}`}>{VERDICT[d.verdict]}</span>
          <code title={d.command}>{d.pattern ?? d.command}</code>
          {!compact && byLabel(d.by) && <span className="agent-sub">{byLabel(d.by)}</span>}
          {!compact && d.reason && <div className="agent-sub lead-reason">{d.reason}</div>}
        </li>
      ))}
    </ul>
  );
}

const PHASES: Phase[] = ["split", "work", "integrate"];

/** 병렬 작업 전체의 진행: 단계, 워커 상태 막대, 커밋·충돌·창 테스트 */
function ProgressOverview({ l }: { l: Live }) {
  const { queue } = useApp();
  const runs = Object.values(l.agents);
  const c = workerCounts(runs);
  const phase = phaseOf(runs);
  const at = phase === "done" ? PHASES.length : PHASES.indexOf(phase);
  const name: Record<Phase, string> = { split: LT.phaseSplit, work: LT.phaseWork, integrate: LT.phaseIntegrate, done: LT.phaseDone };
  const w = (n: number) => `${c.total ? (n / c.total) * 100 : 0}%`;
  const conflicts = l.lead?.timeline.filter((n) => n.kind === "conflict").length ?? 0;
  const testsRun = queue.running.filter((t) => t.task === l.taskId).length;
  const testsWait = queue.waiting.filter((t) => t.task === l.taskId).length;
  return (
    <div className="progress-box">
      <div className="progress-head">
        <b>{LT.progress}</b>
        <ol className="phase-steps">
          {PHASES.map((p, i) => (
            <li key={p} className={i < at ? "done" : i === at ? "now" : ""}>
              {name[p]}
            </li>
          ))}
        </ol>
        <span className="progress-pct">{LT.pctDone.replace("{p}", String(Math.round(doneRatio(c) * 100)))}</span>
      </div>
      {c.total > 0 && (
        <>
          <div className="progress-bar" role="img" aria-label={`${LT.cntDone.replace("{n}", String(c.ok))}, ${LT.cntRun.replace("{n}", String(c.run))}, ${LT.cntWait.replace("{n}", String(c.wait))}, ${LT.cntFail.replace("{n}", String(c.fail))}`}>
            <i className="ok" style={{ width: w(c.ok) }} />
            <i className="run" style={{ width: w(c.run) }} />
            <i className="wait" style={{ width: w(c.wait) }} />
            <i className="fail" style={{ width: w(c.fail) }} />
          </div>
          <div className="progress-legend">
            <span className="ok">{LT.cntDone.replace("{n}", String(c.ok))}</span>
            <span className="run">{LT.cntRun.replace("{n}", String(c.run))}</span>
            <span className="wait">{LT.cntWait.replace("{n}", String(c.wait))}</span>
            {c.fail > 0 && <span className="fail">{LT.cntFail.replace("{n}", String(c.fail))}</span>}
          </div>
        </>
      )}
      {l.lead && (
        <div className="progress-stats">
          <span className="tag tl commit">{LT.statCommits.replace("{n}", String(l.lead.commits.length))}</span>
          {conflicts > 0 && <span className="tag tl conflict">{LT.statConflicts.replace("{n}", String(conflicts))}</span>}
          {testsRun + testsWait > 0 && <span className="tag live">{LT.statTests.replace("{r}", String(testsRun)).replace("{w}", String(testsWait))}</span>}
        </div>
      )}
    </div>
  );
}

/** 총괄이 있는 작업의 내용. 섹션 순서와 표시는 패널 구성을 따른다. */
export function LeadSection({ l }: { l: Live }) {
  const lead = l.lead!;
  const [layout] = usePanelLayout();
  const [editing, setEditing] = useState(false);
  const [logKey, setLogKey] = useState<string | null>(null);
  const inOrg = new Set(lead.org.flatMap((u) => u.keys));
  const inGroup = new Set(lead.groups.flatMap((g) => g.keys));
  const solo = Object.values(l.agents)
    .filter((a) => a.key.startsWith("agent:") && !inGroup.has(a.key) && !inOrg.has(a.key))
    .sort((a, b) => a.key.localeCompare(b.key));
  const section: Record<SectionId, ReactNode> = {
    progress: <ProgressOverview key="progress" l={l} />,
    lead: <LeadCard key="lead" l={l} onLog={setLogKey} />,
    workers: (
      <div key="workers" className="lead-workers">
        {lead.org.length > 0 && <OrgTree l={l} onLog={setLogKey} />}
        {lead.groups
          .filter((g) => !g.keys.some((k) => inOrg.has(k)))
          .map((g) => (
            <GroupCard key={g.index} l={l} index={g.index} title={g.title} keys={g.keys} />
          ))}
        {solo.map((a) => (
          <WorkerCard key={a.key} l={l} a={a} onLog={setLogKey} />
        ))}
      </div>
    ),
    decisions: <DecisionLog key="decisions" l={l} />,
  };
  return (
    <>
      <div className="layout-bar">
        <button type="button" className={`btn sm ghost ${editing ? "on" : ""}`} onClick={() => setEditing((e) => !e)} aria-expanded={editing}>
          <GearIcon /> {LT.layout}
        </button>
      </div>
      {editing && <LayoutEditor />}
      {layout.order.filter((id) => !layout.hidden.includes(id)).map((id) => section[id])}
      {logKey && <LogDrawer l={l} k={logKey} onClose={() => setLogKey(null)} />}
    </>
  );
}

const SECTION_NAME: Record<SectionId, string> = { progress: LT.progress, lead: LT.lead, workers: LT.secWorkers, decisions: LT.decisions };

/** 패널 구성: 섹션 보이기·순서, 워커 카드 기본 밀도, 메인 대화 요약 */
export function LayoutEditor() {
  const [layout, setLayout] = usePanelLayout();
  const toggle = (id: SectionId) => setLayout({ ...layout, hidden: layout.hidden.includes(id) ? layout.hidden.filter((x) => x !== id) : [...layout.hidden, id] });
  return (
    <div className="layout-edit">
      <ol className="layout-list">
        {layout.order.map((id, i) => (
          <li key={id} className={layout.hidden.includes(id) ? "off" : ""}>
            <label>
              <input type="checkbox" checked={!layout.hidden.includes(id)} onChange={() => toggle(id)} />
              <span>{SECTION_NAME[id]}</span>
            </label>
            <div className="spacer" />
            <button type="button" className="icon" disabled={i === 0} onClick={() => setLayout({ ...layout, order: move(layout.order, id, -1) })} aria-label={`${SECTION_NAME[id]} · ${LT.moveUp}`} title={LT.moveUp}>
              <ArrowUpIcon />
            </button>
            <button type="button" className="icon" disabled={i === layout.order.length - 1} onClick={() => setLayout({ ...layout, order: move(layout.order, id, 1) })} aria-label={`${SECTION_NAME[id]} · ${LT.moveDown}`} title={LT.moveDown}>
              <ArrowDownIcon />
            </button>
          </li>
        ))}
      </ol>
      <div className="layout-row">
        <span>{LT.density}</span>
        <div className="seg" role="radiogroup" aria-label={LT.density}>
          {(["compact", "full"] as const).map((d) => (
            <button key={d} type="button" role="radio" aria-checked={layout.density === d} className={layout.density === d ? "on" : ""} onClick={() => setLayout({ ...layout, density: d })}>
              {d === "compact" ? LT.densityCompact : LT.densityFull}
            </button>
          ))}
        </div>
      </div>
      <label className="layout-row">
        <input type="checkbox" checked={layout.brief} onChange={() => setLayout({ ...layout, brief: !layout.brief })} />
        <span>{LT.briefToggle}</span>
      </label>
      <div className="layout-row">
        <div className="spacer" />
        <button type="button" className="btn sm ghost" onClick={() => setLayout(LAYOUT_DEFAULT)}>
          {LT.layoutReset}
        </button>
      </div>
    </div>
  );
}

/** 실시간 출력 전체를 옆에서 열어 본다. 맨 아래면 새 글을 따라가고, 위로 올리면 멈춘다. */
function LogDrawer({ l, k, onClose }: { l: Live; k: string; onClose: () => void }) {
  const a = l.agents[k];
  const title = l.parts[k]?.title ?? (k.startsWith("lead") ? LT.lead : a?.label ?? k);
  const text = tailText(l.streams[k] ?? "", 60000);
  const live = a?.state === "run" && !a.wait;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  // 패널의 유리 효과(backdrop-filter) 안에서는 fixed가 패널 기준이 되므로 body에 그린다.
  return createPortal(
    <div className="drawer-back" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <aside className="log-drawer" role="dialog" aria-modal="true" aria-label={`${LT.winLive} · ${title}`}>
        <header className="log-drawer-head">
          <div className="log-drawer-title">
            <span className="agent-sub">{LT.winLive}</span>
            <b title={title}>{title}</b>
          </div>
          {a && <Elapsed run={a} />}
          <button type="button" className="icon" onClick={onClose} aria-label={LT.logHide} title={LT.logHide} autoFocus>
            <CloseIcon />
          </button>
        </header>
        <div className="log-drawer-body">
          <LiveMarkdown text={text} live={live} empty={LT.logEmpty} jumpLabel={LT.logFollow} />
        </div>
      </aside>
    </div>,
    document.body,
  );
}

const BRIEF_WORKERS = 6;

/** 메인 대화 카드의 진행 요약: 진행률, 총괄이 하는 일·질문, 워커 상태, 최근 활동 */
export function LiveBrief({ l }: { l: Live }) {
  const [layout] = usePanelLayout();
  const workers = Object.values(l.agents)
    .filter((a) => a.key.startsWith("agent:"))
    .sort((a, b) => a.key.localeCompare(b.key));
  if (!layout.brief || (!l.lead && workers.length === 0)) return null;
  const c = workerCounts(Object.values(l.agents));
  const pct = Math.round(doneRatio(c) * 100);
  const w = (n: number) => `${c.total ? (n / c.total) * 100 : 0}%`;
  const lead = l.lead;
  const leadBusy = ["lead-work", "lead"].map((k) => l.agents[k]).find((a) => a?.state === "run");
  const leadAct = leadBusy ? splitActivity(leadBusy.activities[leadBusy.activities.length - 1] ?? "").detail : "";
  const leadText = leadBusy ? leadAct || (leadBusy.key === "lead-work" ? LT.leadWorking : LT.leadJudging) : (lead?.timeline[lead.timeline.length - 1]?.text ?? LT.leadIdle);
  const reply = lead?.said.filter((s) => s.who !== "user").slice(-1)[0];
  return (
    <section className="live-brief" aria-label={LT.progress}>
      {c.total > 0 && (
        <div className="brief-progress">
          <div className="progress-bar" role="img" aria-label={`${LT.cntDone.replace("{n}", String(c.ok))}, ${LT.cntRun.replace("{n}", String(c.run))}, ${LT.cntWait.replace("{n}", String(c.wait))}, ${LT.cntFail.replace("{n}", String(c.fail))}`}>
            <i className="ok" style={{ width: w(c.ok) }} />
            <i className="run" style={{ width: w(c.run) }} />
            <i className="wait" style={{ width: w(c.wait) }} />
            <i className="fail" style={{ width: w(c.fail) }} />
          </div>
          <span className="progress-pct">{LT.pctDone.replace("{p}", String(pct))}</span>
        </div>
      )}
      {lead && (
        <div className={`brief-lead ${leadBusy ? "busy" : ""}`}>
          <i className="lead-now-star" aria-hidden />
          <b>{LT.lead}</b>
          <span className="brief-lead-text" title={leadText}>
            {leadText}
          </span>
          {leadBusy && <Elapsed run={leadBusy} />}
        </div>
      )}
      {lead?.questions.map((q) => (
        <QuestionCard key={q.id} id={q.id} question={q.question} options={q.options} />
      ))}
      {reply && (
        <div className="brief-reply">
          <Markdown text={hideBlocks(reply.text)} />
        </div>
      )}
      {workers.length > 0 && (
        <ul className="brief-workers">
          {workers.slice(0, BRIEF_WORKERS).map((a) => {
            const st = shownState(a);
            const now = a.state === "run" && !a.wait ? splitActivity(a.activities[a.activities.length - 1] ?? "").detail : "";
            const name = l.parts[a.key]?.title ?? a.label;
            return (
              <li key={a.key} className={`st-${st}`}>
                <i className="agent-dot" aria-hidden />
                <b title={name}>{name}</b>
                <span className={`agent-state st-${st}`}>{a.stopped ? LT.failed : runState(a)}</span>
                {now && (
                  <span className="brief-now" title={now}>
                    {now}
                  </span>
                )}
              </li>
            );
          })}
          {workers.length > BRIEF_WORKERS && <li className="brief-more">{LT.moreWorkers.replace("{n}", String(workers.length - BRIEF_WORKERS))}</li>}
        </ul>
      )}
      {lead && lead.timeline.length > 0 && (
        <ol className="brief-timeline">
          {lead.timeline
            .slice(-3)
            .reverse()
            .map((n, i) => (
              <li key={`${n.at}-${i}`} className={`k-${n.kind}`}>
                <span className="lead-tl-time">{hhmm(n.at)}</span>
                <span className={`tag tl ${n.kind}`}>{KIND[n.kind] ?? n.kind}</span>
                <span className="lead-tl-text" title={n.text}>
                  {n.text}
                </span>
              </li>
            ))}
        </ol>
      )}
    </section>
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

/** 창 테스트 로그: 맨 아래에 있으면 새 줄을 따라가고, 위로 올리면 멈춘다. */
function TestLog({ id }: { id: number }) {
  const { testLogs, loadTestLog } = useApp();
  const lines = testLogs[id] ?? [];
  const box = useRef<HTMLPreElement>(null);
  const [follow, setFollow] = useState(true);
  useEffect(() => loadTestLog(id), [id, loadTestLog]);
  useLayoutEffect(() => {
    if (follow && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [lines.length, follow]);
  const onScroll = () => {
    const b = box.current;
    if (b) setFollow(b.scrollHeight - b.scrollTop - b.clientHeight < 24);
  };
  return (
    <div className="test-log">
      <div className="test-log-head">
        <span>{LT.logLines.replace("{n}", String(lines.length))}</span>
        <div className="spacer" />
        {!follow && (
          <button className="link" onClick={() => setFollow(true)}>
            {LT.logFollow}
          </button>
        )}
      </div>
      <pre ref={box} onScroll={onScroll} tabIndex={0}>
        {lines.length ? lines.join("\n") : LT.logEmpty}
      </pre>
    </div>
  );
}

function TestRow({ t, tag, tagClass, action }: { t: TestItem | TestDone; tag: string; tagClass?: string; action?: ReactNode }) {
  const [open, setOpen] = useState(false);
  const summary = "summary" in t ? t.summary : null;
  return (
    <li className={open ? "open" : ""}>
      <div className="lead-queue-row">
        <span className={`tag ${tagClass ?? ""}`}>{tag}</span>
        <code title={t.command}>{t.command}</code>
        <button className="btn sm ghost" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          {open ? LT.logHide : LT.log}
        </button>
        {action}
      </div>
      {summary && <div className="agent-sub test-summary">{summary}</div>}
      {open && <TestLog id={t.id} />}
    </li>
  );
}

export function QueueCard() {
  const { queue, toast } = useApp();
  const stop = (id: number) => api.testCancel(id).catch((e) => toast(errorText(e), "error"));
  const recent = queue.recent ?? [];
  return (
    <section className="tool-sec">
      <h4 className="lead-queue-head">
        {LT.queue}
        <SlotsPicker />
      </h4>
      {queue.running.length + queue.waiting.length === 0 && <div className="agent-sub">{LT.emptyQueue}</div>}
      <ul className="lead-queue">
        {queue.running.map((t) => (
          <TestRow
            key={t.id}
            t={t}
            tag={LT.running}
            tagClass="live"
            action={
              <button className="btn sm danger" onClick={() => stop(t.id)}>
                {LT.stopTest}
              </button>
            }
          />
        ))}
        {queue.waiting.map((t, i) => (
          <TestRow
            key={t.id}
            t={t}
            tag={LT.position.replace("{n}", String(i + 1))}
            action={
              <button className="btn sm ghost" onClick={() => stop(t.id)}>
                {LT.stopTest}
              </button>
            }
          />
        ))}
      </ul>
      {recent.length > 0 && (
        <>
          <h5 className="lead-queue-sub">{LT.recentTests}</h5>
          <ul className="lead-queue">
            {recent.map((t) => (
              <TestRow key={t.id} t={t} tag={t.ok ? LT.testPassed : LT.testFailed} tagClass={t.ok ? "ok" : "bad"} />
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

/** 끝난 작업의 판단 기록과 그룹 대화 */
export function SavedLeadLog({ decisions, groupLog, leadLog, orgLog }: { decisions?: LeadDecision[]; groupLog?: GroupNote[]; leadLog?: LeadNote[]; orgLog?: OrgPost[] }): ReactNode {
  if (!decisions?.length && !groupLog?.length && !leadLog?.length && !orgLog?.length) return null;
  return (
    <details className="draft">
      <summary>{LT.savedLog}</summary>
      {leadLog && leadLog.length > 0 && (
        <>
          <h5 className="lead-queue-sub">{LT.leadLog}</h5>
          <LeadTimeline items={leadLog} />
        </>
      )}
      {decisions && decisions.length > 0 && <DecisionRows items={decisions} />}
      {groupLog && groupLog.length > 0 && (
        <ul className="lead-said">
          {groupLog.map((g, i) => (
            <li key={i}>
              <b>{g.who}</b> <Markdown text={hideBlocks(g.text)} />
            </li>
          ))}
        </ul>
      )}
      {orgLog && orgLog.length > 0 && (
        <>
          <h5 className="lead-queue-sub">{LT.orgBoard}</h5>
          <ul className="lead-said">
            {orgLog.map((p, i) => (
              <li key={i}>
                <b>{p.title}</b> <Markdown text={p.text} />
              </li>
            ))}
          </ul>
        </>
      )}
    </details>
  );
}

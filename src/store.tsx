import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, errorText, type BgJob, type GroupInfo, type GroupNote, type OrgPost, type OrgUnit, type LeadCommit, type LeadNote, type LeadDecision, type LeadPolicy, type LeadQuestion, type QueueView, type Approval, type Conversation, type EngineChoice, type EngineId, type EngineInfo, type Budget, type CouncilPlan, type DebatePlan, type Judgment, type Message, type Settings, type SkillRef, type SkillsView, type TeamPlan } from "./lib/api";
import { useTauriEvent } from "./lib/events";
import { emit } from "@tauri-apps/api/event";
import { ACTIVITY_KEEP, STREAM_KEEP, hideBlocks } from "./lib/panel";
import { liveTurn, turnEvent, type RunUsage } from "./lib/usage";
import { isSleeping } from "./lib/sleep";

/** 총괄 활동 기록을 화면에 몇 개까지 둘지 */
const TIMELINE_KEEP = 300;
/** 팀장 보고를 몇 개까지 둘지 */
const ORG_LOG_KEEP = 400;
/** 창 테스트 로그를 화면에 몇 줄까지 둘지(백엔드 보관량과 같다) */
const TEST_LOG_KEEP = 4000;

/** 글자 조각을 화면에 반영하는 간격(ms) */
const DELTA_FLUSH_MS = 50;
/** 취침 중에는 화면이 꺼져 있으니 드물게 반영한다 */
const SLEEP_FLUSH_MS = 2000;

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

/** AI 실행 하나(초안·검토·토론·동시 에이전트·종합). 오른쪽 "에이전트" 탭이 보여 준다. */
export interface AgentRun {
  key: string;
  engine: EngineId;
  label: string;
  role: string;
  model: string | null;
  effort: string | null;
  state: "run" | "ok" | "fail";
  startedAt: number;
  ms: number | null;
  error: string | null;
  chars: number | null;
  activities: string[];
  /** 사용자가 이 에이전트만 멈췄다 */
  stopped: boolean;
  /** 사용자가 이 에이전트에게 보낸 메시지 */
  sent: string[];
  /** 차례 사이에 무엇을 기다리는지(review·question·test·command) */
  wait?: string | null;
  /** 백그라운드에서 도는 명령 수 */
  bg?: number;
}

/** 곁질문 한 차례. 앱 메모리에만 두고 대화 기록에 남기지 않는다. */
export interface SideTurn {
  id: string;
  role: "user" | "assistant";
  text: string;
  state?: "run" | "ok" | "fail";
  engine?: EngineId | null;
  activity?: string | null;
  error?: string | null;
  stopped?: boolean;
}

/** 병렬 작업의 하위 작업(실행 key → 이름·맡은 파일) */
export interface PartInfo {
  title: string;
  files: string[];
}

/** 병렬 작업의 총괄 상태 */
export interface LeadState {
  engine: EngineId;
  label: string;
  model: string | null;
  said: { who: "lead" | "user"; text: string }[];
  policy: LeadPolicy;
  decisions: LeadDecision[];
  questions: LeadQuestion[];
  groups: GroupInfo[];
  groupLog: GroupNote[];
  /** 조직으로 일할 때 팀과 부서(없으면 비어 있다) */
  org: OrgUnit[];
  /** 팀장끼리 주고받은 보고 */
  orgLog: OrgPost[];
  /** 총괄이 지금까지 한 일(실시간) */
  timeline: LeadNote[];
  /** 총괄이 만든 커밋 */
  commits: LeadCommit[];
}

/** 진행 중인 요청 하나(스트리밍 중인 답) */
export interface Live {
  taskId: string;
  conversationId: string;
  messageId: string | null;
  primary: EngineId | null;
  reviewer: EngineId | null;
  work: boolean;
  skill: SkillRef | null;
  reasons: string[];
  jev: Judgment | null;
  budget: Budget | null;
  /** 지금 단계의 모델·노력치 */
  effort: string | null;
  model: string | null;
  phase: "draft" | "review" | "judge" | null;
  /** 토론일 때 참가 AI와 판정 AI, 각 AI의 진행 상태(엔진:역할 → run/ok/fail) */
  debate: DebatePlan | null;
  debateState: Record<string, "run" | "ok" | "fail">;
  /** 동시 에이전트일 때 참가 AI와 종합 AI */
  team: TeamPlan | null;
  /** 회의일 때 참가 AI·최대 라운드·정리 AI */
  council: CouncilPlan | null;
  /** 병렬 작업의 총괄 */
  lead: LeadState | null;
  /** 여러 AI가 동시에 쓰는 글(실행 key → 지금까지 받은 글). 회의 창이 보여 준다. */
  streams: Record<string, string>;
  /** 실행 key → 상태(시작한 순서대로) */
  agents: Record<string, AgentRun>;
  /** 병렬 작업일 때 실행 key → 하위 작업 */
  parts: Record<string, PartInfo>;
  /** 총괄이 지운 워커 key. 늦게 온 이벤트로 다시 생기지 않게 한다. */
  gone: Record<string, true>;
  /** 에이전트가 뒤에서 돌리는 명령(id → 상태) */
  bg: Record<string, BgJob>;
  /** 실행 key → 토큰 사용량(실행 중에도 늘어난다) */
  usage: Record<string, RunUsage>;
  startedAt: number;
  label: string | null;
  text: string;
  draft: string;
  activities: string[];
}

export type Dialog = null | "settings" | "keys" | "skills" | "usage";

interface AppCtx {
  /** 창 테스트 대기열(앱 전체) */
  queue: QueueView;
  /** 창 테스트 id → 로그 줄(실행 중이거나 최근에 끝난 테스트) */
  testLogs: Record<number, string[]>;
  /** 늦게 연 로그 창이 지금까지의 로그를 받아 온다 */
  loadTestLog: (id: number) => void;
  engines: EngineInfo[];
  setEngines: (e: EngineInfo[]) => void;
  /** 새 세션을 시작할 때 쓸 폴더(없으면 기본 작업 폴더) */
  newFolder: string | null;
  setNewFolder: (p: string | null) => void;
  /** 폴더 선택 창을 열어 세션(또는 새 세션)의 폴더를 바꾼다 */
  chooseFolder: (conversationId: string | null) => Promise<void>;
  resetFolder: (conversationId: string | null) => Promise<void>;
  refreshEngines: () => Promise<void>;
  settings: Settings | null;
  saveSettings: (s: Settings) => Promise<void>;
  conversations: Conversation[];
  conversationId: string | null;
  openConversation: (id: string | null) => void;
  deleteConversation: (id: string) => Promise<void>;
  messages: Message[];
  live: Live[];
  approvals: Approval[];
  decide: (id: string, approve: boolean) => Promise<void>;
  /** work: 작업 모드(허락을 받은 뒤 파일 수정) · build: '만들기' — 폴더 안에서 승인 없이 끝까지 완성 · council: '회의' — 여러 AI가 서로 대화하며 푼다 */
    send: (text: string, engine: EngineChoice, work: boolean, build: boolean, council?: boolean, attachments?: string[]) => Promise<boolean>;
  cancel: (taskId: string) => void;
  /** 에이전트 하나만 멈춘다 */
  cancelAgent: (taskId: string, key: string) => void;
  /** 실행 중인 모든 작업을 멈춘다 */
  cancelAll: () => void;
  /** 지금 세션의 곁질문 */
  side: SideTurn[];
  askSide: (question: string) => Promise<void>;
  stopSide: () => void;
  clearSide: () => void;
  skills: SkillsView | null;
  refreshSkills: () => Promise<void>;
  dialog: Dialog;
  setDialog: (d: Dialog) => void;
  toasts: Toast[];
  toast: (text: string, kind?: Toast["kind"]) => void;
}

const Ctx = createContext<AppCtx | null>(null);

export function useApp(): AppCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("AppProvider missing");
  return c;
}

const blank = (taskId: string, conversationId: string, work: boolean): Live => ({
  taskId,
  conversationId,
  lead: null,
  messageId: null,
  primary: null,
  reviewer: null,
  work,
  skill: null,
  reasons: [],
  jev: null,
  budget: null,
  debate: null,
  debateState: {},
  team: null,
  council: null,
  streams: {},
  usage: {},
  agents: {},
  parts: {},
  gone: {},
  bg: {},
  startedAt: Date.now(),
  effort: null,
  model: null,
  phase: null,
  label: null,
  text: "",
  draft: "",
  activities: [],
});

/** 곁질문에 보내는 진행 상황의 전체 길이(백엔드 SIDE_PROGRESS_CHARS와 맞춘다) */
const PROGRESS_CHARS = 15000;
const WAIT_NOTE: Record<string, string> = { review: "명령 검토 대기", question: "사용자 답 대기", test: "창 테스트 결과 대기", command: "할 일을 끝내고 백그라운드 명령 결과 대기", idle: "할 일을 끝내고 메시지 대기", commit: "변경 반영·커밋 검토 중" };

function secs(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  return s >= 60 ? `${Math.floor(s / 60)}분 ${s % 60}초` : `${s}초`;
}

/** 곁질문 AI에게 줄 지금 진행 상황(화면에는 보이지 않는다). 개요를 먼저 쓰고, 워커마다 짧게, 출력 끝부분은 남은 길이에 맞춘다. */
function progressOf(l: Live, queue: QueueView): string {
  const now = Date.now();
  const all = Object.values(l.agents).sort((a, b) => a.startedAt - b.startedAt);
  const workers = all.filter((a) => a.key.startsWith("agent:"));
  const others = all.filter((a) => !a.key.startsWith("agent:") && !a.key.startsWith("lead"));
  const count = (xs: AgentRun[], f: (a: AgentRun) => boolean) => xs.filter(f).length;
  const lines = [`[개요] 경과 ${secs(now - l.startedAt)} · ${l.work ? "파일 작업" : "대화"}${l.lead ? " · 병렬 작업(총괄 있음)" : ""}`];
  if (workers.length) {
    lines.push(`워커 ${workers.length}명: 완료 ${count(workers, (a) => a.state === "ok")} · 실패/중지 ${count(workers, (a) => a.state === "fail")} · 작업 중 ${count(workers, (a) => a.state === "run" && !a.wait)} · 대기 ${count(workers, (a) => a.state === "run" && !!a.wait)}`);
  }
  if (l.reasons.length) lines.push(`계획: ${l.reasons.join(" / ")}`);
  if (l.lead) {
    const lead = l.lead;
    const busy = ["lead", "lead-work"].map((k) => l.agents[k]).filter((a) => a?.state === "run");
    lines.push(`총괄(${lead.label}): ${busy.length ? busy.map((a) => (a.key === "lead-work" ? "메인 폴더에서 직접 작업 중" : "판단 중")).join(", ") : "대기"} · 커밋 ${lead.commits.length}개 · 명령 판단 ${lead.decisions.length}건 · 열린 질문 ${lead.questions.length}개`);
    for (const q of lead.questions) lines.push(`  사용자에게 묻는 중: ${q.question}`);
  }
  const tests = [...queue.running.map((t) => ({ t, s: "실행 중" })), ...queue.waiting.map((t, i) => ({ t, s: `대기 ${i + 1}번` }))].filter((x) => x.t.task === l.taskId);
  if (tests.length) lines.push(`창 테스트: ${tests.map((x) => `${x.s} ${x.t.command}`).join(" / ")}`);
  const row = (a: AgentRun) => {
    const part = l.parts[a.key];
    const state = a.state === "run" ? (a.wait ? WAIT_NOTE[a.wait] ?? a.wait : "작업 중") : a.state === "ok" ? "완료" : a.stopped ? "중지" : "실패";
    const took = a.state === "run" ? now - a.startedAt : a.ms ?? 0;
    const u = l.usage[a.key];
    const tok = u ? u.done.output + u.cur.output : 0;
    const recent = a.activities.slice(-3).join(" → ");
    return [
      `- ${part?.title ?? a.label} [${a.label}${a.model ? ` · ${a.model}` : ""}] ${state}${a.bg ? ` · 백그라운드 명령 ${a.bg}개 실행 중` : ""} · ${secs(took)} · 도구 ${a.activities.length}회${tok ? ` · 출력 토큰 ${tok}` : ""}`,
      part?.files.length ? `  맡은 파일: ${part.files.slice(0, 6).join(", ")}${part.files.length > 6 ? " …" : ""}` : "",
      recent ? `  최근 활동: ${recent}` : "",
      a.error && a.state === "fail" ? `  오류: ${a.error}` : "",
    ].filter(Boolean);
  };
  if (workers.length) lines.push("", "[워커별]");
  for (const a of workers) lines.push(...row(a));
  if (others.length) lines.push("", "[그 밖의 실행]");
  for (const a of others) lines.push(...row(a));
  if (l.lead?.timeline.length) {
    lines.push("", "[총괄 최근 활동]");
    for (const n of l.lead.timeline.slice(-10)) lines.push(`- ${n.kind}: ${n.text}`);
  }
  if (l.lead?.commits.length) {
    lines.push("", "[커밋]");
    for (const c of l.lead.commits.slice(-8)) lines.push(`- ${c.sha} ${c.message} (${c.files.length}개 파일)`);
  }
  // 출력 끝부분은 개요 뒤에 남은 길이를 실행 중인 것부터 나눠 쓴다.
  const head = lines.join("\n");
  const tails = [...all.filter((a) => a.state === "run"), ...all.filter((a) => a.state !== "run")].filter((a) => l.streams[a.key]?.trim());
  const room = PROGRESS_CHARS - head.length - (l.text ? 1300 : 0) - 200;
  const each = tails.length ? Math.max(0, Math.min(900, Math.floor(room / tails.length) - 40)) : 0;
  const out = [head];
  if (each >= 120) {
    out.push("", "[실행별 출력 끝부분]");
    for (const a of tails) out.push(`- ${l.parts[a.key]?.title ?? a.label}: …${hideBlocks(l.streams[a.key]).slice(-each).replace(/\s+/g, " ")}`);
  }
  if (l.text) out.push("", `[지금 쓰는 답의 끝부분]\n${l.text.slice(-1200)}`);
  return out.join("\n");
}

export function AppProvider({ children }: { children: ReactNode }) {
  const [engines, setEngines] = useState<EngineInfo[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [conversationId, setConversationId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [live, setLive] = useState<Record<string, Live>>({});
  const [queue, setQueue] = useState<QueueView>({ capacity: 1, running: [], waiting: [], recent: [] });
  const [testLogs, setTestLogs] = useState<Record<number, string[]>>({});
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [skills, setSkills] = useState<SkillsView | null>(null);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [newFolder, setNewFolder] = useState<string | null>(null);
  const [sideThreads, setSideThreads] = useState<Record<string, SideTurn[]>>({});

  const toast = useCallback((text: string, kind: Toast["kind"] = "info") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t.slice(-3), { id, text, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "error" ? 7000 : 3200);
  }, []);

  const refreshConversations = useCallback(() => {
    api.conversationList().then(setConversations).catch(() => {});
  }, []);
  const loadMessages = useCallback(
    (id: string | null) => {
      if (!id) return setMessages([]);
      api.conversationMessages(id).then(setMessages).catch((e) => toast(errorText(e), "error"));
    },
    [toast],
  );
  const refreshSkills = useCallback(async () => {
    try {
      setSkills(await api.skillsList());
    } catch {
      /* 다음에 다시 */
    }
  }, []);

  const refreshEngines = useCallback(async () => {
    try {
      setEngines(await api.enginesRefresh());
    } catch (e) {
      toast(errorText(e), "error");
    }
  }, [toast]);

  useEffect(() => {
    api.settingsGet().then(setSettings).catch((e) => toast(errorText(e), "error"));
    refreshConversations();
    refreshSkills();
    api.approvalList().then(setApprovals).catch(() => {});
    // 시작할 때 백엔드가 CLI를 찾는 데 몇 초 걸린다.
    let tries = 0;
    const poll = () =>
      api
        .enginesList()
        .then((e) => {
          setEngines(e);
          if (e.length === 0 && tries++ < 30) setTimeout(poll, 500);
        })
        .catch(() => {});
    poll();
  }, [refreshConversations, refreshSkills, toast]);

  useEffect(() => loadMessages(conversationId), [conversationId, loadMessages]);

  const patch = (taskId: string, f: (l: Live) => Live) => setLive((m) => (m[taskId] ? { ...m, [taskId]: f(m[taskId]) } : m));
  // 글자 조각은 모아 두었다가 짧은 간격마다 한 번에 반영한다. 병렬 에이전트가 많으면 조각마다 다시 그려 화면이 느려진다.
  const pending = useRef<{ text: Record<string, string>; streams: Record<string, Record<string, string>>; timer: number | null }>({ text: {}, streams: {}, timer: null });
  const flushDeltas = useCallback(() => {
    const q = pending.current;
    if (q.timer != null) window.clearTimeout(q.timer);
    q.timer = null;
    const { text, streams } = q;
    if (!Object.keys(text).length && !Object.keys(streams).length) return;
    q.text = {};
    q.streams = {};
    setLive((m) => {
      const out = { ...m };
      for (const [id, d] of Object.entries(text)) if (out[id]) out[id] = { ...out[id], text: out[id].text + d };
      for (const [id, keys] of Object.entries(streams)) {
        const l = out[id];
        if (!l) continue;
        const next = { ...l.streams };
        for (const [k, d] of Object.entries(keys)) {
          const t = (next[k] ?? "") + d;
          next[k] = l.council || t.length <= STREAM_KEEP ? t : t.slice(-STREAM_KEEP);
        }
        out[id] = { ...l, streams: next };
      }
      return out;
    });
  }, []);
  const queueDeltas = () => {
    if (pending.current.timer == null) pending.current.timer = window.setTimeout(flushDeltas, isSleeping() ? SLEEP_FLUSH_MS : DELTA_FLUSH_MS);
  };
  useEffect(
    () => () => {
      if (pending.current.timer != null) window.clearTimeout(pending.current.timer);
    },
    [],
  );

  useTauriEvent<{ taskId: string; conversationId: string; messageId: string; primary: EngineId; reviewer: EngineId | null; work: boolean; skill: SkillRef | null; reasons: string[]; jev: Judgment | null; budget: Budget | null; debate: DebatePlan | null; team: TeamPlan | null; council: CouncilPlan | null; model: string | null }>("task:started", (p) =>
    setLive((m) => ({
      ...m,
      [p.taskId]: { ...(m[p.taskId] ?? blank(p.taskId, p.conversationId, p.work)), messageId: p.messageId, primary: p.primary, reviewer: p.reviewer, work: p.work, skill: p.skill, reasons: p.reasons, jev: p.jev, budget: p.budget, debate: p.debate ?? null, team: p.team ?? null, council: p.council ?? null, model: p.model ?? null },
    })),
  );
  useTauriEvent<{ taskId: string; conversationId: string; key: string; engine: EngineId; label: string; role: string; state: AgentRun["state"]; model?: string | null; effort?: string | null; ms?: number; error?: string | null; chars?: number; stopped?: boolean; inputTokens?: number | null; outputTokens?: number | null; costUsd?: number | null }>("task:agent", (p) =>
    setLive((m) => {
      const l = m[p.taskId] ?? blank(p.taskId, p.conversationId, false);
      if (l.gone[p.key]) return m;
      const prev = l.agents[p.key];
      const run: AgentRun =
        p.state === "run"
          ? // 같은 워커의 다음 차례는 앞선 기록을 이어 간다.
            { key: p.key, engine: p.engine, label: p.label, role: p.role, model: p.model ?? null, effort: p.effort ?? null, state: "run", startedAt: prev?.startedAt ?? Date.now(), ms: null, error: null, chars: null, activities: prev?.activities ?? [], stopped: false, sent: prev?.sent ?? [], wait: null, bg: prev?.bg ?? 0 }
          : { ...(prev ?? { key: p.key, engine: p.engine, label: p.label, role: p.role, model: null, effort: null, startedAt: Date.now(), activities: [], sent: [] }), state: p.state, ms: p.ms ?? null, error: p.error ?? null, chars: p.chars ?? null, stopped: !!p.stopped, wait: null };
      return { ...m, [p.taskId]: { ...l, agents: { ...l.agents, [p.key]: run }, usage: { ...l.usage, [p.key]: turnEvent(l.usage[p.key], p) } } };
    }),
  );
  useTauriEvent<{ taskId: string; key: string; input: number; output: number; costUsd: number | null }>("task:agent-usage", (p) => patch(p.taskId, (l) => ({ ...l, usage: { ...l.usage, [p.key]: liveTurn(l.usage[p.key], p) } })));
  useTauriEvent<{ taskId: string; key: string; text: string }>("task:agent-activity", (p) =>
    patch(p.taskId, (l) => {
      const a = l.agents[p.key];
      return a ? { ...l, agents: { ...l.agents, [p.key]: { ...a, activities: [...a.activities, p.text].slice(-ACTIVITY_KEEP) } } } : l;
    }),
  );
  useTauriEvent<{ taskId: string; key: string; note: string }>("task:agent-wait", (p) =>
    patch(p.taskId, (l) => {
      const a = l.agents[p.key];
      return a ? { ...l, agents: { ...l.agents, [p.key]: { ...a, state: "run", wait: p.note } } } : l;
    }),
  );
  useTauriEvent<BgJob>("task:bg-job", (j) => patch(j.taskId, (l) => ({ ...l, bg: { ...l.bg, [j.id]: j } })));
  useTauriEvent<{ taskId: string; key: string; count: number }>("task:agent-bg", (p) =>
    patch(p.taskId, (l) => {
      const a = l.agents[p.key];
      return a ? { ...l, agents: { ...l.agents, [p.key]: { ...a, bg: p.count } } } : l;
    }),
  );
  useTauriEvent<{ taskId: string; conversationId: string; engine: EngineId; label: string; model: string | null }>("lead:started", (p) =>
    setLive((m) => {
      const l = m[p.taskId] ?? blank(p.taskId, p.conversationId, true);
      return { ...m, [p.taskId]: { ...l, lead: { engine: p.engine, label: p.label, model: p.model, said: [], policy: { allowAll: false, rules: [] }, decisions: [], questions: [], groups: [], groupLog: [], org: [], orgLog: [], timeline: [], commits: [] } } };
    }),
  );
  const patchLead = (taskId: string, fn: (d: LeadState) => LeadState) => patch(taskId, (l) => (l.lead ? { ...l, lead: fn(l.lead) } : l));
  useTauriEvent<{ taskId: string; text: string }>("lead:said", (p) => patchLead(p.taskId, (d) => ({ ...d, said: [...d.said, { who: "lead", text: p.text }] })));
  useTauriEvent<{ taskId: string; text: string }>("lead:user", (p) => patchLead(p.taskId, (d) => ({ ...d, said: [...d.said, { who: "user", text: p.text }] })));
  useTauriEvent<LeadDecision>("lead:decision", (p) => patchLead(p.taskId, (d) => ({ ...d, decisions: [...d.decisions, p] })));
  useTauriEvent<{ taskId: string; policy: LeadPolicy }>("lead:policy", (p) => patchLead(p.taskId, (d) => ({ ...d, policy: p.policy })));
  useTauriEvent<LeadQuestion>("lead:question", (p) => patchLead(p.taskId, (d) => ({ ...d, questions: [...d.questions.filter((q) => q.id !== p.id), p] })));
  useTauriEvent<{ id: string }>("lead:question-closed", (p) =>
    setLive((m) => Object.fromEntries(Object.entries(m).map(([k, l]) => [k, l.lead ? { ...l, lead: { ...l.lead, questions: l.lead.questions.filter((q) => q.id !== p.id) } } : l]))),
  );
  useTauriEvent<{ taskId: string; groups: GroupInfo[] }>("task:groups", (p) => patchLead(p.taskId, (d) => ({ ...d, groups: p.groups })));
  useTauriEvent<GroupNote>("group:say", (p) => patchLead(p.taskId, (d) => ({ ...d, groupLog: [...d.groupLog, p] })));
  useTauriEvent<{ taskId: string; units: OrgUnit[] }>("task:org", (p) => patchLead(p.taskId, (d) => ({ ...d, org: p.units })));
  useTauriEvent<OrgPost>("org:post", (p) => patchLead(p.taskId, (d) => ({ ...d, orgLog: [...d.orgLog, p].slice(-ORG_LOG_KEEP) })));
  useTauriEvent<{ taskId: string } & LeadNote>("lead:activity", (p) => patchLead(p.taskId, (d) => ({ ...d, timeline: [...d.timeline, p].slice(-TIMELINE_KEEP) })));
  useTauriEvent<LeadCommit>("lead:commit", (p) => patchLead(p.taskId, (d) => ({ ...d, commits: [...d.commits, p] })));
  useTauriEvent<QueueView>("queue:changed", (v) => {
    setQueue(v);
    // 대기열에서 사라진 테스트의 로그는 버린다.
    const keep = new Set([...v.running, ...v.waiting, ...v.recent].map((t) => t.id));
    setTestLogs((m) => (Object.keys(m).some((id) => !keep.has(Number(id))) ? Object.fromEntries(Object.entries(m).filter(([id]) => keep.has(Number(id)))) : m));
  });
  useTauriEvent<{ id: number; lines: string[] }>("test:log", (p) => setTestLogs((m) => ({ ...m, [p.id]: [...(m[p.id] ?? []), ...p.lines].slice(-TEST_LOG_KEEP) })));
  const loadTestLog = useCallback((id: number) => {
    api
      .testLog(id)
      .then((lines) => setTestLogs((m) => ({ ...m, [id]: lines.length >= (m[id]?.length ?? 0) ? lines : m[id] })))
      .catch(() => {});
  }, []);
  useEffect(() => {
    api.queueGet().then(setQueue).catch(() => {});
  }, []);
  // 별도 창이 열리면 지금까지의 상태를 채워 준다.
  const liveRef = useRef(live);
  liveRef.current = live;
  useTauriEvent<{ label: string; task: string; keys: string[] }>("worker-window:hello", (p) => {
    const l = liveRef.current[p.task];
    const pick = <T,>(from: Record<string, T> | undefined) => Object.fromEntries(p.keys.filter((k) => from?.[k] !== undefined).map((k) => [k, from![k]]));
    const groupIndex = l?.lead?.groups.find((g) => g.keys.some((k) => p.keys.includes(k)))?.index;
    const unit = l?.lead?.org.find((u) => u.keys.some((k) => p.keys.includes(k))) ?? null;
    const orgLog = unit ? (l?.lead?.orgLog ?? []).filter((x) => x.from === unit.index || x.to.includes(unit.index)) : [];
    emit("worker-window:snapshot", { label: p.label, runs: pick(l?.agents), streams: pick(l?.streams), parts: pick(l?.parts), usage: pick(l?.usage), decisions: l?.lead?.decisions.filter((d) => p.keys.includes(d.key)) ?? [], groupLog: l?.lead?.groupLog.filter((g) => g.group === groupIndex) ?? [], unit, orgLog });
  });
  useTauriEvent<{ taskId: string; key: string; text: string }>("task:agent-message", (p) =>
    patch(p.taskId, (l) => {
      const a = l.agents[p.key];
      return a ? { ...l, agents: { ...l.agents, [p.key]: { ...a, sent: [...a.sent, p.text] } } } : l;
    }),
  );
  // 회의 창과 에이전트 탭이 실행마다 지금까지 쓴 글을 보여 준다.
  useTauriEvent<{ taskId: string; key: string; delta: string }>("task:agent-delta", (p) => {
    const keys = (pending.current.streams[p.taskId] ??= {});
    keys[p.key] = (keys[p.key] ?? "") + p.delta;
    queueDeltas();
  });
  useTauriEvent<{ taskId: string; conversationId: string; parts: ({ key: string } & PartInfo)[] }>("task:parts", (p) =>
    setLive((m) => {
      const l = m[p.taskId] ?? blank(p.taskId, p.conversationId, true);
      // 총괄이 단계 중에 더한 워커는 다음 단계 목록에 없으니 덮지 않고 합친다.
      return { ...m, [p.taskId]: { ...l, parts: { ...l.parts, ...Object.fromEntries(p.parts.map((x) => [x.key, { title: x.title, files: x.files }])) } } };
    }),
  );
  // 총괄이 단계 중에 워커를 새로 만들었다.
  useTauriEvent<{ taskId: string; conversationId: string; key: string; title: string; files: string[] }>("task:part-added", (p) =>
    setLive((m) => {
      const l = m[p.taskId] ?? blank(p.taskId, p.conversationId, true);
      return { ...m, [p.taskId]: { ...l, parts: { ...l.parts, [p.key]: { title: p.title, files: p.files } } } };
    }),
  );
  // 총괄이 워커를 지웠다: 카드·글·사용량을 바로 뺀다.
  useTauriEvent<{ taskId: string; key: string }>("task:agent-removed", (p) =>
    patch(p.taskId, (l) => {
      const drop = <T,>(r: Record<string, T>) => Object.fromEntries(Object.entries(r).filter(([k]) => k !== p.key));
      const lead = l.lead && { ...l.lead, groups: l.lead.groups.map((g) => ({ ...g, keys: g.keys.filter((k) => k !== p.key) })).filter((g) => g.keys.length > 0) };
      return { ...l, lead, agents: drop(l.agents), parts: drop(l.parts), streams: drop(l.streams), usage: drop(l.usage), gone: { ...l.gone, [p.key]: true } };
    }),
  );
  // 병렬 작업을 나눌 수 없어 한 AI로 돌아갈 때
  useTauriEvent<{ taskId: string; team: TeamPlan | null; reasons: string[] }>("task:plan", (p) => patch(p.taskId, (l) => ({ ...l, team: p.team, reasons: p.reasons })));
  useTauriEvent<{ taskId: string; engine: EngineId; role: string; state: "run" | "ok" | "fail" }>("task:debate", (p) =>
    patch(p.taskId, (l) => ({ ...l, debateState: { ...l.debateState, [`${p.engine}:${p.role}`]: p.state } })),
  );
  useTauriEvent<{ taskId: string; phase: "draft" | "review"; label: string; effort: string | null; model: string | null }>("task:phase", (p) => {
    // 앞 단계의 글자가 새 단계에 붙지 않게 먼저 반영한다.
    flushDeltas();
    patch(p.taskId, (l) => ({ ...l, phase: p.phase, label: p.label, effort: p.effort, model: p.model, text: "", draft: p.phase === "review" ? l.text : l.draft }));
  });
  useTauriEvent<{ taskId: string; delta: string }>("task:delta", (p) => {
    pending.current.text[p.taskId] = (pending.current.text[p.taskId] ?? "") + p.delta;
    queueDeltas();
  });
  useTauriEvent<{ taskId: string; text: string }>("task:activity", (p) => patch(p.taskId, (l) => ({ ...l, activities: [...l.activities, p.text].slice(-12) })));
  useTauriEvent<Approval>("task:approval", (a) => setApprovals((x) => [...x.filter((y) => y.id !== a.id), a]));
  useTauriEvent<Approval>("approval:decided", (a) => setApprovals((x) => x.filter((y) => y.id !== a.id)));
  useTauriEvent<{ conversationId: string; taskId: string }>("message:created", (p) => {
    if (p.conversationId === conversationId) loadMessages(conversationId);
    setApprovals((x) => x.filter((y) => y.taskId !== p.taskId));
  });
  useTauriEvent<{ taskId: string }>("task:done", (p) => {
    setLive(({ [p.taskId]: _, ...rest }) => rest);
    refreshConversations();
  });
  const patchSide = useCallback((id: string, f: (t: SideTurn) => SideTurn) => {
    setSideThreads((m) => Object.fromEntries(Object.entries(m).map(([k, v]) => [k, v.map((t) => (t.id === id ? f(t) : t))])));
  }, []);
  useTauriEvent<{ sideId: string; engine: EngineId }>("side:started", (p) => patchSide(p.sideId, (t) => ({ ...t, engine: p.engine, text: "", activity: null })));
  useTauriEvent<{ sideId: string; delta: string }>("side:delta", (p) => patchSide(p.sideId, (t) => ({ ...t, text: t.text + p.delta })));
  useTauriEvent<{ sideId: string; text: string }>("side:activity", (p) => patchSide(p.sideId, (t) => ({ ...t, activity: p.text })));
  useTauriEvent<{ sideId: string; ok: boolean; text: string; error: string | null; engine: EngineId | null; stopped: boolean }>("side:done", (p) =>
    patchSide(p.sideId, (t) => ({ ...t, state: p.ok ? "ok" : "fail", text: p.text || t.text, engine: p.engine ?? t.engine, error: p.error, stopped: p.stopped, activity: null })),
  );
  // 키가 거절된 API를 백엔드가 자동 선택에서 뺐을 때
  useTauriEvent("engines:changed", () => {
    api.enginesList().then(setEngines).catch(() => {});
  });
  useTauriEvent("settings:changed", () => {
    api.settingsGet().then(setSettings).catch(() => {});
  });
  useTauriEvent("data:cleared", () => {
    setConversationId(null);
    setMessages([]);
    setSideThreads({});
    refreshConversations();
  });

  const send = useCallback(
    async (text: string, engine: EngineChoice, work: boolean, build: boolean, council = false, attachments: string[] = []) => {
      try {
        const r = await api.chatSend(conversationId, text, engine, work, build, conversationId ? null : newFolder, council, attachments);
        if (!conversationId) setNewFolder(null);
        setLive((m) => ({ ...m, [r.taskId]: m[r.taskId] ?? blank(r.taskId, r.conversationId, !council && (work || build)) }));
        if (r.conversationId !== conversationId) setConversationId(r.conversationId);
        else loadMessages(r.conversationId);
        refreshConversations();
        return true;
      } catch (e) {
        toast(errorText(e), "error");
        return false;
      }
    },
    [conversationId, loadMessages, refreshConversations, toast, newFolder],
  );

  const chooseFolder = useCallback(
    async (id: string | null) => {
      try {
        const p = await api.pickFolder();
        if (!p) return;
        if (id) {
          await api.conversationSetWorkspace(id, p);
          refreshConversations();
          toast("이 세션의 폴더를 바꿨습니다. 이어 쓰던 CLI 세션은 새로 시작합니다.");
        } else setNewFolder(p);
      } catch (e) {
        toast(errorText(e), "error");
      }
    },
    [refreshConversations, toast],
  );

  const resetFolder = useCallback(
    async (id: string | null) => {
      try {
        if (id) {
          await api.conversationSetWorkspace(id, null);
          refreshConversations();
        } else setNewFolder(null);
      } catch (e) {
        toast(errorText(e), "error");
      }
    },
    [refreshConversations, toast],
  );

  const cancel = useCallback((taskId: string) => {
    api.taskCancel(taskId).catch(() => {});
  }, []);
  const cancelAgent = useCallback((taskId: string, key: string) => {
    api.agentCancel(taskId, key).catch((e) => toast(errorText(e), "error"));
  }, [toast]);
  const cancelAll = useCallback(() => {
    api.cancelAll().catch(() => {});
  }, []);

  const sideKey = conversationId ?? "new";
  const side = useMemo(() => sideThreads[sideKey] ?? [], [sideThreads, sideKey]);
  const askSide = useCallback(
    async (question: string) => {
      const id = crypto.randomUUID();
      const running = Object.values(live).filter((l) => l.conversationId === conversationId);
      const progress = running.length ? running.map((l) => progressOf(l, queue)).join("\n\n") : null;
      const thread = side.filter((t) => t.text.trim() && t.state !== "fail").map((t) => ({ role: t.role, text: t.text }));
      setSideThreads((m) => ({ ...m, [sideKey]: [...(m[sideKey] ?? []), { id: `${id}:q`, role: "user", text: question }, { id, role: "assistant", text: "", state: "run" }] }));
      try {
        await api.sideAsk(id, conversationId, conversationId ? null : newFolder, question, progress, thread);
      } catch (e) {
        patchSide(id, (t) => ({ ...t, state: "fail", error: errorText(e) }));
      }
    },
    [conversationId, live, newFolder, patchSide, side, sideKey, queue],
  );
  const stopSide = useCallback(() => {
    for (const t of side) if (t.state === "run") api.taskCancel(t.id).catch(() => {});
  }, [side]);
  const clearSide = useCallback(() => {
    stopSide();
    setSideThreads((m) => ({ ...m, [sideKey]: [] }));
  }, [sideKey, stopSide]);

  const decide = useCallback(
    async (id: string, approve: boolean) => {
      try {
        await api.approvalDecide(id, approve);
        setApprovals((x) => x.filter((y) => y.id !== id));
      } catch (e) {
        toast(errorText(e), "error");
      }
    },
    [toast],
  );

  const deleteConversation = useCallback(
    async (id: string) => {
      try {
        await api.conversationDelete(id);
        if (id === conversationId) setConversationId(null);
        refreshConversations();
      } catch (e) {
        toast(errorText(e), "error");
      }
    },
    [conversationId, refreshConversations, toast],
  );

  const saveSettings = useCallback(
    async (s: Settings) => {
      try {
        setSettings(await api.settingsUpdate(s));
      } catch (e) {
        toast(errorText(e), "error");
      }
    },
    [toast],
  );

  const value = useMemo<AppCtx>(
    () => ({
      queue,
      testLogs,
      loadTestLog,
      engines,
      setEngines,
      newFolder,
      setNewFolder,
      chooseFolder,
      resetFolder,
      refreshEngines,
      settings,
      saveSettings,
      conversations,
      conversationId,
      openConversation: setConversationId,
      deleteConversation,
      messages,
      live: Object.values(live),
      approvals,
      decide,
      send,
      cancel,
      cancelAgent,
      cancelAll,
      side,
      askSide,
      stopSide,
      clearSide,
      skills,
      refreshSkills,
      dialog,
      setDialog,
      toasts,
      toast,
    }),
    [queue, testLogs, loadTestLog, engines, newFolder, chooseFolder, resetFolder, refreshEngines, settings, saveSettings, conversations, conversationId, deleteConversation, messages, live, approvals, decide, send, cancel, cancelAgent, cancelAll, side, askSide, stopSide, clearSide, skills, refreshSkills, dialog, toasts, toast],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

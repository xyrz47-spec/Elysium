import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { api, errorText, type Approval, type Conversation, type EngineChoice, type EngineId, type EngineInfo, type Budget, type CouncilPlan, type DebatePlan, type Judgment, type Message, type Settings, type SkillRef, type SkillsView, type TeamPlan } from "./lib/api";
import { useTauriEvent } from "./lib/events";

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
}

/** 병렬 작업의 하위 작업(실행 key → 이름·맡은 파일) */
export interface PartInfo {
  title: string;
  files: string[];
}

/** 에이전트 하나가 지금까지 쓴 글은 끝부분만 둔다(오래 도는 작업에서 메모리를 아낀다). */
const STREAM_KEEP = 20000;

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
  /** 여러 AI가 동시에 쓰는 글(실행 key → 지금까지 받은 글). 회의 창이 보여 준다. */
  streams: Record<string, string>;
  /** 실행 key → 상태(시작한 순서대로) */
  agents: Record<string, AgentRun>;
  /** 병렬 작업일 때 실행 key → 하위 작업 */
  parts: Record<string, PartInfo>;
  startedAt: number;
  label: string | null;
  text: string;
  draft: string;
  activities: string[];
}

export type Dialog = null | "settings" | "skills" | "usage";

interface AppCtx {
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
  send: (text: string, engine: EngineChoice, work: boolean, build: boolean, council?: boolean) => Promise<void>;
  cancel: (taskId: string) => void;
  /** 에이전트 하나만 멈춘다 */
  cancelAgent: (taskId: string, key: string) => void;
  /** 실행 중인 모든 작업을 멈춘다 */
  cancelAll: () => void;
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
  agents: {},
  parts: {},
  startedAt: Date.now(),
  effort: null,
  model: null,
  phase: null,
  label: null,
  text: "",
  draft: "",
  activities: [],
});

export function AppProvider({ children }: { children: ReactNode }) {
  const [engines, setEngines] = useState<EngineInfo[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [conversationId, setConversationId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [live, setLive] = useState<Record<string, Live>>({});
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [skills, setSkills] = useState<SkillsView | null>(null);
  const [dialog, setDialog] = useState<Dialog>(null);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [newFolder, setNewFolder] = useState<string | null>(null);

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

  useTauriEvent<{ taskId: string; conversationId: string; messageId: string; primary: EngineId; reviewer: EngineId | null; work: boolean; skill: SkillRef | null; reasons: string[]; jev: Judgment | null; budget: Budget | null; debate: DebatePlan | null; team: TeamPlan | null; council: CouncilPlan | null; model: string | null }>("task:started", (p) =>
    setLive((m) => ({
      ...m,
      [p.taskId]: { ...(m[p.taskId] ?? blank(p.taskId, p.conversationId, p.work)), messageId: p.messageId, primary: p.primary, reviewer: p.reviewer, work: p.work, skill: p.skill, reasons: p.reasons, jev: p.jev, budget: p.budget, debate: p.debate ?? null, team: p.team ?? null, council: p.council ?? null, model: p.model ?? null },
    })),
  );
  useTauriEvent<{ taskId: string; conversationId: string; key: string; engine: EngineId; label: string; role: string; state: AgentRun["state"]; model?: string | null; effort?: string | null; ms?: number; error?: string | null; chars?: number; stopped?: boolean }>("task:agent", (p) =>
    setLive((m) => {
      const l = m[p.taskId] ?? blank(p.taskId, p.conversationId, false);
      const prev = l.agents[p.key];
      const run: AgentRun =
        p.state === "run"
          ? { key: p.key, engine: p.engine, label: p.label, role: p.role, model: p.model ?? null, effort: p.effort ?? null, state: "run", startedAt: Date.now(), ms: null, error: null, chars: null, activities: [], stopped: false }
          : { ...(prev ?? { key: p.key, engine: p.engine, label: p.label, role: p.role, model: null, effort: null, startedAt: Date.now(), activities: [] }), state: p.state, ms: p.ms ?? null, error: p.error ?? null, chars: p.chars ?? null, stopped: !!p.stopped };
      return { ...m, [p.taskId]: { ...l, agents: { ...l.agents, [p.key]: run } } };
    }),
  );
  useTauriEvent<{ taskId: string; key: string; text: string }>("task:agent-activity", (p) =>
    patch(p.taskId, (l) => {
      const a = l.agents[p.key];
      return a ? { ...l, agents: { ...l.agents, [p.key]: { ...a, activities: [...a.activities, p.text].slice(-6) } } } : l;
    }),
  );
  useTauriEvent<{ taskId: string; key: string; delta: string }>("task:agent-delta", (p) =>
    // 회의 창과 에이전트 탭이 실행마다 지금까지 쓴 글을 보여 준다.
    patch(p.taskId, (l) => {
      const next = (l.streams[p.key] ?? "") + p.delta;
      return { ...l, streams: { ...l.streams, [p.key]: l.council || next.length <= STREAM_KEEP ? next : next.slice(-STREAM_KEEP) } };
    }),
  );
  useTauriEvent<{ taskId: string; conversationId: string; parts: ({ key: string } & PartInfo)[] }>("task:parts", (p) =>
    setLive((m) => {
      const l = m[p.taskId] ?? blank(p.taskId, p.conversationId, true);
      return { ...m, [p.taskId]: { ...l, parts: Object.fromEntries(p.parts.map((x) => [x.key, { title: x.title, files: x.files }])) } };
    }),
  );
  // 병렬 작업을 나눌 수 없어 한 AI로 돌아갈 때
  useTauriEvent<{ taskId: string; team: TeamPlan | null; reasons: string[] }>("task:plan", (p) => patch(p.taskId, (l) => ({ ...l, team: p.team, reasons: p.reasons })));
  useTauriEvent<{ taskId: string; engine: EngineId; role: string; state: "run" | "ok" | "fail" }>("task:debate", (p) =>
    patch(p.taskId, (l) => ({ ...l, debateState: { ...l.debateState, [`${p.engine}:${p.role}`]: p.state } })),
  );
  useTauriEvent<{ taskId: string; phase: "draft" | "review"; label: string; effort: string | null; model: string | null }>("task:phase", (p) =>
    patch(p.taskId, (l) => ({ ...l, phase: p.phase, label: p.label, effort: p.effort, model: p.model, text: "", draft: p.phase === "review" ? l.text : l.draft })),
  );
  useTauriEvent<{ taskId: string; delta: string }>("task:delta", (p) => patch(p.taskId, (l) => ({ ...l, text: l.text + p.delta })));
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
    refreshConversations();
  });

  const send = useCallback(
    async (text: string, engine: EngineChoice, work: boolean, build: boolean, council = false) => {
      try {
        const r = await api.chatSend(conversationId, text, engine, work, build, conversationId ? null : newFolder, council);
        if (!conversationId) setNewFolder(null);
        setLive((m) => ({ ...m, [r.taskId]: m[r.taskId] ?? blank(r.taskId, r.conversationId, !council && (work || build)) }));
        if (r.conversationId !== conversationId) setConversationId(r.conversationId);
        else loadMessages(r.conversationId);
        refreshConversations();
      } catch (e) {
        toast(errorText(e), "error");
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
      skills,
      refreshSkills,
      dialog,
      setDialog,
      toasts,
      toast,
    }),
    [engines, newFolder, chooseFolder, resetFolder, refreshEngines, settings, saveSettings, conversations, conversationId, deleteConversation, messages, live, approvals, decide, send, cancel, cancelAgent, cancelAll, skills, refreshSkills, dialog, toasts, toast],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

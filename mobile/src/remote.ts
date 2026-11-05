import type { LeadQuestion } from "./Question";
// PC의 엘리시움과 이야기하는 래퍼. 실제 통신은 Rust(지문 고정 TLS)가 하고, 토큰은 화면으로 오지 않는다.
import { invoke } from "@tauri-apps/api/core";
import type { Approval, Attachment, Conversation, EngineInfo, Message, UsageSummary } from "../../src/lib/api";

export type { Approval, Attachment, Conversation, EngineInfo, Message };

export interface Connection {
  pcName: string;
  hosts: string[];
  port: number;
  fingerprint: string;
  /** 다른 네트워크에서도 붙을 수 있는지(PC 터널 키를 받았는지) */
  relay?: boolean;
}

/** PC가 따라가는 작업·에이전트 상태(늦게 붙어도 한 번에 받는다) */
export interface TaskSnap {
  taskId: string;
  conversationId: string;
  work: boolean;
  lead: boolean;
  parts: Record<string, string>;
  agents: Record<string, { key: string; engine: string | null; label: string; role: string; state: "run" | "ok" | "fail"; wait: string | null; last: string | null; stopped: boolean; done: { input: number; output: number; cost: number }; cur: { input: number; output: number; cost: number }; tail: string; tools: number; bg: number }>;
  /** 총괄이 한 일(최근 몇 개) */
  timeline: LeadNoteLite[];
  commits: number;
}

export interface LeadNoteLite {
  kind: string;
  text: string;
  at: string;
}

export interface PcInfo {
  pcName: string;
  version: string;
  allowApprove: boolean;
  device: string;
}

const get = <T,>(path: string) => invoke<T>("call", { method: "GET", path, body: null });
const post = <T,>(path: string, body: unknown = {}) => invoke<T>("call", { method: "POST", path, body });
const del = <T,>(path: string) => invoke<T>("call", { method: "DELETE", path, body: null });

export const remote = {
  connection: () => invoke<Connection | null>("connection"),
  pair: (link: string, device: string) => invoke<Connection>("pair", { link, device }),
  forget: () => invoke<void>("forget"),
  eventsStart: () => invoke<void>("events_start"),
  reconnect: () => invoke<void>("reconnect"),
  info: () => get<PcInfo>("/api/info"),
  engines: () => get<EngineInfo[]>("/api/engines"),
  conversations: () => get<Conversation[]>("/api/conversations"),
  messages: (id: string) => get<Message[]>(`/api/conversations/${encodeURIComponent(id)}/messages`),
  send: (conversationId: string | null, text: string, engine: string, work: boolean, attachments: string[] = []) =>
    post<{ conversationId: string; taskId: string; userMessageId: string }>("/api/send", { conversationId, text, engine, work, attachments }),
  upload: (name: string, dataBase64: string) => post<Attachment>("/api/attachments", { name, dataBase64 }),
  attachment: (id: string) => get<{ attachment: Attachment; dataBase64: string | null }>(`/api/attachments/${encodeURIComponent(id)}`),
  discard: (id: string) => del<{ removed: boolean }>(`/api/attachments/${encodeURIComponent(id)}`),
  cancel: (taskId: string) => post<{ cancelled: boolean }>(`/api/tasks/${encodeURIComponent(taskId)}/cancel`),
  approvals: () => get<Approval[]>("/api/approvals"),
  questions: () => get<LeadQuestion[]>("/api/questions"),
  answerQuestion: (id: string, text: string) => post<{ answered: boolean }>(`/api/questions/${encodeURIComponent(id)}`, { text }),
  decide: (id: string, approve: boolean) => post<Approval>(`/api/approvals/${encodeURIComponent(id)}`, { approve }),
  usage: (range: string) => get<UsageSummary>(`/api/usage?range=${encodeURIComponent(range)}`),
  agents: () => get<TaskSnap[]>("/api/agents"),
  taskMessage: (taskId: string, text: string) => post<{ sent: number }>(`/api/tasks/${encodeURIComponent(taskId)}/message`, { text }),
  agentCancel: (taskId: string, key: string) => post<{ cancelled: boolean }>(`/api/tasks/${encodeURIComponent(taskId)}/agents/${encodeURIComponent(key)}/cancel`),
  agentMessage: (taskId: string, key: string, text: string) => post<{ sent: boolean }>(`/api/tasks/${encodeURIComponent(taskId)}/agents/${encodeURIComponent(key)}/message`, { text }),
  leadSend: (taskId: string, text: string, about: string | null) => post<{ sent: boolean }>(`/api/tasks/${encodeURIComponent(taskId)}/lead`, { text, about }),
  side: (sideId: string, conversationId: string | null, question: string, thread: { role: string; text: string }[]) => post<{ started: boolean }>("/api/side", { sideId, conversationId, question, thread }),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "알 수 없는 오류";
}

/** 기기 이름: 안드로이드 모델명(없으면 Android) */
export function deviceName(): string {
  const m = /Android [\d.]+;\s*([^;)]+)/.exec(navigator.userAgent);
  const model = m?.[1]?.replace(/Build\/.*/, "").trim();
  return model && model !== "K" ? model : "Android";
}

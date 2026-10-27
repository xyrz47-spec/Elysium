// PC의 엘리시움과 이야기하는 래퍼. 실제 통신은 Rust(지문 고정 TLS)가 하고, 토큰은 화면으로 오지 않는다.
import { invoke } from "@tauri-apps/api/core";
import type { Approval, Conversation, EngineInfo, Message, UsageSummary } from "../../src/lib/api";

export type { Approval, Conversation, EngineInfo, Message };

export interface Connection {
  pcName: string;
  hosts: string[];
  port: number;
  fingerprint: string;
}

export interface PcInfo {
  pcName: string;
  version: string;
  allowApprove: boolean;
  device: string;
}

const get = <T,>(path: string) => invoke<T>("call", { method: "GET", path, body: null });
const post = <T,>(path: string, body: unknown = {}) => invoke<T>("call", { method: "POST", path, body });

export const remote = {
  connection: () => invoke<Connection | null>("connection"),
  pair: (link: string, device: string) => invoke<Connection>("pair", { link, device }),
  forget: () => invoke<void>("forget"),
  eventsStart: () => invoke<void>("events_start"),
  info: () => get<PcInfo>("/api/info"),
  engines: () => get<EngineInfo[]>("/api/engines"),
  conversations: () => get<Conversation[]>("/api/conversations"),
  messages: (id: string) => get<Message[]>(`/api/conversations/${encodeURIComponent(id)}/messages`),
  send: (conversationId: string | null, text: string, engine: string, work: boolean) =>
    post<{ conversationId: string; taskId: string; userMessageId: string }>("/api/send", { conversationId, text, engine, work }),
  cancel: (taskId: string) => post<{ cancelled: boolean }>(`/api/tasks/${encodeURIComponent(taskId)}/cancel`),
  approvals: () => get<Approval[]>("/api/approvals"),
  decide: (id: string, approve: boolean) => post<Approval>(`/api/approvals/${encodeURIComponent(id)}`, { approve }),
  usage: (range: string) => get<UsageSummary>(`/api/usage?range=${encodeURIComponent(range)}`),
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

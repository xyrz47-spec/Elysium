// Rust 명령과 1:1로 대응하는 타입이 지정된 IPC 래퍼.
import { invoke } from "@tauri-apps/api/core";

export type CliId = "claude" | "codex" | "antigravity";
export type ApiId = "claude-api" | "openai" | "gemini-api" | "nvidia";
export type EngineId = CliId | ApiId;
export type EngineChoice = "auto" | EngineId;
export type ReviewMode = "auto" | "always" | "off" | "debate";

/** 엔진 이름. 예전 기록의 "gemini"(Gemini CLI)는 Antigravity로 보인다. */
export const ENGINE_LABEL: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  antigravity: "Antigravity",
  gemini: "Antigravity",
  "claude-api": "Claude API",
  openai: "OpenAI API",
  "gemini-api": "Gemini API",
  nvidia: "NVIDIA API",
};

/** API 엔진 → 키 이름 */
export const API_SECRET: Record<ApiId, SecretName> = { "claude-api": "anthropic", openai: "openai", "gemini-api": "gemini", nvidia: "nvidia" };

export interface Settings {
  workspaceDir: string | null;
  review: ReviewMode;
  jevEnabled: boolean;
  models: Record<string, string>;
  /** 대화 요청마다 동시에 돌릴 에이전트 수(1이면 끔, 최대 5) */
  agents: number;
}

export interface EngineInfo {
  id: EngineId;
  label: string;
  kind: "cli" | "api";
  installed: boolean;
  path: string | null;
  /** CLI는 버전, API는 모델 이름 */
  version: string | null;
  verified: boolean;
  installHint: string;
}

export type SecretName = "typesafe" | "anthropic" | "openai" | "gemini" | "nvidia";

export interface SecretStatus {
  name: SecretName;
  present: boolean;
  source: "keyring" | "env" | "none";
}

export interface Conversation {
  id: string;
  title: string;
  workspaceDir: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface Budget {
  tier: "light" | "standard" | "deep";
  effort: "low" | "medium" | "high";
  claudeModel: string | null;
  maxTokens: number;
}

export const TIER_LABEL: Record<Budget["tier"], string> = { light: "가볍게", standard: "보통", deep: "깊게" };

export interface Step {
  engine: EngineId;
  label: string;
  role: "draft" | "review" | "debate" | "rebuttal" | "judge" | "agent" | "synth" | "council" | "plan";
  effort: string | null;
  model: string | null;
  ok: boolean;
  ms: number;
  costUsd: number | null;
  inputTokens: number | null;
  outputTokens: number | null;
  activities: string[];
  error: string | null;
}

export interface SkillRef {
  id: string;
  name: string;
  how: "explicit" | "auto";
}

export interface Judgment {
  model: string;
  task: string | null;
  taskConfidence: number | null;
  difficulty: number | null;
  difficultyConfidence: number | null;
  /** 0~3: 정확도가 얼마나 중요한지(틀리면 얼마나 곤란한지) */
  stakes?: number | null;
  stakesConfidence?: number | null;
  /** 0~1: 최신 웹 정보가 필요할 확률 */
  fresh?: number | null;
  /** 0~1: 이 PC의 파일을 열어 봐야 할 확률 */
  local?: number | null;
  ms: number;
  costUsd: number;
}

/** 토론: 두 AI가 따로 답하고(반론) 판정 AI가 정리 */
export interface DebatePlan {
  a: EngineId;
  b: EngineId;
  judge: EngineId;
  rebuttal: boolean;
}

/** 동시 에이전트: 여러 AI가 같은 요청에 따로 답하고 종합 AI가 정리 */
export interface TeamPlan {
  agents: { engine: EngineId; model: string | null }[];
  synth: EngineId;
}

/** 회의: 여러 AI가 라운드마다 서로의 발언을 읽고 대화하며 푼다 */
export interface CouncilPlan {
  agents: { engine: EngineId; model: string | null }[];
  rounds: number;
  synth: EngineId;
}

export interface CouncilTurn {
  round: number;
  idx: number;
  engine: EngineId;
  ok: boolean;
  text: string;
}

/** JEV가 답 비교·검토 생략을 판단한 기록 */
export interface JevCheck {
  kind: "compare" | "council" | "draft";
  value: number;
  ms: number;
  costUsd: number;
}

export interface MessageData {
  steps?: Step[];
  work?: boolean;
  reasons?: string[];
  skill?: SkillRef | null;
  draft?: string;
  draftedBy?: EngineId;
  reviewedBy?: EngineId;
  judgedBy?: EngineId;
  synthBy?: EngineId;
  /** work: 병렬 작업(하위 작업마다 폴더 사본에서 고친 뒤 합침) — title·files·conflicts가 있다 */
  team?: { agents: { engine: EngineId; model: string | null; ok: boolean; text: string; title?: string; files?: string[] }[]; synth: EngineId | null; work?: boolean; conflicts?: string[] };
  debate?: { participants: EngineId[]; judge: EngineId | null; rounds: { round: number; engine: EngineId; text: string }[] };
  council?: { agents: { engine: EngineId; model: string | null }[]; rounds: CouncilTurn[]; synth: EngineId | null; agreed: boolean; agree: number | null; maxRounds: number };
  jevChecks?: JevCheck[];
  skippedReview?: EngineId;
  primary?: EngineId;
  reviewer?: EngineId | null;
  jev?: Judgment | null;
  budget?: Budget | null;
  error?: string;
  rejected?: boolean;
  engine?: EngineId | null;
  /** '만들기'로 보낸 요청(사용자 메시지에 저장) */
  build?: boolean;
  /** '회의'로 보낸 요청(사용자 메시지에 저장) */
  askedCouncil?: boolean;
}

export interface Message {
  id: string;
  conversationId: string;
  role: "user" | "assistant";
  content: string;
  data: MessageData | null;
  createdAt: string;
}

export interface Approval {
  id: string;
  conversationId: string;
  taskId: string;
  engine: EngineId;
  summary: string;
  workspace: string;
  status: "pending" | "approved" | "rejected" | "expired";
  createdAt: string;
  decidedAt: string | null;
}

export interface Skill {
  id: string;
  name: string;
  description: string;
  engine: EngineId | null;
  review: boolean | null;
  mode: "chat" | "work" | null;
  triggers: string[];
  source: string;
  path: string;
  enabled: boolean;
}

export interface Plugin {
  id: string;
  name: string;
  description: string;
  version: string;
  path: string;
  skills: number;
  extras: string[];
  builtin: boolean;
  enabled: boolean;
  error: string | null;
}

export interface SkillsView {
  skills: Skill[];
  plugins: Plugin[];
  skillsDir: string;
  pluginsDir: string;
}

export interface AppInfo {
  version: string;
  dataDir: string;
  workspace: string;
}

export const api = {
  appInfo: () => invoke<AppInfo>("app_info"),
  settingsGet: () => invoke<Settings>("settings_get"),
  settingsUpdate: (settings: Settings) => invoke<Settings>("settings_update", { settings }),
  enginesList: () => invoke<EngineInfo[]>("engines_list"),
  enginesRefresh: () => invoke<EngineInfo[]>("engines_refresh"),
  secretsStatus: () => invoke<SecretStatus[]>("secrets_status"),
  secretSet: (name: SecretName, value: string) => invoke<EngineInfo[]>("secret_set", { name, value }),
  secretClear: (name: SecretName) => invoke<EngineInfo[]>("secret_clear", { name }),
  apiTest: (engine: ApiId) => invoke<string>("api_test", { engine }),
  jevTest: () => invoke<Judgment>("jev_test"),
  conversationList: () => invoke<Conversation[]>("conversation_list"),
  conversationMessages: (conversationId: string) => invoke<Message[]>("conversation_messages", { conversationId }),
  conversationRename: (conversationId: string, title: string) => invoke<void>("conversation_rename", { conversationId, title }),
  conversationDelete: (conversationId: string) => invoke<void>("conversation_delete", { conversationId }),
  conversationSetWorkspace: (conversationId: string, path: string | null) => invoke<void>("conversation_set_workspace", { conversationId, path }),
  pickFolder: () => invoke<string | null>("pick_folder"),
  /** work: 작업 모드(허락을 받은 뒤 파일 수정) · build: '만들기' — 승인 없이 폴더 안에서 끝까지 완성(작업 모드 포함) */
  chatSend: (conversationId: string | null, text: string, engine: EngineChoice, work: boolean, build: boolean, workspace: string | null, council = false) =>
    invoke<{ conversationId: string; taskId: string; userMessageId: string }>("chat_send", { conversationId, text, engine, work: !council && (work || build), build, workspace, council }),
  taskCancel: (taskId: string) => invoke<boolean>("task_cancel", { taskId }),
  /** 실행 중인 에이전트 하나만 멈춘다(같은 작업의 다른 에이전트는 계속) */
  agentCancel: (taskId: string, key: string) => invoke<boolean>("agent_cancel", { taskId, key }),
  /** 작업 중 메시지: API 작업 모드가 다음 차례에 읽는다. 받지 않는 AI면 오류 */
  taskSteer: (taskId: string, text: string) => invoke<void>("task_steer", { taskId, text }),
  cancelAll: () => invoke<number>("cancel_all"),
  approvalList: () => invoke<Approval[]>("approval_list"),
  approvalDecide: (approvalId: string, approve: boolean) => invoke<Approval>("approval_decide", { approvalId, approve }),
  skillsList: () => invoke<SkillsView>("skills_list"),
  skillToggle: (id: string, enabled: boolean) => invoke<void>("skill_toggle", { id, enabled }),
  pluginToggle: (id: string, enabled: boolean) => invoke<void>("plugin_toggle", { id, enabled }),
  skillCreate: (name: string) => invoke<string>("skill_create", { name }),
  openPath: (kind: string) => invoke<void>("open_path", { kind }),
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  usageToday: () => invoke<{ calls: number; costUsd: number }>("usage_today"),
  memoryGet: (conversationId: string) => invoke<MemoryView>("memory_get", { conversationId }),
  conversationMarkdown: (conversationId: string) => invoke<string>("conversation_markdown", { conversationId }),
  conversationExport: (conversationId: string) => invoke<string>("conversation_export", { conversationId }),
  memoryClear: (conversationId: string) => invoke<MemoryView>("memory_clear", { conversationId }),
  usageSummary: (range: UsageRange) => invoke<UsageSummary>("usage_summary", { range }),
  pricingList: () => invoke<ModelPrice[]>("pricing_list"),
  remoteStatus: () => invoke<RemoteStatus>("remote_status"),
  remoteConfigure: (enabled: boolean, allowApprove: boolean) => invoke<RemoteStatus>("remote_configure", { enabled, allowApprove }),
  remotePairStart: () => invoke<PairInfo>("remote_pair_start"),
  remotePairCancel: () => invoke<void>("remote_pair_cancel"),
  remoteRevoke: (id: string) => invoke<RemoteStatus>("remote_revoke", { id }),
  dataDeleteAll: (confirm: string) => invoke<void>("data_delete_all", { confirm }),
};

/** 원격 제어(폰 앱) 상태. 토큰 값은 PC에도 남기지 않는다(해시만). */
export interface RemoteStatus {
  config: { enabled: boolean; port: number; allowApprove: boolean };
  running: number | null;
  addresses: { ip: string; kind: "lan" | "tailscale"; name: string }[];
  fingerprint: string | null;
  devices: { id: string; name: string; createdAt: string; lastSeenAt: string | null }[];
}

export interface PairInfo {
  link: string;
  qrSvg: string;
  expiresIn: number;
}

/** 세션 메모리: 이 세션의 모든 AI가 같이 쓰는 요약 */
export interface MemoryView {
  summary: string | null;
  upto: string | null;
  messages: number;
}

export type UsageRange = "today" | "7d" | "30d" | "all";

/** 엔진(CLI·API·JEV) 하나의 기간 사용량 */
export interface EngineUsage {
  engine: string;
  kind: "cli" | "api" | "jev";
  calls: number;
  failed: number;
  drafts: number;
  reviews: number;
  inputTokens: number;
  outputTokens: number;
  /** CLI는 CLI가 알려 준 추정치(구독 요금제면 실제 청구액이 아님) */
  costUsd: number;
  avgLatencyMs: number;
  lastAt: string | null;
}

export interface ModelPrice {
  engine: string;
  engineLabel: string;
  kind: "cli" | "api" | "jev";
  model: string;
  displayName: string;
  inputUsdPerM: number;
  outputUsdPerM: number;
  /** 캐시에서 읽은 입력 1M 토큰당 */
  cachedUsdPerM?: number;
  description: string;
}

export interface UsageSummary {
  range: UsageRange;
  engines: EngineUsage[];
  days: { date: string; calls: number; tokens: number; costUsd: number }[];
  pricing?: ModelPrice[];
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

/** 경로의 마지막 폴더 이름 */
export function baseName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

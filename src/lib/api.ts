// Rust 명령과 1:1로 대응하는 타입이 지정된 IPC 래퍼.
import { invoke } from "@tauri-apps/api/core";

export type CliId = "claude" | "codex" | "antigravity" | "opencode";
export type ApiId = "claude-api" | "openai" | "gemini-api" | "nvidia";
export type EngineId = CliId | ApiId;
export type EngineChoice = "auto" | EngineId;
export type ReviewMode = "auto" | "always" | "off" | "debate";

/** One seat of a team built in the work setup panel. model: a name from workModels(). */
export interface SetupSeat {
  model: string;
  effort: string | null;
  count: number;
}
/** dept: departments above the team, "개발/프론트" for two levels. Empty when not grouped. */
export interface SetupGroup {
  dept: string;
  members: SetupSeat[];
}
export interface SetupTeam {
  lead: SetupSeat | null;
  groups: SetupGroup[];
}
/** How one request runs, set before sending. null / 0 fields follow the settings and automatic choices. */
export interface WorkSetup {
  team: SetupTeam | null;
  agents: number;
  tier: "light" | "standard" | "deep" | null;
  review: ReviewMode | null;
  leadCommit: boolean | null;
  roadmap: boolean | null;
  jev: boolean | null;
  maxTurns: number | null;
}

/** 엔진 이름. 예전 기록의 "gemini"(Gemini CLI)는 Antigravity로 보인다. */
export const ENGINE_LABEL: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  antigravity: "Antigravity",
  gemini: "Antigravity",
  opencode: "OpenCode",
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
  /** 입력하는 동안 JEV가 모드·스킬을 추천한다 */
  jevSuggest: boolean;
  /** 창을 띄우는 실행(창 테스트)을 동시에 몇 개까지 돌릴지(앱 전체, 1~8) */
  testSlots: number;
  /** 병렬 작업에서 워커가 끝낼 때마다 총괄이 변경을 검토·보정한 뒤 커밋한다 */
  leadCommit: boolean;
  /** 모든 AI에게 미리 알려 두는 말(나에 대해·답변 방식) */
  instructions: string;
  /** 답변 언어 */
  answerLang: AnswerLang;
  /** 답변 길이 */
  answerLength: AnswerLength;
  /** 자주 쓰는 프롬프트 */
  prompts: SavedPrompt[];
  /** 작업이 끝났을 때 창이 앞에 없으면 작업 표시줄에서 알린다 */
  notifyDone: boolean;
  /** Shut down / restart / log off after a task, when the user asked for it in that task */
  powerOnDone: boolean;
}

/** Plan usage windows of the signed-in Claude Code account */
export interface ClaudeLimits {
  plan: string | null;
  tier: string | null;
  windows: { key: string; percent: number; resetsAt: string | null }[];
  extraEnabled: boolean;
  extraUsed: number | null;
  extraLimit: number | null;
  /** "login", "expired" or an error text */
  error: string | null;
  checkedAt: number;
}

/** Codex account limits from `codex app-server` (window length in minutes, reset in Unix seconds) */
export interface CodexLimits {
  plan: string | null;
  groups: { name: string | null; windows: { minutes: number; percent: number; resetsAt: number | null }[] }[];
  /** "missing" (not installed), "login" (not signed in) or an error text */
  error: string | null;
  checkedAt: number;
}

/** Antigravity quota from the desktop app's local server: models sharing one quota form a pool */
export interface AgyLimits {
  plan: string | null;
  /** 5-hour ("5h") and weekly windows per quota group */
  groups: { name: string; windows: { key: string; percent: number; resetsAt: string | null }[] }[];
  pools: { models: string[]; percent: number; resetsAt: string | null }[];
  /** "app_off" when the Antigravity app is not running, or an error text */
  error: string | null;
  checkedAt: number;
}

export type AnswerLang = "auto" | "ko" | "en" | "ja" | "zh";
export type AnswerLength = "auto" | "brief" | "detailed";

export interface SavedPrompt {
  title: string;
  text: string;
}

/** 모든 세션에서 찾은 결과: 세션과 찾은 곳의 앞뒤 글(제목에서 찾았으면 빈 글) */
export interface SearchHit {
  conversationId: string;
  snippet: string;
}

/** 백엔드 settings::INSTRUCTIONS_MAX와 같다 */
export const INSTRUCTIONS_MAX = 2000;

/** 채팅에 붙은 파일(사용자가 올렸거나 에이전트가 답에 붙임) */
export interface Attachment {
  id: string;
  name: string;
  mime: string;
  size: number;
  kind: "image" | "text" | "pdf" | "file";
  origin: "user" | "agent";
}

/** 입력창 추천 */
export interface Suggestion {
  mode: "chat" | "work" | "build" | "council" | null;
  modeConfidence: number | null;
  skill: string | null;
  skillFit: number | null;
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
  /** 고를 수 있는 모델(OpenCode: 무료 모델만) */
  models: string[];
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
  role: "draft" | "review" | "debate" | "rebuttal" | "judge" | "agent" | "synth" | "council" | "plan" | "roadmap" | "finish";
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
  kind: "compare" | "council" | "draft" | "verify" | "triage" | "reviewCheck";
  value: number;
  ms: number;
  costUsd: number;
}

export interface MessageData {
  attachments?: Attachment[];
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
  team?: { agents: { engine: EngineId; model: string | null; ok: boolean; text: string; title?: string; files?: string[] }[]; synth: EngineId | null; work?: boolean; conflicts?: string[]; decisions?: LeadDecision[]; groupLog?: GroupNote[]; leadLog?: LeadNote[]; orgLog?: OrgPost[] };
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

export interface LeadDecision {
  id: string;
  taskId: string;
  key: string;
  command: string;
  why: string;
  /** ran: an agent ran the command itself (no review needed) */
  verdict: "once" | "deny" | "similar" | "all" | "ask_user" | "ran";
  by: "jev" | "lead" | "rule" | "all" | "self" | "worker";
  pattern: string | null;
  window: boolean;
  reason: string;
  at: string;
}

export interface LeadPolicy {
  allowAll: boolean;
  rules: { pattern: string; window: boolean }[];
}

export interface LeadQuestion {
  id: string;
  taskId: string;
  conversationId: string;
  question: string;
  options: string[];
  workers: string[];
}

export interface GroupNote {
  taskId: string;
  group: number;
  round: number;
  key: string;
  who: string;
  text: string;
  claim: string[];
  done: boolean;
}

export interface GroupInfo {
  index: number;
  title: string;
  keys: string[];
}

/** 조직으로 일할 때 하위 작업(팀) 하나: 속한 부서(바깥 → 안쪽)와 보고를 주고받는 팀 */
export interface OrgUnit {
  index: number;
  title: string;
  org: string[];
  keys: string[];
  peers: number[];
}

/** 팀장이 다른 팀장에게 올린 보고 */
export interface OrgPost {
  taskId: string;
  from: number;
  title: string;
  to: number[];
  text: string;
  at: string;
}

export interface TestItem {
  id: number;
  task: string;
  key: string;
  command: string;
  startedAt: number | null;
}

/** 끝난 창 테스트(최근 몇 개) */
export interface TestDone {
  id: number;
  task: string;
  key: string;
  command: string;
  ok: boolean;
  summary: string;
  startedAt: number | null;
  endedAt: number;
}

export interface QueueView {
  capacity: number;
  running: TestItem[];
  waiting: TestItem[];
  recent: TestDone[];
}

/** 총괄이 한 일 한 줄 */
export interface LeadNote {
  taskId: string;
  kind: "review" | "consult" | "merge" | "conflict" | "commit" | "hold" | "work" | "test" | "warn" | "info";
  text: string;
  at: string;
}

/** 총괄이 만든 커밋 */
export interface LeadCommit {
  taskId: string;
  sha: string;
  message: string;
  files: string[];
  note: string;
}

export const api = {
  leadSend: (taskId: string, text: string, about: string | null, direct = false) => invoke<void>("lead_send", { taskId, text, about, direct }),
  leadAnswer: (questionId: string, text: string) => invoke<void>("lead_answer", { questionId, text }),
  leadQuestions: () => invoke<LeadQuestion[]>("lead_questions"),
  leadRevoke: (taskId: string) => invoke<void>("lead_revoke", { taskId }),
  testCancel: (id: number) => invoke<boolean>("test_cancel", { id }),
  queueGet: () => invoke<QueueView>("queue_get"),
  testLog: (id: number) => invoke<string[]>("test_log", { id }),
  bgJobs: (taskId?: string) => invoke<BgJob[]>("bg_jobs", { taskId }),
  bgRead: (id: string) => invoke<string>("bg_read", { id }),
  bgStop: (id: string) => invoke<boolean>("bg_stop", { id }),
  workerWindowOpen: (taskId: string, key: string, title: string) => invoke<void>("worker_window_open", { taskId, key, title }),
  powerSleep: (on: boolean, monitorOff: boolean) => invoke<void>("power_sleep", { on, monitorOff }),
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
  conversationSearch: (query: string) => invoke<SearchHit[]>("conversation_search", { query }),
  /** 창이 앞에 없으면 작업 표시줄 단추를 깜박여 알린다 */
  windowAttention: () => invoke<void>("window_attention"),
  powerCancel: () => invoke<boolean>("power_cancel"),
  limitsClaude: () => invoke<ClaudeLimits>("limits_claude"),
  limitsAntigravity: () => invoke<AgyLimits>("limits_antigravity"),
  limitsCodex: () => invoke<CodexLimits>("limits_codex"),
  chatContinue: (conversationId: string, messageId: string) => invoke<{ conversationId: string; taskId: string; userMessageId: string }>("chat_continue", { conversationId, messageId }),
  tasksRunning: () => invoke<string[]>("tasks_running"),
  /** 화면 전체 배율(0.8~1.4) */
  uiZoom: (scale: number) => invoke<void>("ui_zoom", { scale }),
  conversationDelete: (conversationId: string) => invoke<void>("conversation_delete", { conversationId }),
  conversationSetWorkspace: (conversationId: string, path: string | null) => invoke<void>("conversation_set_workspace", { conversationId, path }),
  pickFolder: () => invoke<string | null>("pick_folder"),
  /** work: 작업 모드(허락을 받은 뒤 파일 수정) · build: '만들기' — 승인 없이 폴더 안에서 끝까지 완성(작업 모드 포함) */
  chatSend: (conversationId: string | null, text: string, engine: EngineChoice, work: boolean, build: boolean, workspace: string | null, council = false, attachments: string[] = [], setup: WorkSetup | null = null) =>
    invoke<{ conversationId: string; taskId: string; userMessageId: string }>("chat_send", { conversationId, text, engine, work: !council && (work || build), build, workspace, council, attachments, setup }),
  /** Model names the work setup panel can offer */
  workModels: () => invoke<string[]>("work_models"),
  attachmentPick: () => invoke<Attachment[]>("attachment_pick"),
  attachmentAddPaths: (paths: string[]) => invoke<Attachment[]>("attachment_add_paths", { paths }),
  attachmentAddBytes: (name: string, dataBase64: string) => invoke<Attachment>("attachment_add_bytes", { name, dataBase64 }),
  attachmentRemove: (id: string) => invoke<void>("attachment_remove", { id }),
  attachmentPreview: (id: string) => invoke<string | null>("attachment_preview", { id }),
  /** 실행되는 형식이면 열지 않고 탐색기에서 보여 주고 true를 돌려준다 */
  attachmentOpen: (id: string) => invoke<boolean>("attachment_open", { id }),
  attachmentReveal: (id: string) => invoke<void>("attachment_reveal", { id }),
  attachmentSaveAs: (id: string) => invoke<string | null>("attachment_save_as", { id }),
  jevSuggest: (text: string, hasFiles: boolean) => invoke<Suggestion | null>("jev_suggest", { text, hasFiles }),
  taskCancel: (taskId: string) => invoke<boolean>("task_cancel", { taskId }),
  /** 실행 중인 에이전트 하나만 멈춘다(같은 작업의 다른 에이전트는 계속) */
  agentCancel: (taskId: string, key: string) => invoke<boolean>("agent_cancel", { taskId, key }),
  /** 에이전트 하나에게 말을 건다 */
  agentMessage: (taskId: string, key: string, text: string) => invoke<void>("agent_message", { taskId, key, text }),
  /** 곁질문: 대화 기록에 남지 않는다. 답은 side:* 이벤트로 온다 */
  sideAsk: (sideId: string, conversationId: string | null, workspace: string | null, question: string, progress: string | null, thread: { role: string; text: string }[]) =>
    invoke<void>("side_ask", { sideId, conversationId, workspace, question, progress, thread }),
  /** 작업 중 메시지: 지금 실행 중인 에이전트 모두에게 전한다 */
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
  remoteConfigure: (enabled: boolean, allowApprove: boolean, relay?: boolean) => invoke<RemoteStatus>("remote_configure", { enabled, allowApprove, relay }),
  remotePairStart: () => invoke<PairInfo>("remote_pair_start"),
  remotePairCancel: () => invoke<void>("remote_pair_cancel"),
  remoteRevoke: (id: string) => invoke<RemoteStatus>("remote_revoke", { id }),
  dataDeleteAll: (confirm: string) => invoke<void>("data_delete_all", { confirm }),
};

/** 원격 제어(폰 앱) 상태. 토큰 값은 PC에도 남기지 않는다(해시만). */
export interface RemoteStatus {
  config: { enabled: boolean; port: number; allowApprove: boolean; relay: boolean };
  running: number | null;
  addresses: { ip: string; kind: "lan" | "tailscale"; name: string }[];
  fingerprint: string | null;
  devices: { id: string; name: string; createdAt: string; lastSeenAt: string | null }[];
  /** 다른 네트워크 연결(iroh 터널) 상태. 꺼져 있으면 null */
  relay: "starting" | "online" | "error" | null;
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

export function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function toBase64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let out = "";
  // 한 번에 넘기면 인자 수 한도를 넘는다.
  for (let i = 0; i < bytes.length; i += 0x8000) out += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(out);
}

/** 답이 흐르는 동안 파일 표시를 숨긴다(끝나면 백엔드가 첨부로 바꾼다). 닫히지 않은 블록은 끝까지 숨긴다. */
export function stripFileMarkers(text: string): string {
  return text
    .replace(/^[ \t]*\[\[file:[^\]\n]*\]\][\s\S]*?(^[ \t]*\[\[\/file\]\][ \t]*$|(?![\s\S]))/gm, "")
    .replace(/^[ \t]*\[\[attach:[^\]\n]*\]\][ \t]*$/gm, "")
    .trimEnd();
}

/** 경로의 마지막 폴더 이름 */
export function baseName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** 에이전트가 뒤에서 돌리는 명령 */
export interface BgJob {
  id: string;
  taskId: string;
  /** 이 명령을 돌린 실행 key */
  key: string;
  command: string;
  /** claude: Claude Code의 백그라운드 셸, elysium: Elysium이 워커 대신 실행 */
  source: "claude" | "elysium";
  state: "run" | "ok" | "fail" | "stopped";
  startedAt: string;
  endedAt: string | null;
  stoppable: boolean;
}

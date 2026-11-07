import { LT, SlotsPicker } from "./LeadPanel";
import { useEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { API_SECRET, INSTRUCTIONS_MAX, api, errorText, type AnswerLang, type AnswerLength, type ApiId, type AppInfo, type EngineInfo, type Judgment, type ReviewMode, type SavedPrompt, type SecretStatus, type Settings } from "../lib/api";
import { applyScale, applyWidth, loadScale, loadWidth, SCALES, type ChatWidth, type UiScale } from "../lib/view";
import { useApp } from "../store";
import { CloseIcon, FolderIcon, RefreshIcon } from "./Icons";
import Segmented from "./Segmented";
import { loadAutoSleep, loadMonitorOff, saveAutoSleep, saveMonitorOff, setSleep } from "../lib/sleep";
import { applyLowSpec, loadLowSpec } from "../lib/perf";
import { POWER_TEXT } from "./PowerBanner";

const PERF_TEXT = {
  title: "저사양 모드",
  help: "배경 애니메이션과 블러 등 그래픽 효과를 최소화하여 화면 끊김을 줄입니다. 이 설정은 현재 PC에만 저장됩니다.",
};

const SLEEP = {
  title: "취침 모드",
  help: "화면을 꺼 전력을 절약하며 AI 작업은 최고 속도로 중단 없이 계속 실행됩니다. 마우스를 클릭하거나 아무 키나 누르면 언제든 원래 화면으로 돌아옵니다.",
  auto: "자동 취침 전환",
  off: "끔",
  minutes: "{n}분",
  monitor: "취침 시 모니터 끄기",
  now: "지금 시작",
};
const AUTO_CHOICES = [0, 10, 30, 60];

/** 취침 모드: 지금 켜기, 자동 전환, 모니터 끄기 */
function SleepSettings() {
  const [auto, setAuto] = useState(loadAutoSleep);
  const [monitor, setMonitor] = useState(loadMonitorOff);
  return (
    <>
      <div className="sleep-row">
        <span>{SLEEP.auto}</span>
        <Segmented
          label={SLEEP.auto}
          value={String(auto)}
          options={AUTO_CHOICES.map((m) => ({ value: String(m), label: m ? SLEEP.minutes.replace("{n}", String(m)) : SLEEP.off }))}
          onChange={(v) => {
            setAuto(Number(v));
            saveAutoSleep(Number(v));
          }}
        />
      </div>
      <label className="sleep-row check">
        <input
          type="checkbox"
          checked={monitor}
          onChange={(e) => {
            setMonitor(e.target.checked);
            saveMonitorOff(e.target.checked);
          }}
        />
        <span>{SLEEP.monitor}</span>
        <div className="spacer" />
        <button className="btn sm" onClick={() => setSleep(true)}>
          {SLEEP.now}
        </button>
      </label>
      <p className="help">{SLEEP.help}</p>
    </>
  );
}
import RemoteSection from "./RemoteSection";

const REVIEW: { id: ReviewMode; label: string; help: string }[] = [
  { id: "auto", label: "자동", help: "어려운 요청은 다른 AI가 검토하고, 정확도가 중요한 요청은 두 AI가 토론한 뒤 판정 AI가 정리합니다(JEV를 켜면 JEV가 판단)" },
  { id: "always", label: "항상", help: "모든 대화 답을 한 번 더 검토합니다(느리고 호출이 두 배)" },
  { id: "debate", label: "토론", help: "모든 대화 요청을 서로 다른 회사의 두 AI가 따로 답하고 반론한 뒤 판정 AI가 정리합니다(가장 정확하지만 호출이 4~5배)" },
  { id: "off", label: "끄기", help: "첫 답을 바로 보여 줍니다" },
];

const TASK_LABEL: Record<string, string> = { code: "코드", writing: "글쓰기", research: "조사", analysis: "분석", chat: "대화", translate: "번역" };

function modelPriceHint(engine: string, model: string): string {
  const m = model.toLowerCase();
  if (engine === "claude-api" || engine === "claude") {
    if (m.includes("haiku")) return "입력 $0.80 / 출력 $4.00 (1M 토큰)";
    if (m.includes("opus")) return "입력 $15.00 / 출력 $75.00 (1M 토큰)";
    return "입력 $3.00 / 출력 $15.00 (1M 토큰)";
  }
  if (engine === "openai" || engine === "codex") {
    if (m.includes("mini") || m.includes("oss")) return "입력 $0.15 / 출력 $0.60 (1M 토큰)";
    if (m.includes("o1")) return "입력 $15.00 / 출력 $60.00 (1M 토큰)";
    if (m.includes("o3")) return "입력 $1.10 / 출력 $4.40 (1M 토큰)";
    return "입력 $2.50 / 출력 $10.00 (1M 토큰)";
  }
  if (engine === "gemini-api" || engine === "antigravity") {
    if (m.includes("pro") || m.includes("argon")) return "입력 $1.25 / 출력 $5.00 (1M 토큰)";
    return "입력 $0.10 / 출력 $0.40 (1M 토큰)";
  }
  if (engine === "nvidia") {
    return "NVIDIA Build 체험 무료 ($0.00)";
  }
  return "";
}

const KEYS: [string, string][] = [
  ["Ctrl + N", "새 세션"],
  ["Ctrl + B", "세션 목록 펼치기·접기"],
  ["Ctrl + J", "도구 패널 열기·닫기"],
  ["Ctrl + /", "단축키 보기"],
  ["Ctrl + ,", "설정"],
  ["Enter", "보내기(대화 모드: 질문 · 작업 모드: 허락 후 파일 수정)"],
  ["← / →", "대화·작업 모드 바꾸기(모드 스위치에서)"],
  ["Ctrl + Enter", "만들기(폴더 안에서 끝까지 완성)"],
  ["Ctrl + Shift + Enter", "회의"],
  ["Shift + Enter", "줄 바꿈"],
  ["/", "스킬 고르기(입력창 맨 앞)"],
  ["Esc", "창·패널 닫기"],
  ["F2", "세션 이름 바꾸기(세션 목록에서, 두 번 눌러도 됨)"],
];

function Section({ id, title, i, children, extra, sectionRef }: { id: string; title: string; i: number; children: React.ReactNode; extra?: React.ReactNode; sectionRef?: RefObject<HTMLElement | null> }) {
  return (
    <section id={`set-${id}`} ref={sectionRef} style={{ "--i": i } as CSSProperties}>
      <div className="sec-head">
        <h3>{title}</h3>
        {extra}
      </div>
      {children}
    </section>
  );
}

const LANGS: { value: AnswerLang; label: string }[] = [
  { value: "auto", label: "자동" },
  { value: "ko", label: "한국어" },
  { value: "en", label: "English" },
  { value: "ja", label: "日本語" },
  { value: "zh", label: "中文" },
];
const LENGTHS: { value: AnswerLength; label: string; help: string }[] = [
  { value: "auto", label: "자동", help: "요청에 맞춰 AI가 길이를 정합니다." },
  { value: "brief", label: "짧게", help: "결론부터 짧게 답합니다. 코드·숫자·경고는 줄이지 않습니다." },
  { value: "detailed", label: "자세히", help: "배경, 이유, 예시, 주의할 점까지 설명합니다." },
];
const SCALE_LABEL: Record<UiScale, string> = { 0.9: "작게", 1: "보통", 1.1: "크게", 1.2: "더 크게" };

/** 처음 쓰는 사람을 위한 프롬프트 예시 */
const PROMPT_EXAMPLES: SavedPrompt[] = [
  { title: "코드 리뷰", text: "아래 코드를 리뷰해 줘. 버그, 보안 문제, 더 단순하게 쓸 수 있는 곳을 중요한 것부터 알려 주고, 고친 코드도 보여 줘." },
  { title: "핵심 요약", text: "아래 내용을 한 문장 결론과 핵심 3~5개로 요약해 줘. 숫자와 이름은 원문 그대로 둬." },
  { title: "자연스러운 번역", text: "아래 글을 자연스러운 영어로 번역해 줘. 전문 용어는 그대로 두고, 어색한 표현이 있으면 따로 알려 줘." },
  { title: "업무 메일 초안", text: "아래 내용으로 정중하고 간결한 업무 메일을 써 줘. 제목, 본문, 마무리 인사를 포함해 줘." },
  { title: "오류 원인 찾기", text: "아래 오류 메시지와 상황을 보고 가능한 원인을 가능성 높은 순서로 알려 주고, 하나씩 확인하는 방법을 알려 줘." },
];

/** 답변 방식: 언어·길이·모든 AI에게 미리 알려 둘 말 */
function AnswerSettings({ settings, save }: { settings: Settings; save: (s: Settings) => void }) {
  const [text, setText] = useState(settings.instructions ?? "");
  useEffect(() => setText(settings.instructions ?? ""), [settings.instructions]);
  const dirty = text.trim() !== (settings.instructions ?? "").trim();
  const length = settings.answerLength ?? "auto";
  return (
    <>
      <div className="set-row">
        <span>답변 언어</span>
        <Segmented label="답변 언어" className="dense" value={settings.answerLang ?? "auto"} options={LANGS} onChange={(v) => save({ ...settings, answerLang: v })} />
      </div>
      <div className="set-row">
        <span>답변 길이</span>
        <Segmented label="답변 길이" className="dense" value={length} options={LENGTHS.map(({ value, label }) => ({ value, label }))} onChange={(v) => save({ ...settings, answerLength: v })} />
      </div>
      <p className="help">{LENGTHS.find((l) => l.value === length)?.help} 자동 언어는 한국어로 답하되, 다른 언어로 물으면 그 언어로 답합니다.</p>
      <label className="set-field">
        <span>
          모든 AI에게 미리 알려 둘 것 <small>{text.length.toLocaleString()} / {INSTRUCTIONS_MAX.toLocaleString()}자</small>
        </span>
        <textarea
          rows={4}
          value={text}
          maxLength={INSTRUCTIONS_MAX}
          placeholder={"예: 저는 React와 Rust를 쓰는 개발자입니다. 코드는 TypeScript로, 설명은 짧게 해 주세요. 회사 이름은 바깥으로 나가는 글에 쓰지 마세요."}
          onChange={(e) => setText(e.target.value)}
          onBlur={() => dirty && save({ ...settings, instructions: text })}
        />
      </label>
      <div className="row">
        <p className="help grow">모든 요청의 지시 끝에 붙습니다. 매번 토큰을 쓰므로 꼭 필요한 것만 적으세요. 이번 요청과 어긋나면 이번 요청이 우선합니다.</p>
        <button className="btn sm" disabled={!dirty} onClick={() => save({ ...settings, instructions: text })}>
          저장
        </button>
      </div>
    </>
  );
}

/** 자주 쓰는 프롬프트: 입력창의 책 단추에서 골라 넣는다 */
function PromptSettings({ settings, save }: { settings: Settings; save: (s: Settings) => void }) {
  const prompts = settings.prompts ?? [];
  const [list, setList] = useState<SavedPrompt[]>(prompts);
  // 저장하면 빈 줄은 빠지므로, 아직 내용을 쓰는 중인 빈 줄은 남겨 둔다.
  useEffect(() => setList((cur) => [...(settings.prompts ?? []), ...cur.filter((p) => !p.text.trim())]), [settings.prompts]);
  const commit = (next: SavedPrompt[]) => save({ ...settings, prompts: next.filter((p) => p.text.trim()) });
  const change = (i: number, p: Partial<SavedPrompt>) => setList((cur) => cur.map((x, j) => (j === i ? { ...x, ...p } : x)));
  const missing = PROMPT_EXAMPLES.filter((e) => !prompts.some((p) => p.text === e.text));
  return (
    <>
      {list.length === 0 && <p className="help">저장한 프롬프트가 없습니다. 아래 예시를 넣거나 새로 만드세요. 입력창에서 쓴 글을 바로 저장할 수도 있습니다.</p>}
      <ul className="prompt-list">
        {list.map((p, i) => (
          <li key={i}>
            <input type="text" value={p.title} maxLength={60} placeholder="이름" aria-label="프롬프트 이름" onChange={(e) => change(i, { title: e.target.value })} onBlur={() => commit(list)} />
            <textarea rows={2} value={p.text} placeholder="내용" aria-label="프롬프트 내용" onChange={(e) => change(i, { text: e.target.value })} onBlur={() => commit(list)} />
            <button type="button" className="icon" aria-label={`${p.title || "프롬프트"} 지우기`} title="지우기" onClick={() => commit(list.filter((_, j) => j !== i))}>
              <CloseIcon />
            </button>
          </li>
        ))}
      </ul>
      <div className="row">
        <button className="btn sm" onClick={() => setList((cur) => [...cur, { title: "", text: "" }])}>
          새 프롬프트
        </button>
        {missing.length > 0 && (
          <button className="btn sm ghost" onClick={() => commit([...prompts, ...missing])}>
            예시 {missing.length}개 넣기
          </button>
        )}
      </div>
    </>
  );
}

/** 화면·알림: 글자 크기, 대화 폭, 작업 끝 알림 */
function ViewSettings({ settings, save }: { settings: Settings; save: (s: Settings) => void }) {
  const [scale, setScale] = useState<UiScale>(loadScale);
  const [width, setWidth] = useState<ChatWidth>(loadWidth);
  const [low, setLow] = useState(loadLowSpec);
  return (
    <>
      <div className="set-row">
        <span>글자·화면 크기</span>
        <Segmented
          label="글자·화면 크기"
          className="dense"
          value={String(scale)}
          options={SCALES.map((v) => ({ value: String(v), label: SCALE_LABEL[v] }))}
          onChange={(v) => {
            const n = Number(v) as UiScale;
            setScale(n);
            applyScale(n);
          }}
        />
      </div>
      <div className="set-row">
        <span>대화 폭</span>
        <Segmented
          label="대화 폭"
          className="dense"
          value={width}
          options={[
            { value: "normal", label: "보통" },
            { value: "wide", label: "넓게" },
            { value: "full", label: "꽉 차게" },
          ]}
          onChange={(v) => {
            setWidth(v);
            applyWidth(v);
          }}
        />
      </div>
      <p className="help">크기와 폭은 이 PC에만 기억합니다. 넓게는 표와 코드가 많은 답을 볼 때 좋습니다.</p>
      <label className="toggle-row">
        <span>
          {PERF_TEXT.title}
          <small>{PERF_TEXT.help}</small>
        </span>
        <input
          type="checkbox"
          className="switch"
          role="switch"
          checked={low}
          onChange={(e) => {
            setLow(e.target.checked);
            applyLowSpec(e.target.checked);
          }}
        />
      </label>
      <label className="toggle-row">
        <span>
          작업이 끝나면 알리기
          <small>다른 창을 보고 있을 때 답이 끝나면 작업 표시줄의 Elysium 단추가 깜박입니다.</small>
        </span>
        <input type="checkbox" className="switch" role="switch" checked={settings.notifyDone ?? true} onChange={(e) => save({ ...settings, notifyDone: e.target.checked })} />
      </label>
    </>
  );
}

/** 설정 묶음: 한 번에 한 묶음만 보여 준다 */
type GroupId = "general" | "ai" | "work" | "etc";
const GROUPS: { id: GroupId; label: string; sections: string[] }[] = [
  { id: "general", label: "일반", sections: ["answer", "prompts", "view", "folder"] },
  { id: "ai", label: "AI 연결", sections: ["cli", "api", "jev"] },
  { id: "work", label: "작업 방식", sections: ["review", "queue"] },
  { id: "etc", label: "기타", sections: ["remote", "sleep", "keys", "data"] },
];

/** API 키 한 줄: 키 저장·지우기·확인, 모델 이름 */
function ApiRow({ e, status, onEngines }: { e: EngineInfo; status?: SecretStatus; onEngines: (list: EngineInfo[]) => void }) {
  const { settings, saveSettings, toast } = useApp();
  const [key, setKey] = useState("");
  const [model, setModel] = useState(e.version ?? "");
  const [check, setCheck] = useState<{ ok: boolean; text: string } | null>(null);
  const secret = API_SECRET[e.id as ApiId];
  useEffect(() => setModel(e.version ?? ""), [e.version]);

  async function save() {
    try {
      onEngines(await api.secretSet(secret, key));
      setKey("");
      setCheck(null);
      toast(`${e.label} 키를 Windows 자격 증명 관리자에 저장했습니다.`);
    } catch (err) {
      toast(errorText(err), "error");
    }
  }
  async function clear() {
    try {
      onEngines(await api.secretClear(secret));
      setCheck(null);
    } catch (err) {
      toast(errorText(err), "error");
    }
  }
  async function test() {
    setCheck({ ok: true, text: "확인 중…" });
    try {
      setCheck({ ok: true, text: await api.apiTest(e.id as ApiId) });
    } catch (err) {
      setCheck({ ok: false, text: errorText(err) });
    }
  }
  function saveModel() {
    if (!settings || model.trim() === (e.version ?? "")) return;
    saveSettings({ ...settings, models: { ...settings.models, [e.id]: model.trim() } });
  }

  const priceHint = modelPriceHint(e.id, model);

  return (
    <div className={`api-row ${e.installed ? "on" : ""}`}>
      <div className="api-top">
        <i className="dot" />
        <b>{e.label}</b>
        {e.installed && <span className="tag">{status?.source === "env" ? "환경 변수" : "키 저장됨"}</span>}
        <span className="api-hint">{e.installHint}</span>
      </div>
      <div className="row">
        <input type="password" value={key} onChange={(ev) => setKey(ev.target.value)} placeholder={e.installed ? "새 키로 바꾸려면 입력" : "API 키"} autoComplete="off" spellCheck={false} aria-label={`${e.label} 키`} />
        <button className="btn sm" disabled={!key.trim()} onClick={save}>
          저장
        </button>
        {e.installed && status?.source === "keyring" && (
          <button className="btn sm ghost" onClick={clear}>
            지우기
          </button>
        )}
      </div>
      <div className="row">
        <input type="text" value={model} onChange={(ev) => setModel(ev.target.value)} onBlur={saveModel} onKeyDown={(ev) => ev.key === "Enter" && saveModel()} aria-label={`${e.label} 모델`} spellCheck={false} className="model-input" />
        <button className="btn sm" disabled={!e.installed} onClick={test} title="1토큰 요청으로 키와 모델을 확인합니다">
          확인
        </button>
      </div>
      {priceHint && <div className="api-price-hint">단가: {priceHint}</div>}
      {check && <div className={`check ${check.ok ? "ok" : "err"}`}>{check.text}</div>}
    </div>
  );
}

/** 설정: 연결된 AI(CLI·API 키), JEV, 기본 작업 폴더, 검토, 단축키, 데이터. focusKeys면 단축키 칸으로 옮긴다. */
export default function SettingsDialog({ focusKeys = false }: { focusKeys?: boolean }) {
  const { settings, saveSettings, engines, setEngines, refreshEngines, setDialog, toast } = useApp();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [usage, setUsage] = useState<{ calls: number; costUsd: number } | null>(null);
  const [secrets, setSecrets] = useState<SecretStatus[]>([]);
  const [confirmText, setConfirmText] = useState("");
  const [scanning, setScanning] = useState(false);
  const [tsKey, setTsKey] = useState("");
  const [jev, setJev] = useState<{ ok: boolean; text: string } | null>(null);
  const keysRef = useRef<HTMLElement>(null);
  const [group, setGroup] = useState<GroupId>(focusKeys ? "etc" : "general");
  const shown = GROUPS.find((g) => g.id === group)!.sections;
  useEffect(() => {
    if (focusKeys) {
      setGroup("etc");
      requestAnimationFrame(() => keysRef.current?.scrollIntoView({ block: "start" }));
    }
  }, [focusKeys]);

  const loadSecrets = () => api.secretsStatus().then(setSecrets).catch(() => {});
  useEffect(() => {
    api.appInfo().then(setInfo).catch(() => {});
    api.usageToday().then(setUsage).catch(() => {});
    loadSecrets();
  }, [settings]);

  if (!settings) return null;
  const ts = secrets.find((x) => x.name === "typesafe");

  async function rescan() {
    setScanning(true);
    await refreshEngines();
    setScanning(false);
  }
  async function pickDefault() {
    const p = await api.pickFolder().catch(() => null);
    if (p) await saveSettings({ ...settings!, workspaceDir: p });
  }
  async function saveTs() {
    try {
      await api.secretSet("typesafe", tsKey);
      setTsKey("");
      loadSecrets();
      toast("TypeSafe 키를 저장했습니다.");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }
  async function testJev() {
    setJev({ ok: true, text: "판단 중…" });
    try {
      const j: Judgment = await api.jevTest();
      setJev({
        ok: true,
        text: `${j.model}: ${j.task ? (TASK_LABEL[j.task] ?? j.task) : "?"} (${Math.round((j.taskConfidence ?? 0) * 100)}%) · 난이도 ${j.difficulty?.toFixed(1) ?? "?"}/3 · ${j.ms}ms`,
      });
    } catch (e) {
      setJev({ ok: false, text: errorText(e) });
    }
  }
  async function wipe() {
    try {
      await api.dataDeleteAll(confirmText);
      setConfirmText("");
      toast("대화와 기록을 모두 지웠습니다.");
    } catch (e) {
      toast(errorText(e), "error");
    }
  }

  const clis = engines.filter((e) => e.kind === "cli");
  const apis = engines.filter((e) => e.kind === "api");
  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
      <div className="modal wide settings-modal" role="dialog" aria-label="설정">
        <div className="set-top">
        <div className="modal-head">
          <h2>설정</h2>
          <div className="row" style={{ gap: 8 }}>
            <button className="btn sm ghost" onClick={() => setDialog("usage")}>
              토큰 단가표 & 사용량 보기
            </button>
            <button className="icon" onClick={() => setDialog(null)} aria-label="닫기">
              <CloseIcon />
            </button>
          </div>
        </div>
        <nav className="set-groups" role="tablist" aria-label="설정 묶음">
          {GROUPS.map((g) => (
            <button
              key={g.id}
              type="button"
              role="tab"
              aria-selected={group === g.id}
              className={group === g.id ? "on" : ""}
              onClick={() => {
                setGroup(g.id);
                document.querySelector(".settings-modal .modal-scroll")?.scrollTo({ top: 0 });
              }}
            >
              {g.label}
            </button>
          ))}
        </nav>
        </div>
        <div className="modal-scroll">

        {shown.includes("answer") && (
        <Section id="answer" title="답변 방식" i={0}>
          <AnswerSettings settings={settings} save={saveSettings} />
        </Section>
        )}

        {shown.includes("prompts") && (
        <Section id="prompts" title="자주 쓰는 프롬프트" i={1}>
          <PromptSettings settings={settings} save={saveSettings} />
        </Section>
        )}

        {shown.includes("view") && (
        <Section id="view" title="화면 · 알림" i={2}>
          <ViewSettings settings={settings} save={saveSettings} />
        </Section>
        )}

        {shown.includes("cli") && (
        <Section
          id="cli"
          title="CLI"
          i={3}
          extra={
            <button className="btn sm" onClick={rescan} disabled={scanning}>
              <RefreshIcon /> {scanning ? "찾는 중…" : "다시 찾기"}
            </button>
          }
        >
          <div className="engines">
            {clis.map((e) => (
              <div key={e.id} className={`engine ${e.installed ? "on" : ""}`}>
                <i className="dot" />
                <div>
                  <b>{e.label}</b>
                  {e.installed && !e.verified && <em className="tag">미검증</em>}
                  <span>{e.installed ? `${e.version ?? "설치됨"} · ${e.path}` : e.installHint}</span>
                  <small className="engine-price-sub">
                    {e.id === "claude" && "Sonnet $3/$15 · Haiku $0.80/$4 · Opus $15/$75 (1M 토큰)"}
                    {e.id === "codex" && "GPT-5/4o $2.50/$10 · Mini $0.15/$0.60 · o1 $15/$60 (1M 토큰)"}
                    {e.id === "antigravity" && "Flash $0.10/$0.40 · Argon/Pro $1.25/$5.00 · Claude $3/$15 (1M 토큰)"}
                    {e.id === "opencode" && "무료 모델 전용 ($0)"}
                  </small>
                </div>
              </div>
            ))}
          </div>
          {clis.some((e) => e.id === "opencode" && e.installed) && (
            <div className="engine-option">
              <label className="engine-option-row">
                <span>{"OpenCode 무료 모델 선택"}</span>
                <select
                  className="select"
                  value={settings.models.opencode ?? ""}
                  onChange={(ev) => {
                    const { opencode: _, ...rest } = settings.models;
                    saveSettings({ ...settings, models: ev.target.value ? { ...rest, opencode: ev.target.value } : rest });
                  }}
                >
                  <option value="">{"자동 (JEV 선택)"}</option>
                  {clis
                    .find((e) => e.id === "opencode")
                    ?.models.map((m) => (
                      <option key={m} value={m}>
                        {m.replace("opencode/", "")}
                      </option>
                    ))}
                </select>
              </label>
              <p className="help">{"요청은 OpenCode Zen으로 전송되며, DeepSeek 및 GLM 외 일부 무료 모델은 프롬프트를 보관하거나 모델 학습에 활용할 수 있습니다. 민감한 정보가 감지되면 프롬프트를 저장하지 않는 DeepSeek과 GLM만 사용됩니다."}</p>
            </div>
          )}
          <p className="help">CLI는 파일을 읽고(만들기에서는 만들고 고치고) 세션을 이어 씁니다. 로그인은 각 CLI에서 합니다.</p>
        </Section>
        )}

        {shown.includes("api") && (
        <Section id="api" title="API 키 & 모델 단가" i={4}>
          <div className="api-grid">
            {apis.map((e) => (
              <ApiRow key={e.id} e={e} status={secrets.find((s) => s.name === API_SECRET[e.id as ApiId])} onEngines={(l) => (setEngines(l), loadSecrets())} />
            ))}
          </div>
          <p className="help">키는 Windows 자격 증명 관리자에만 안전하게 저장됩니다. 공식 API 단가에 따라 토큰 비용이 투명하게 계산됩니다.</p>
        </Section>
        )}

        {shown.includes("jev") && (
        <Section id="jev" title="JEV" i={5}>
          <label className="toggle-row">
            <span>
              JEV로 판단하기
              <small>{"JEV가 요청마다 스킬, AI와 모델, 에이전트 분담을 정하고 불필요한 검토와 기억을 생략해 토큰을 아낍니다. (1M 입력 토큰당 $0.042)"}</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={settings.jevEnabled} onChange={(e) => saveSettings({ ...settings, jevEnabled: e.target.checked })} />
          </label>
          <label className="toggle-row">
            <span>
              {"JEV 모드 및 스킬 추천"}
              <small>{"메시지를 입력하는 동안 적합한 모드와 스킬을 실시간으로 추천합니다. 이를 위해 아직 전송되지 않은 입력 텍스트가 TypeSafe로 전송됩니다."}</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={settings.jevSuggest} disabled={!settings.jevEnabled} onChange={(e) => saveSettings({ ...settings, jevSuggest: e.target.checked })} />
          </label>
          <div className="row">
            <span className={`tag ${ts?.present ? "ok" : ""}`}>{ts?.present ? (ts.source === "env" ? "TypeSafe 키: 환경 변수" : "TypeSafe 키 저장됨") : "TypeSafe 키 없음"}</span>
            <input type="password" value={tsKey} onChange={(e) => setTsKey(e.target.value)} placeholder="TypeSafe API 키" autoComplete="off" aria-label="TypeSafe 키" />
            <button className="btn sm" disabled={!tsKey.trim()} onClick={saveTs}>
              저장
            </button>
            <button className="btn sm" disabled={!ts?.present} onClick={testJev} title="예시 요청 하나를 판단해 봅니다">
              판단 시험
            </button>
          </div>
          {jev && <div className={`check ${jev.ok ? "ok" : "err"}`}>{jev.text}</div>}
        </Section>
        )}







        {shown.includes("review") && (
        <Section id="review" title="검토" i={6}>
          <Segmented label="검토" className="wide" value={settings.review} options={REVIEW.map((r) => ({ value: r.id, label: r.label }))} onChange={(v) => saveSettings({ ...settings, review: v })} />
          <p className="help">{REVIEW.find((r) => r.id === settings.review)?.help}</p>
        </Section>
        )}
        {shown.includes("queue") && (
        <Section id="queue" title={LT.queue} i={8}>
          <SlotsPicker />
          <p className="help">{LT.slotsHelp}</p>
          <label className="toggle-row">
            <span>
              {"작업 결과 검토 및 선별 커밋"}
              <small>{"Git 저장소에서만 동작하며, 워커는 프로젝트의 .elysium/worktrees 폴더에서 작업합니다. 워커가 작업을 마칠 때마다 총괄 AI가 변경 사항을 검토·보완한 뒤, 다른 미커밋 변경 사항은 그대로 두고 해당 파일만 안전하게 커밋합니다."}</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={settings.leadCommit ?? true} onChange={(e) => saveSettings({ ...settings, leadCommit: e.target.checked })} />
          </label>
          <label className="toggle-row">
            <span>
              {POWER_TEXT.toggle}
              <small>{POWER_TEXT.help}</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={settings.powerOnDone ?? false} onChange={(e) => saveSettings({ ...settings, powerOnDone: e.target.checked })} />
          </label>
        </Section>
        )}
        {shown.includes("folder") && (
        <Section id="folder" title="기본 작업 폴더" i={9}>
          <div className="row">
            <div className="path">{info?.workspace}</div>
            <button className="btn sm" onClick={pickDefault}>
              <FolderIcon /> 고르기
            </button>
            <button className="btn sm ghost" onClick={() => api.openPath("workspace").catch((e) => toast(errorText(e), "error"))}>
              열기
            </button>
          </div>
          <p className="help">새 세션은 이 폴더에서 시작합니다. 세션마다 다른 폴더는 입력창의 폴더 칩이나 세션 머리에서 고르세요.</p>
        </Section>
        )}
        {shown.includes("remote") && (
        <Section id="remote" title="폰 연결" i={10}>
          <RemoteSection />
        </Section>
        )}
        {shown.includes("sleep") && (
        <Section id="sleep" title={SLEEP.title} i={11}>
          <SleepSettings />
        </Section>
        )}
        {shown.includes("keys") && (
        <Section id="keys" title="단축키" i={12} sectionRef={keysRef}>
          <dl className="tool-keys">
            {KEYS.map(([k, v]) => (
              <div key={k}>
                <dt>
                  {k.split(" + ").map((p, i) => (
                    <span key={p}>
                      {i > 0 && " + "}
                      <kbd>{p}</kbd>
                    </span>
                  ))}
                </dt>
                <dd>{v}</dd>
              </div>
            ))}
          </dl>
        </Section>
        )}

        {shown.includes("data") && (
        <Section id="data" title="데이터" i={13}>
          <div className="row">
            <input type="text" value={confirmText} onChange={(e) => setConfirmText(e.target.value)} placeholder="DELETE 입력" style={{ maxWidth: 160 }} />
            <button className="btn danger sm" disabled={confirmText !== "DELETE"} onClick={wipe}>
              모든 세션 삭제
            </button>
          </div>
        </Section>
        )}

        <div className="modal-foot">
          <span>Elysium v{info?.version}</span>
          {usage && (
            <span title="CLI·API가 알려 준 추정 비용입니다. 구독 요금제에서는 실제 청구액이 아닙니다.">
              {" "}
              · 오늘 {usage.calls}회 호출 · 추정 ${usage.costUsd.toFixed(2)}
            </span>
          )}
          <button className="btn sm ghost" style={{ marginLeft: "auto" }} onClick={() => setDialog("usage")}>
            전체 토큰 비용 확인하기 →
          </button>
        </div>
        </div>
      </div>
    </div>
  );
}

import { LT, SlotsPicker } from "./LeadPanel";
import { useEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { API_SECRET, api, errorText, type ApiId, type AppInfo, type EngineInfo, type Judgment, type ReviewMode, type SecretStatus } from "../lib/api";
import { useApp } from "../store";
import { CloseIcon, FolderIcon, RefreshIcon } from "./Icons";
import Segmented from "./Segmented";
import RemoteSection from "./RemoteSection";
import { A as AGENT_TEXT, agentChoice } from "./Tools";

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
  ["Ctrl + F", "세션에서 찾기"],
  ["Ctrl + /", "단축키 보기"],
  ["Ctrl + ,", "설정"],
  ["Enter", "보내기(대화 모드: 질문 · 작업 모드: 허락 후 파일 수정)"],
  ["← / →", "대화·작업 모드 바꾸기(모드 스위치에서)"],
  ["Ctrl + Enter", "만들기(폴더 안에서 끝까지 완성)"],
  ["Ctrl + Shift + Enter", "회의"],
  ["Shift + Enter", "줄 바꿈"],
  ["/", "스킬 고르기(입력창 맨 앞)"],
  ["Esc", "창·패널 닫기"],
];

function Section({ title, i, children, extra, sectionRef }: { title: string; i: number; children: React.ReactNode; extra?: React.ReactNode; sectionRef?: RefObject<HTMLElement | null> }) {
  return (
    <section ref={sectionRef} style={{ "--i": i } as CSSProperties}>
      <div className="sec-head">
        <h3>{title}</h3>
        {extra}
      </div>
      {children}
    </section>
  );
}

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
  useEffect(() => {
    if (focusKeys) keysRef.current?.scrollIntoView({ block: "start" });
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
      <div className="modal wide" role="dialog" aria-label="설정">
        <div className="modal-scroll">
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

        <Section
          title="CLI"
          i={0}
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

        <Section title="API 키 & 모델 단가" i={1}>
          <div className="api-grid">
            {apis.map((e) => (
              <ApiRow key={e.id} e={e} status={secrets.find((s) => s.name === API_SECRET[e.id as ApiId])} onEngines={(l) => (setEngines(l), loadSecrets())} />
            ))}
          </div>
          <p className="help">키는 Windows 자격 증명 관리자에만 안전하게 저장됩니다. 공식 API 단가에 따라 토큰 비용이 투명하게 계산됩니다.</p>
        </Section>

        <Section title="JEV" i={2}>
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

        <Section title="폰 연결" i={3}>
          <RemoteSection />
        </Section>

        <Section title="기본 작업 폴더" i={3}>
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

        <Section title="검토" i={4}>
          <Segmented label="검토" className="wide" value={settings.review} options={REVIEW.map((r) => ({ value: r.id, label: r.label }))} onChange={(v) => saveSettings({ ...settings, review: v })} />
          <p className="help">{REVIEW.find((r) => r.id === settings.review)?.help}</p>
        </Section>

        <Section title={LT.queue} i={4}>
          <SlotsPicker />
          <p className="help">{LT.slotsHelp}</p>
        </Section>

        <Section title={AGENT_TEXT.preset} i={4}>
          <Segmented
            label={AGENT_TEXT.preset}
            className="wide dense"
            value={agentChoice.value(settings)}
            options={agentChoice.options}
            onChange={(v) => saveSettings(agentChoice.apply(settings, v))}
          />
          <p className="help">{agentChoice.help(settings)}</p>
        </Section>

        <Section title="단축키" i={5} sectionRef={keysRef}>
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

        <Section title="데이터" i={6}>
          <div className="row">
            <input type="text" value={confirmText} onChange={(e) => setConfirmText(e.target.value)} placeholder="DELETE 입력" style={{ maxWidth: 160 }} />
            <button className="btn danger sm" disabled={confirmText !== "DELETE"} onClick={wipe}>
              모든 세션 삭제
            </button>
          </div>
        </Section>

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

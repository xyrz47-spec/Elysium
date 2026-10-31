import { useEffect, useState, type CSSProperties } from "react";
import { API_SECRET, api, errorText, type ApiId, type AppInfo, type EngineInfo, type Judgment, type ReviewMode, type SecretStatus } from "../lib/api";
import { useApp } from "../store";
import { CloseIcon, FolderIcon, RefreshIcon } from "./Icons";
import Segmented from "./Segmented";
import RemoteSection from "./RemoteSection";
import { A as AGENT_TEXT } from "./Tools";

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

function Section({ title, i, children, extra }: { title: string; i: number; children: React.ReactNode; extra?: React.ReactNode }) {
  return (
    <section style={{ "--i": i } as CSSProperties}>
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

/** 설정: 연결된 AI(CLI·API 키), JEV, 기본 작업 폴더, 검토, 데이터. */
export default function SettingsDialog() {
  const { settings, saveSettings, engines, setEngines, refreshEngines, setDialog, toast } = useApp();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [usage, setUsage] = useState<{ calls: number; costUsd: number } | null>(null);
  const [secrets, setSecrets] = useState<SecretStatus[]>([]);
  const [confirmText, setConfirmText] = useState("");
  const [scanning, setScanning] = useState(false);
  const [tsKey, setTsKey] = useState("");
  const [jev, setJev] = useState<{ ok: boolean; text: string } | null>(null);

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
                  </small>
                </div>
              </div>
            ))}
          </div>
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
              <small>질문 난이도와 필요한 도구를 미리 판단해 최적의 모델을 선택하고 작업 과정을 조율합니다. 불필요한 검토 단계와 세션 메모리를 생략하여 토큰 소모를 크게 절약합니다. (1M 입력 토큰당 $0.042)</small>
            </span>
            <input type="checkbox" className="switch" role="switch" checked={settings.jevEnabled} onChange={(e) => saveSettings({ ...settings, jevEnabled: e.target.checked })} />
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

        <Section title={AGENT_TEXT.preset} i={4}>
          <Segmented
            label={AGENT_TEXT.preset}
            className="wide"
            value={String(settings.agents ?? 1)}
            options={["1", "2", "3", "4", "5"].map((v) => ({ value: v, label: v === "1" ? AGENT_TEXT.off : v }))}
            onChange={(v) => saveSettings({ ...settings, agents: Number(v) })}
          />
          <p className="help">{AGENT_TEXT.presetHelp}</p>
        </Section>

        <Section title="데이터" i={5}>
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

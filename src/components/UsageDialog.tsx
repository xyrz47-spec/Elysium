import { useEffect, useMemo, useState, type CSSProperties } from "react";
import {
  api,
  ENGINE_LABEL,
  errorText,
  type EngineId,
  type EngineUsage,
  type ModelPrice,
  type UsageRange,
  type UsageSummary,
} from "../lib/api";
import { useApp } from "../store";
import { CloseIcon } from "./Icons";
import Segmented from "./Segmented";

type ViewMode = "stats" | "pricing";

const VIEW_TABS: { value: ViewMode; label: string }[] = [
  { value: "stats", label: "사용 통계" },
  { value: "pricing", label: "토큰 단가표 & 계산기" },
];

const RANGES: { value: UsageRange; label: string }[] = [
  { value: "today", label: "오늘" },
  { value: "7d", label: "7일" },
  { value: "30d", label: "30일" },
  { value: "all", label: "전체" },
];

const GROUPS: { kind: EngineUsage["kind"]; title: string; help: string }[] = [
  {
    kind: "cli",
    title: "CLI",
    help: "비용은 Claude Code 제공 추정치 또는 모델 단가(1M 토큰 기준)로 자동 환산한 금액입니다. 구독 요금제(Claude Pro/Max 등)를 이용 중이면 실제 청구액과 다를 수 있습니다.",
  },
  {
    kind: "api",
    title: "API",
    help: "공식 API 키로 호출한 사용량과 모델 단가(1M 토큰 기준)로 정확히 계산된 비용입니다. NVIDIA 무료 체험은 0달러로 계산됩니다.",
  },
  {
    kind: "jev",
    title: "JEV 판단",
    help: "요청마다 최적의 AI와 노력치를 고르고 세션 기억을 선별하는 데 쓴 사용량입니다 (1M 입력 토큰당 $0.042).",
  },
];

const label = (engine: string) => ENGINE_LABEL[engine as EngineId] ?? (engine === "jev" ? "JEV" : engine);

function tokens(n: number): string {
  if (!n) return "—";
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(n);
}

function usd(n: number): string {
  if (n === 0) return "$0";
  if (n < 0.0001) return `$${n.toFixed(6)}`;
  if (n < 0.01) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(2)}`;
}

function secs(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}초` : `${ms}ms`;
}

function when(iso: string | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  const today = new Date();
  return d.toDateString() === today.toDateString()
    ? d.toLocaleTimeString("ko-KR", { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString("ko-KR", { month: "short", day: "numeric" });
}

const dayLabel = (date: string) => {
  const [, m, d] = date.split("-");
  return `${Number(m)}월 ${Number(d)}일`;
};

/** 하루 단위 호출 수 막대 */
function DailyBars({ days }: { days: UsageSummary["days"] }) {
  const [hover, setHover] = useState<number | null>(null);
  const shown = days.slice(-30);
  const max = Math.max(1, ...shown.map((d) => d.calls));
  const h = shown[hover ?? -1];
  return (
    <figure className="ubars" aria-label="하루 호출 수">
      <div className="ubars-plot" onMouseLeave={() => setHover(null)}>
        <span className="ubars-max">{max}회</span>
        {shown.map((d, i) => (
          <div
            key={d.date}
            className={`ubar ${hover === i ? "on" : ""}`}
            onMouseEnter={() => setHover(i)}
            onFocus={() => setHover(i)}
            tabIndex={0}
            aria-label={`${dayLabel(d.date)} 호출 ${d.calls}회`}
          >
            <i
              style={
                {
                  "--h": d.calls ? `${Math.max(4, (d.calls / max) * 100)}%` : "0%",
                  "--i": i,
                } as CSSProperties
              }
            />
          </div>
        ))}
        {h && (
          <div
            className="ubars-tip"
            style={{
              left: `clamp(64px, ${((hover! + 0.5) / shown.length) * 100}%, calc(100% - 64px))`,
            }}
          >
            <b>{dayLabel(h.date)}</b>
            <span>호출 {h.calls}회</span>
            <span>토큰 {tokens(h.tokens)}</span>
            <span>추정 비용 {usd(h.costUsd)}</span>
          </div>
        )}
      </div>
      <figcaption className="ubars-axis">
        <span>{dayLabel(shown[0].date)}</span>
        <span>{dayLabel(shown[shown.length - 1].date)}</span>
      </figcaption>
    </figure>
  );
}

/** 토큰 단가표 및 실시간 비용 계산기 컴포넌트 */
/** 1M 토큰당 단가: 아주 싼 값(캐시 읽기 $0.005 등)도 0으로 보이지 않게 자릿수를 늘린다. */
const perM = (v: number) => `$${v < 0.1 ? v.toFixed(3) : v.toFixed(2)}`;

function PricingView({
  pricingList,
  onSelectModel,
}: {
  pricingList: ModelPrice[];
  onSelectModel?: (m: ModelPrice) => void;
}) {
  const [filter, setFilter] = useState<string>("all");
  const [calcModel, setCalcModel] = useState<string>(pricingList[0]?.model ?? "sonnet");
  const [calcInput, setCalcInput] = useState<number>(10000);
  const [calcOutput, setCalcOutput] = useState<number>(2000);

  const filtered = useMemo(() => {
    if (filter === "all") return pricingList;
    return pricingList.filter((p) => p.kind === filter);
  }, [pricingList, filter]);

  const selected = useMemo(() => {
    return pricingList.find((p) => p.model === calcModel) ?? pricingList[0];
  }, [pricingList, calcModel]);

  const calcResult = useMemo(() => {
    if (!selected) return { inCost: 0, outCost: 0, total: 0, krw: 0 };
    const inCost = (calcInput * selected.inputUsdPerM) / 1e6;
    const outCost = (calcOutput * selected.outputUsdPerM) / 1e6;
    const total = inCost + outCost;
    const krw = Math.round(total * 1380); // 1 USD ≈ 1,380 KRW
    return { inCost, outCost, total, krw };
  }, [selected, calcInput, calcOutput]);

  const filterOptions = [
    { value: "all", label: "전체" },
    { value: "cli", label: "CLI 엔진" },
    { value: "api", label: "API 엔진" },
    { value: "jev", label: "JEV 판단" },
  ];

  return (
    <div className="pricing-view">
      <div className="pricing-top-bar">
        <Segmented label="단가 분류" value={filter} options={filterOptions} onChange={setFilter} />
        <span className="pricing-note">기준: 1,000,000(1M) 토큰당 USD</span>
      </div>

      <div className="pricing-table-wrap">
        <table className="utable upricing-table">
          <thead>
            <tr>
              <th>구분</th>
              <th>엔진</th>
              <th>모델명</th>
              <th>입력 (1M)</th>
              <th>출력 (1M)</th>
              <th>캐시 입력 (1M)</th>
              <th>설명</th>
            </tr>
          </thead>
          <tbody>
            {filtered.map((p) => {
              const isSelected = selected?.model === p.model;
              return (
                <tr
                  key={`${p.engine}-${p.model}`}
                  className={`pricing-row ${isSelected ? "selected" : ""}`}
                  onClick={() => {
                    setCalcModel(p.model);
                    onSelectModel?.(p);
                  }}
                  title="클릭하면 아래 계산기에 이 모델이 적용됩니다"
                >
                  <td>
                    <span className={`kind-tag ${p.kind}`}>{p.kind.toUpperCase()}</span>
                  </td>
                  <td>
                    <b>{p.engineLabel}</b>
                  </td>
                  <td>
                    <code className="model-code">{p.displayName}</code>
                  </td>
                  <td className="cost-num">{p.inputUsdPerM === 0 ? "무료 ($0)" : perM(p.inputUsdPerM)}</td>
                  <td className="cost-num">{p.outputUsdPerM === 0 ? "무료 ($0)" : perM(p.outputUsdPerM)}</td>
                  <td className="cost-sub">{p.cachedUsdPerM == null || p.inputUsdPerM === 0 ? "—" : perM(p.cachedUsdPerM)}</td>
                  <td>
                    <small className="price-desc">{p.description}</small>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>

      {/* 실시간 토큰 비용 계산기 */}
      <section className="pricing-calc-box">
        <div className="calc-head">
          <h4>실시간 비용 계산기</h4>
          <span className="calc-sub">위 목록에서 모델을 클릭하거나 선택하여 직접 예상 비용을 확인하세요.</span>
        </div>

        <div className="calc-grid">
          <div className="calc-field">
            <label>대상 모델</label>
            <select
              value={calcModel}
              onChange={(e) => setCalcModel(e.target.value)}
              className="calc-select"
            >
              {pricingList.map((p) => (
                <option key={`${p.engine}-${p.model}`} value={p.model}>
                  [{p.engineLabel}] {p.displayName} (입력 ${p.inputUsdPerM} / 출력 ${p.outputUsdPerM})
                </option>
              ))}
            </select>
          </div>

          <div className="calc-field">
            <label>입력 토큰 수</label>
            <input
              type="number"
              min={0}
              step={1000}
              value={calcInput}
              onChange={(e) => setCalcInput(Math.max(0, Number(e.target.value) || 0))}
              className="calc-input"
            />
            <small>≈ {tokens(calcInput)} 토큰</small>
          </div>

          <div className="calc-field">
            <label>출력 토큰 수</label>
            <input
              type="number"
              min={0}
              step={500}
              value={calcOutput}
              onChange={(e) => setCalcOutput(Math.max(0, Number(e.target.value) || 0))}
              className="calc-input"
            />
            <small>≈ {tokens(calcOutput)} 토큰</small>
          </div>

          <div className="calc-result">
            <div className="calc-res-label">추정 합계 비용</div>
            <div className="calc-res-usd">{usd(calcResult.total)}</div>
            <div className="calc-res-krw">약 {calcResult.krw.toLocaleString()}원 (환율 1,380원 기준)</div>
            <div className="calc-res-breakdown">
              <span>입력 {usd(calcResult.inCost)}</span>
              <span>+</span>
              <span>출력 {usd(calcResult.outCost)}</span>
            </div>
          </div>
        </div>

        <div className="pricing-guide-cards">
          <div className="guide-card">
            <b>1k (1,000) 토큰이란?</b>
            <p>한글 약 500~800자, 영문 약 750단어에 해당합니다. 일반적인 1회 질문 또는 짧은 답변의 크기입니다.</p>
          </div>
          <div className="guide-card">
            <b>1M (1,000,000) 토큰이란?</b>
            <p>단행본 책 약 10~15권(영문 약 75만 단어) 분량입니다. 대규모 코드베이스 전체 분석이나 긴 대화 컨텍스트에 해당합니다.</p>
          </div>
          <div className="guide-card">
            <b>비용 절감 팁</b>
            <p>Elysium의 JEV 기능은 질문 난이도를 자동 판별해 쉬운 요청에는 Haiku/Flash 같은 가벼운 모델을 배정하여 토큰 비용을 90% 이상 절감합니다.</p>
          </div>
        </div>
      </section>
    </div>
  );
}

/** CLI·API·JEV별 사용량 및 단가 확인 모달 */
export default function UsageDialog() {
  const { setDialog, toast } = useApp();
  const [view, setView] = useState<ViewMode>("stats");
  const [range, setRange] = useState<UsageRange>("7d");
  const [data, setData] = useState<UsageSummary | null>(null);
  const [prices, setPrices] = useState<ModelPrice[]>([]);

  useEffect(() => {
    api.usageSummary(range).then((res) => {
      setData(res);
      if (res.pricing && res.pricing.length > 0) {
        setPrices(res.pricing);
      }
    }).catch((e) => toast(errorText(e), "error"));
  }, [range, toast]);

  useEffect(() => {
    api.pricingList().then(setPrices).catch(() => {});
  }, []);

  const all = data?.engines ?? [];
  const total = {
    calls: all.reduce((s, e) => s + e.calls, 0),
    tokens: all.reduce((s, e) => s + e.inputTokens + e.outputTokens, 0),
    cost: all.reduce((s, e) => s + e.costUsd, 0),
    failed: all.reduce((s, e) => s + e.failed, 0),
  };

  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
      <div className="modal wide usage" role="dialog" aria-label="사용량 및 토큰 비용">
        <div className="modal-scroll">
        <div className="modal-head">
          <div className="row" style={{ gap: 14 }}>
            <h2>사용량 & 비용</h2>
            <Segmented label="보기 모드" value={view} options={VIEW_TABS} onChange={setView} />
          </div>
          <div className="row">
            {view === "stats" && (
              <Segmented label="기간" value={range} options={RANGES} onChange={setRange} />
            )}
            <button className="icon" onClick={() => setDialog(null)} aria-label="닫기">
              <CloseIcon />
            </button>
          </div>
        </div>

        {view === "stats" ? (
          <>
            <div className="ustats" key={`stats-${range}`}>
              <div style={{ "--i": 0 } as CSSProperties}>
                <b>{total.calls.toLocaleString()}</b>
                <span>전체 호출</span>
              </div>
              <div style={{ "--i": 1 } as CSSProperties}>
                <b>{tokens(total.tokens)}</b>
                <span>총 사용 토큰</span>
              </div>
              <div style={{ "--i": 2 } as CSSProperties}>
                <b className="cost-highlight">{usd(total.cost)}</b>
                <span>총 추정 비용 (USD)</span>
              </div>
              <div style={{ "--i": 3 } as CSSProperties} className={total.failed ? "warn" : ""}>
                <b>{total.failed}</b>
                <span>실패</span>
              </div>
            </div>

            {data && range !== "today" && data.days.length > 1 && <DailyBars key={`bars-${range}`} days={data.days} />}

            {data && all.length === 0 && (
              <p className="uempty">이 기간에는 사용 기록이 없습니다. 질문을 보내면 엔진별로 여기에 누적 계산됩니다.</p>
            )}

            {GROUPS.map((g, gi) => {
              const rows = all.filter((e) => e.kind === g.kind);
              if (!rows.length) return null;
              const groupTotalTokens = rows.reduce((s, r) => s + r.inputTokens + r.outputTokens, 0);
              const groupTotalCost = rows.reduce((s, r) => s + r.costUsd, 0);
              return (
                <section key={g.kind} style={{ "--i": gi } as CSSProperties}>
                  <div className="sec-head">
                    <h3>{g.title}</h3>
                    <span className="sec-summary">
                      {tokens(groupTotalTokens)} 토큰 · {usd(groupTotalCost)}
                    </span>
                  </div>
                  <table className="utable">
                    <thead>
                      <tr>
                        <th>엔진</th>
                        <th>호출</th>
                        <th>입력 토큰</th>
                        <th>출력 토큰</th>
                        <th>추정 비용</th>
                        <th>평균 시간</th>
                        <th>마지막 호출</th>
                      </tr>
                    </thead>
                    <tbody>
                      {rows.map((e) => (
                        <tr key={e.engine}>
                          <td>
                            <div className="uname">{label(e.engine)}</div>
                            <div className="ushare" aria-hidden>
                              <i style={{ "--w": `${(e.calls / Math.max(1, total.calls)) * 100}%` } as CSSProperties} />
                            </div>
                          </td>
                          <td>
                            <b>{e.calls.toLocaleString()}회</b>
                            <small>
                              {e.kind === "jev" ? "판단" : `초안 ${e.drafts} / 검토 ${e.reviews}`}
                              {e.failed > 0 && <em> 실패 {e.failed}</em>}
                            </small>
                          </td>
                          <td>{tokens(e.inputTokens)}</td>
                          <td>{tokens(e.outputTokens)}</td>
                          <td className="cost-val">{usd(e.costUsd)}</td>
                          <td>{secs(e.avgLatencyMs)}</td>
                          <td>{when(e.lastAt)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  <div className="group-foot-bar">
                    <p className="help">{g.help}</p>
                    <button className="btn sm ghost" onClick={() => setView("pricing")}>
                      단가표 기준 보기 →
                    </button>
                  </div>
                </section>
              );
            })}
          </>
        ) : (
          <PricingView pricingList={prices} />
        )}
        </div>
      </div>
    </div>
  );
}

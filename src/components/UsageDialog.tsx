import { useEffect, useState, type CSSProperties } from "react";
import { api, ENGINE_LABEL, errorText, type EngineId, type EngineUsage, type UsageRange, type UsageSummary } from "../lib/api";
import { useApp } from "../store";
import { CloseIcon } from "./Icons";
import Segmented from "./Segmented";

const RANGES: { value: UsageRange; label: string }[] = [
  { value: "today", label: "오늘" },
  { value: "7d", label: "7일" },
  { value: "30d", label: "30일" },
  { value: "all", label: "전체" },
];

const GROUPS: { kind: EngineUsage["kind"]; title: string; help: string }[] = [
  { kind: "cli", title: "CLI", help: "비용은 CLI가 알려 준 추정치입니다. 구독 요금제로 쓰면 실제 청구액과 다릅니다." },
  { kind: "api", title: "API", help: "API 키로 부른 양입니다. NVIDIA 무료 체험은 0달러로 셉니다." },
  { kind: "jev", title: "JEV 판단", help: "요청마다 AI와 노력치를 고르는 데 쓴 양입니다." },
];

const label = (engine: string) => ENGINE_LABEL[engine as EngineId] ?? (engine === "jev" ? "JEV" : engine);

function tokens(n: number): string {
  if (!n) return "—";
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(n);
}

function usd(n: number): string {
  if (!n) return "$0";
  return n < 0.01 ? `$${n.toFixed(4)}` : `$${n.toFixed(2)}`;
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

/** 하루 단위 호출 수 막대(한 가지 색). 막대에 올리면 그날의 호출·토큰·비용이 보인다. */
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
          <div key={d.date} className={`ubar ${hover === i ? "on" : ""}`} onMouseEnter={() => setHover(i)} onFocus={() => setHover(i)} tabIndex={0} aria-label={`${dayLabel(d.date)} 호출 ${d.calls}회`}>
            <i style={{ "--h": d.calls ? `${Math.max(4, (d.calls / max) * 100)}%` : "0%", "--i": i } as CSSProperties} />
          </div>
        ))}
        {h && (
          <div className="ubars-tip" style={{ left: `clamp(64px, ${((hover! + 0.5) / shown.length) * 100}%, calc(100% - 64px))` }}>
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

/** CLI·API·JEV별 사용량: 기간을 고르면 엔진마다 호출·토큰·추정 비용·평균 시간·실패가 보인다. */
export default function UsageDialog() {
  const { setDialog, toast } = useApp();
  const [range, setRange] = useState<UsageRange>("7d");
  const [data, setData] = useState<UsageSummary | null>(null);

  useEffect(() => {
    api.usageSummary(range).then(setData).catch((e) => toast(errorText(e), "error"));
  }, [range, toast]);

  const all = data?.engines ?? [];
  const total = {
    calls: all.reduce((s, e) => s + e.calls, 0),
    tokens: all.reduce((s, e) => s + e.inputTokens + e.outputTokens, 0),
    cost: all.reduce((s, e) => s + e.costUsd, 0),
    failed: all.reduce((s, e) => s + e.failed, 0),
  };

  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && setDialog(null)}>
      <div className="modal wide usage" role="dialog" aria-label="사용량">
        <div className="modal-head">
          <h2>사용량</h2>
          <div className="row">
            <Segmented label="기간" value={range} options={RANGES} onChange={setRange} />
            <button className="icon" onClick={() => setDialog(null)} aria-label="닫기">
              <CloseIcon />
            </button>
          </div>
        </div>

        <div className="ustats" key={`stats-${range}`}>
          <div style={{ "--i": 0 } as CSSProperties}>
            <b>{total.calls}</b>
            <span>호출</span>
          </div>
          <div style={{ "--i": 1 } as CSSProperties}>
            <b>{tokens(total.tokens)}</b>
            <span>토큰</span>
          </div>
          <div style={{ "--i": 2 } as CSSProperties}>
            <b>{usd(total.cost)}</b>
            <span>추정 비용</span>
          </div>
          <div style={{ "--i": 3 } as CSSProperties} className={total.failed ? "warn" : ""}>
            <b>{total.failed}</b>
            <span>실패</span>
          </div>
        </div>

        {data && range !== "today" && data.days.length > 1 && <DailyBars key={`bars-${range}`} days={data.days} />}

        {data && all.length === 0 && <p className="uempty">이 기간에는 사용 기록이 없습니다. 질문을 보내면 엔진별로 여기에 쌓입니다.</p>}

        {GROUPS.map((g, gi) => {
          const rows = all.filter((e) => e.kind === g.kind);
          if (!rows.length) return null;
          return (
            <section key={g.kind} style={{ "--i": gi } as CSSProperties}>
              <h3>{g.title}</h3>
              <table className="utable">
                <thead>
                  <tr>
                    <th>엔진</th>
                    <th>호출</th>
                    <th>입력 토큰</th>
                    <th>출력 토큰</th>
                    <th>추정 비용</th>
                    <th>평균 시간</th>
                    <th>마지막</th>
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
                        <b>{e.calls}</b>
                        <small>
                          {e.kind === "jev" ? "판단" : `초안 ${e.drafts} / 검토 ${e.reviews}`}
                          {e.failed > 0 && <em> 실패 {e.failed}</em>}
                        </small>
                      </td>
                      <td>{tokens(e.inputTokens)}</td>
                      <td>{tokens(e.outputTokens)}</td>
                      <td>{usd(e.costUsd)}</td>
                      <td>{secs(e.avgLatencyMs)}</td>
                      <td>{when(e.lastAt)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <p className="help">{g.help}</p>
            </section>
          );
        })}
      </div>
    </div>
  );
}

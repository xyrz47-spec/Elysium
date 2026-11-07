// Plan limits of the AI subscriptions (Claude Code: 5-hour and weekly windows).
import { useCallback, useEffect, useState, type CSSProperties, type ReactNode } from "react";
import { api, errorText, type AgyLimits, type ClaudeLimits, type CodexLimits } from "../lib/api";
import { ChevronIcon, RefreshIcon } from "./Icons";

/** Refresh while the tab is open (the backend also caches for 30 s). */
const REFRESH_MS = 120_000;

export const LIMIT_TEXT = {
  title: "사용량 한도",
  tip: "AI 구독의 5시간 및 주간 잔여 한도 확인",
  claude: "Claude Code",
  five_hour: "5시간 한도",
  seven_day: "주간 전체 한도",
  seven_day_opus: "Opus 한도",
  seven_day_sonnet: "Sonnet 한도",
  seven_day_oauth_apps: "서드파티 앱 한도",
  resets: "재설정: {t}",
  inHours: "{h}시간 {m}분후",
  inMinutes: "{m}분 후",
  extra: "추가 사용량",
  login: "먼저 Claude Code에 로그인해 주세요.",
  expired: "로그인 세션이 만료되었습니다. 다음 Claude Code 실행 시 자동으로 갱신됩니다.",
  checked: "마지막 확인: {t}",
  refresh: "새로고침",
  plan: "플랜",
  antigravity: "Antigravity",
  agyOff: "사용량을 확인하려면 Antigravity 앱을 실행해 주세요.",
  weekly: "주간 한도",
  agyNeedsApp: "앱이 실행 중이어야 한도가 표시됩니다",
  codex: "Codex",
  codexMissing: "Codex CLI가 설치되어 있지 않습니다.",
  codexLogin: "codex를 한 번 실행하여 로그인해 주세요.",
  days: "{d}일",
  hours: "{h}시간",
  fold: "접기/펼치기",
  agyHint: "같은 막대에 표시된 모델은 사용 한도를 공유합니다.",
};

function when(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return null;
  const mins = Math.max(0, Math.round((t - Date.now()) / 60000));
  if (mins < 60) return LIMIT_TEXT.inMinutes.replace("{m}", String(mins));
  if (mins < 24 * 60) return LIMIT_TEXT.inHours.replace("{h}", String(Math.floor(mins / 60))).replace("{m}", String(mins % 60));
  const d = new Date(t);
  return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** "Gemini 4 Argon (Medium)", "Gemini 4 Argon (Low)" → "Gemini 4 Argon (Medium·Low)" */
function poolName(models: string[]): string {
  const by = new Map<string, string[]>();
  for (const m of models) {
    const hit = /^(.*?)\s*\(([^)]*)\)$/.exec(m);
    const [base, level] = hit ? [hit[1], hit[2]] : [m, ""];
    by.set(base, [...(by.get(base) ?? []), ...(level ? [level] : [])]);
  }
  return [...by.entries()].map(([base, lv]) => (lv.length ? `${base} (${lv.join("·")})` : base)).join(" + ");
}

/** Argon has only a weekly limit; for the others the API gives one reset time, so a near reset means the 5-hour window. */
function poolKind(models: string[], resetsAt: string | null): string {
  if (models.some((m) => /argon/i.test(m))) return LIMIT_TEXT.weekly;
  const t = resetsAt ? new Date(resetsAt).getTime() - Date.now() : NaN;
  return !Number.isNaN(t) && t <= 5.2 * 3600_000 ? LIMIT_TEXT.five_hour : LIMIT_TEXT.weekly;
}

/** Codex reports the window length: 5 hours, a week, 30 days … */
function windowName(minutes: number): string {
  if (minutes === 300) return LIMIT_TEXT.five_hour;
  if (minutes === 10080) return LIMIT_TEXT.weekly;
  if (minutes % 1440 === 0) return LIMIT_TEXT.days.replace("{d}", String(minutes / 1440));
  return LIMIT_TEXT.hours.replace("{h}", String(Math.round(minutes / 60)));
}

const OPEN_KEY = "elysium.limitsOpen";
function loadOpen(): Record<string, boolean> {
  try {
    return JSON.parse(localStorage.getItem(OPEN_KEY) ?? "{}") as Record<string, boolean>;
  } catch {
    return {};
  }
}

/** One CLI's limits; the header folds it. The open/closed choice is remembered. */
function CliSection({ id, title, extra, note, children }: { id: string; title: string; extra?: ReactNode; note?: string; children: ReactNode }) {
  const [open, setOpen] = useState(() => loadOpen()[id] ?? true);
  const toggle = () => {
    setOpen((o) => {
      try {
        localStorage.setItem(OPEN_KEY, JSON.stringify({ ...loadOpen(), [id]: !o }));
      } catch {
        /* not saved: this run only */
      }
      return !o;
    });
  };
  return (
    <section className={`tool-sec limit-cli ${open ? "open" : ""}`}>
      <h4>
        <button type="button" className="limit-fold" onClick={toggle} aria-expanded={open} title={LIMIT_TEXT.fold}>
          <ChevronIcon open={open} />
          {title}
        </button>
        {extra}
        {note && <span className="limit-note">{note}</span>}
      </h4>
      {open && children}
    </section>
  );
}

function Bar({ label, percent, resets }: { label: string; percent: number; resets: string | null }) {
  const p = Math.min(100, Math.max(0, percent));
  const level = p >= 90 ? "high" : p >= 70 ? "mid" : "low";
  return (
    <div className={`limit-row ${level}`}>
      <div className="limit-head">
        <b>{label}</b>
        <span className="limit-pct">{Math.round(p)}%</span>
      </div>
      <div className="limit-bar" role="progressbar" aria-label={label} aria-valuenow={Math.round(p)} aria-valuemin={0} aria-valuemax={100}>
        <i style={{ "--w": `${p}%` } as CSSProperties} />
      </div>
      {resets && <div className="agent-sub">{LIMIT_TEXT.resets.replace("{t}", resets)}</div>}
    </div>
  );
}

export default function LimitsTab() {
  const [c, setC] = useState<ClaudeLimits | null>(null);
  const [g, setG] = useState<AgyLimits | null>(null);
  const [x, setX] = useState<CodexLimits | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const load = useCallback(() => {
    setBusy(true);
    api
      .limitsClaude()
      .then((v) => (setC(v), setErr(null)))
      .catch((e) => setErr(errorText(e)))
      .finally(() => setBusy(false));
    api
      .limitsAntigravity()
      .then(setG)
      .catch((e) => setG({ plan: null, groups: [], pools: [], error: errorText(e), checkedAt: 0 }));
    api
      .limitsCodex()
      .then(setX)
      .catch((e) => setX({ plan: null, groups: [], error: errorText(e), checkedAt: 0 }));
  }, []);
  useEffect(() => {
    load();
    const t = setInterval(load, REFRESH_MS);
    return () => clearInterval(t);
  }, [load]);
  const checked = c?.checkedAt ? new Date(c.checkedAt * 1000) : null;
  return (
    <div className="tool-body">
      <div className="limit-top">
        <button type="button" className="icon limit-refresh" onClick={load} disabled={busy} aria-label={LIMIT_TEXT.refresh} title={LIMIT_TEXT.refresh}>
          <RefreshIcon />
        </button>
      </div>
      <CliSection
        id="claude"
        title={LIMIT_TEXT.claude}
        extra={
          c?.plan && (
            <span className="tag">
              {LIMIT_TEXT.plan} {c.plan}
            </span>
          )
        }
      >
        {err && <div className="agent-err">{err}</div>}
        {c?.error === "login" && <p className="tool-help">{LIMIT_TEXT.login}</p>}
        {c?.error === "expired" && <p className="tool-help">{LIMIT_TEXT.expired}</p>}
        {c?.error && c.error !== "login" && c.error !== "expired" && <div className="agent-err">{c.error}</div>}
        {c?.windows.map((w) => (
          <Bar key={w.key} label={(LIMIT_TEXT as Record<string, string>)[w.key] ?? w.key} percent={w.percent} resets={when(w.resetsAt)} />
        ))}
        {c && c.extraEnabled && c.extraLimit != null && (
          <div className="limit-extra">
            <span>{LIMIT_TEXT.extra}</span>
            <b>
              ${(c.extraUsed ?? 0).toFixed(2)} / ${c.extraLimit.toFixed(2)}
            </b>
          </div>
        )}
        {checked && <div className="agent-sub">{LIMIT_TEXT.checked.replace("{t}", `${String(checked.getHours()).padStart(2, "0")}:${String(checked.getMinutes()).padStart(2, "0")}`)}</div>}
      </CliSection>
      <CliSection id="codex" title={LIMIT_TEXT.codex} extra={x?.plan && <span className="tag">{x.plan}</span>}>
        {x?.error === "missing" && <p className="tool-help">{LIMIT_TEXT.codexMissing}</p>}
        {x?.error === "login" && <p className="tool-help">{LIMIT_TEXT.codexLogin}</p>}
        {x?.error && x.error !== "missing" && x.error !== "login" && <div className="agent-err">{x.error}</div>}
        {x?.groups.map((grp, i) => (
          <div key={grp.name ?? i} className={x.groups.length > 1 ? "limit-group" : "limit-plain"}>
            {grp.name && <h5>{grp.name}</h5>}
            {grp.windows.map((w) => (
              <Bar key={w.minutes} label={windowName(w.minutes)} percent={w.percent} resets={when(w.resetsAt != null ? new Date(w.resetsAt * 1000).toISOString() : null)} />
            ))}
          </div>
        ))}
      </CliSection>
      <CliSection id="antigravity" title={LIMIT_TEXT.antigravity} extra={g?.plan && <span className="tag">{g.plan}</span>} note={LIMIT_TEXT.agyNeedsApp}>
        {g?.error === "app_off" && <p className="tool-help">{LIMIT_TEXT.agyOff}</p>}
        {g?.error && g.error !== "app_off" && <div className="agent-err">{g.error}</div>}
        {g?.groups.map((grp) => (
          <div key={grp.name} className="limit-group">
            <h5>{grp.name}</h5>
            {grp.windows.map((w) => (
              <Bar key={w.key} label={w.key === "5h" ? LIMIT_TEXT.five_hour : LIMIT_TEXT.weekly} percent={w.percent} resets={when(w.resetsAt)} />
            ))}
          </div>
        ))}
        {g?.pools.map((p) => (
          <Bar key={p.models.join("|")} label={`${poolName(p.models)} · ${poolKind(p.models, p.resetsAt)}`} percent={p.percent} resets={when(p.resetsAt)} />
        ))}
        {g && g.groups.length === 0 && g.pools.length > 1 && <div className="agent-sub">{LIMIT_TEXT.agyHint}</div>}
      </CliSection>
    </div>
  );
}

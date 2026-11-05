// 워커·그룹 하나를 따로 보여 주는 창. 다른 모니터로 옮길 수 있다. 상태는 메인 창에서 받아 채우고 이후 이벤트를 따른다.
import { emit } from "@tauri-apps/api/event";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Elapsed, LT, NowLine, runState, shownState, splitActivity, VERDICT } from "./components/LeadPanel";
import Segmented from "./components/Segmented";
import LiveMarkdown from "./components/LiveMarkdown";
import Markdown from "./components/Markdown";
import { SendIcon, StopIcon } from "./components/Icons";
import { api, ENGINE_LABEL, errorText, type GroupNote, type LeadDecision, type OrgPost, type OrgUnit } from "./lib/api";
import { useTauriEvent } from "./lib/events";
import { hideBlocks, STREAM_KEEP } from "./lib/panel";
import type { AgentRun, PartInfo } from "./store";
import { TOKEN_TEXT } from "./components/TokenMeter";
import { liveTurn, shortTokens, total, turnEvent, usd, type RunUsage } from "./lib/usage";

export interface WorkerSnapshot {
  label: string;
  runs: Record<string, AgentRun>;
  streams: Record<string, string>;
  parts: Record<string, PartInfo>;
  usage?: Record<string, RunUsage>;
  decisions: LeadDecision[];
  groupLog: GroupNote[];
  /** 조직으로 일할 때 이 창의 팀 */
  unit?: OrgUnit | null;
  /** 이 팀이 보내거나 받은 팀장 보고 */
  orgLog?: OrgPost[];
}

type Target = "worker" | "lead";
type Tab = "output" | "activity" | "files" | "talk" | "org" | "commands";

/** 글자 조각을 창에 반영하는 간격(ms). 조각마다 다시 그리지 않는다. */
const FLUSH_MS = 80;
/** 보관 한도를 넘으면 앞을 이만큼 더 잘라, 잘린 경계가 글자마다 움직이지 않게 한다. */
const TRIM_STEP = 50_000;

/** 보관 한도를 넘었을 때만 앞을 크게 자른다(덩어리 경계가 그대로 남아 다시 그리지 않는다). */
function keepTail(text: string): string {
  return text.length > STREAM_KEEP ? text.slice(text.length - STREAM_KEEP + TRIM_STEP) : text;
}

function keysOf(key: string): string[] {
  return key.startsWith("group:") ? key.split(":").slice(2).join(":").split(",") : [key];
}

const memberName = (i: number) => LT.memberN.replace("{n}", String(i + 1));

/** 머리의 숫자 하나 */
function Metric({ label, children, accent = false }: { label: string; children: ReactNode; accent?: boolean }) {
  return (
    <span className={`worker-metric ${accent ? "accent" : ""}`}>
      <em>{label}</em>
      <b>{children}</b>
    </span>
  );
}

/** 사용한 도구를 최신 것부터 */
function Timeline({ items }: { items: string[] }) {
  if (!items.length) return <div className="worker-empty">{LT.emptyActivity}</div>;
  return (
    <ol className="worker-timeline">
      {items
        .slice()
        .reverse()
        .map((a, i) => {
          const { tool, detail } = splitActivity(a);
          return (
            <li key={items.length - i} className={i === 0 ? "latest" : ""}>
              <i aria-hidden />
              {tool && <span className="tag tool">{tool}</span>}
              <span className="worker-timeline-text" title={detail}>
                {detail}
              </span>
            </li>
          );
        })}
    </ol>
  );
}

/** 그룹 대화: 누가 몇 라운드에 무엇을 말했는지, 맡은 파일 */
function Talk({ log }: { log: GroupNote[] }) {
  const box = useRef<HTMLOListElement>(null);
  useEffect(() => {
    if (box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [log.length]);
  if (!log.length) return <div className="worker-empty">{LT.noTalk}</div>;
  return (
    <ol ref={box} className="worker-feed">
      {log.map((g, i) => (
        <li key={i} className={g.done ? "done" : ""}>
          <div className="worker-feed-head">
            <b>{g.who}</b>
            <span className="agent-sub">{LT.round.replace("{n}", String(g.round + 1))}</span>
            {g.done && <span className="tag ok">{LT.finished}</span>}
          </div>
          {g.text.trim() && <Markdown text={hideBlocks(g.text)} />}
          {g.claim.length > 0 && (
            <div className="agent-files">
              {g.claim.map((f) => (
                <code key={f}>{f}</code>
              ))}
            </div>
          )}
        </li>
      ))}
    </ol>
  );
}

/** 팀장 보고: 이 팀이 보내거나 받은 것 */
function OrgFeed({ log, unit }: { log: OrgPost[]; unit: OrgUnit }) {
  if (!log.length) return <div className="worker-empty">{LT.emptyOrg}</div>;
  return (
    <ol className="worker-feed">
      {log.map((p, i) => {
        const mine = p.from === unit.index;
        return (
          <li key={i} className={mine ? "mine" : ""}>
            <div className="worker-feed-head">
              <span className={`tag ${mine ? "live" : ""}`}>{mine ? LT.orgSent : LT.orgGot}</span>
              <b>{p.title}</b>
            </div>
            <Markdown text={p.text} />
          </li>
        );
      })}
    </ol>
  );
}

export default function WorkerWindow({ task, windowKey }: { task: string; windowKey: string }) {
  const label = useRef(crypto.randomUUID()).current;
  const keys = keysOf(windowKey);
  const groupIndex = windowKey.startsWith("group:") ? Number(windowKey.split(":")[1]) : null;
  const [snap, setSnap] = useState<WorkerSnapshot | null>(null);
  const [shown, setShown] = useState(keys[0]);
  const [tab, setTab] = useState<Tab>("output");
  const [target, setTarget] = useState<Target>("worker");
  const [msg, setMsg] = useState("");
  const [error, setError] = useState<string | null>(null);
  const patch = (fn: (s: WorkerSnapshot) => WorkerSnapshot) => setSnap((s) => (s ? fn(s) : s));

  // 구독이 걸린 뒤에 물어봐야 그 사이의 이벤트를 잃지 않는다.
  useTauriEvent<WorkerSnapshot>("worker-window:snapshot", (s) => {
    if (s.label === label) setSnap(s);
  });
  // 답이 구독보다 먼저 오면 잃어버리므로 받을 때까지 다시 묻는다.
  useEffect(() => {
    if (snap) return;
    const ask = () => emit("worker-window:hello", { label, task, keys });
    ask();
    const t = setInterval(ask, 700);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snap === null]);
  // 글자 조각은 모았다가 짧은 간격마다 한 번에 붙인다.
  const pending = useRef<{ text: Record<string, string>; timer: number | null }>({ text: {}, timer: null });
  useEffect(
    () => () => {
      if (pending.current.timer != null) window.clearTimeout(pending.current.timer);
    },
    [],
  );
  useTauriEvent<{ taskId: string; key: string; delta: string }>("task:agent-delta", (p) => {
    if (p.taskId !== task || !keys.includes(p.key)) return;
    const q = pending.current;
    q.text[p.key] = (q.text[p.key] ?? "") + p.delta;
    if (q.timer != null) return;
    q.timer = window.setTimeout(() => {
      const add = q.text;
      q.text = {};
      q.timer = null;
      patch((s) => {
        const streams = { ...s.streams };
        for (const [k, d] of Object.entries(add)) streams[k] = keepTail((streams[k] ?? "") + d);
        return { ...s, streams };
      });
    }, FLUSH_MS);
  });
  useTauriEvent<{ taskId: string; key: string; text: string }>("task:agent-activity", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => (s.runs[p.key] ? { ...s, runs: { ...s.runs, [p.key]: { ...s.runs[p.key], activities: [...s.runs[p.key].activities, p.text].slice(-200) } } } : s));
  });
  useTauriEvent<{ taskId: string; key: string; note: string }>("task:agent-wait", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => (s.runs[p.key] ? { ...s, runs: { ...s.runs, [p.key]: { ...s.runs[p.key], state: "run", wait: p.note } } } : s));
  });
  useTauriEvent<{ taskId: string; key: string; state: AgentRun["state"]; stopped?: boolean; error?: string | null; label: string; engine: AgentRun["engine"]; role: string; model?: string | null; effort?: string | null; ms?: number; inputTokens?: number | null; outputTokens?: number | null; costUsd?: number | null }>("task:agent", (p) => {
    if (p.taskId === task && keys.includes(p.key))
      patch((s) => {
        const prev = s.runs[p.key];
        const start = p.state === "run" && prev?.state !== "run";
        const run: AgentRun = {
          key: p.key,
          engine: p.engine,
          label: p.label,
          role: p.role,
          model: p.model ?? prev?.model ?? null,
          effort: p.effort ?? prev?.effort ?? null,
          startedAt: prev?.startedAt ?? Date.now(),
          ms: start ? null : (p.ms ?? prev?.ms ?? null),
          chars: prev?.chars ?? null,
          activities: prev?.activities ?? [],
          sent: prev?.sent ?? [],
          state: p.state,
          stopped: !!p.stopped,
          error: p.error ?? null,
          wait: null,
        };
        return { ...s, runs: { ...s.runs, [p.key]: run }, usage: { ...s.usage, [p.key]: turnEvent(s.usage?.[p.key], p) } };
      });
  });
  useTauriEvent<{ taskId: string; key: string; input: number; output: number; costUsd: number | null }>("task:agent-usage", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => ({ ...s, usage: { ...s.usage, [p.key]: liveTurn(s.usage?.[p.key], p) } }));
  });
  useTauriEvent<{ taskId: string; key: string; text: string }>("task:agent-message", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => (s.runs[p.key] ? { ...s, runs: { ...s.runs, [p.key]: { ...s.runs[p.key], sent: [...s.runs[p.key].sent, p.text] } } } : s));
  });
  useTauriEvent<LeadDecision>("lead:decision", (d) => {
    if (d.taskId === task && keys.includes(d.key)) patch((s) => ({ ...s, decisions: [...s.decisions, d] }));
  });
  useTauriEvent<GroupNote>("group:say", (g) => {
    if (g.taskId === task && g.group === groupIndex) patch((s) => ({ ...s, groupLog: [...s.groupLog, g] }));
  });
  useTauriEvent<{ taskId: string; units: OrgUnit[] }>("task:org", (p) => {
    if (p.taskId === task) patch((s) => ({ ...s, unit: p.units.find((u) => u.keys.some((k) => keys.includes(k))) ?? s.unit ?? null }));
  });
  useTauriEvent<OrgPost>("org:post", (p) => {
    if (p.taskId === task) patch((s) => (s.unit && (p.from === s.unit.index || p.to.includes(s.unit.index)) ? { ...s, orgLog: [...(s.orgLog ?? []), p] } : s));
  });

  const send = async () => {
    const t = msg.trim();
    if (!t) return;
    setMsg("");
    setError(null);
    const go = target === "worker" ? api.agentMessage(task, shown, t) : api.leadSend(task, t, shown);
    await go.catch((e) => {
      setMsg(t);
      setError(errorText(e));
    });
  };

  const run = snap?.runs[shown];
  const part = snap?.parts[shown] ?? snap?.parts[keys[0]];
  const text = hideBlocks(snap?.streams[shown] ?? "");
  const decisions = snap?.decisions.filter((d) => d.key === shown) ?? [];
  const live = run?.state === "run" && !run.wait;
  const who = run ? [ENGINE_LABEL[run.engine] ?? run.label, run.model, run.effort].filter(Boolean) : [];
  const tok = total(snap?.usage?.[shown]);
  const unit = snap?.unit ?? null;
  const orgLog = snap?.orgLog ?? [];
  const group = keys.length > 1;
  const count = (n: number) => (n > 0 ? <i className="worker-tab-count">{n >= 200 ? "200+" : n}</i> : null);
  const tabs: { value: Tab; label: ReactNode }[] = [
    { value: "output", label: LT.tabOutput },
    { value: "activity", label: <>{LT.tabActivity}{count(run?.activities.length ?? 0)}</> },
    { value: "files", label: <>{LT.tabFiles}{count(part?.files.length ?? 0)}</> },
    ...(groupIndex !== null ? [{ value: "talk" as Tab, label: <>{LT.tabTalk}{count(snap?.groupLog.length ?? 0)}</> }] : []),
    ...(unit && unit.peers.length > 0 ? [{ value: "org" as Tab, label: <>{LT.tabOrg}{count(orgLog.length)}</> }] : []),
    ...(decisions.length > 0 ? [{ value: "commands" as Tab, label: <>{LT.tabCommands}{count(decisions.length)}</> }] : []),
  ];
  const tabNow = tabs.some((t) => t.value === tab) ? tab : "output";
  const title = part?.title ?? run?.label ?? "";
  return (
    <div className="worker-win">
      <main className={`worker-inner ${run?.state ?? ""}`}>
        {!snap ? (
          <div className="worker-empty">{LT.winConnecting}</div>
        ) : (
          <>
            <header className="worker-head">
              <div className="worker-title">
                <span className={`worker-status ${run?.state ?? ""} ${run?.wait ? "waiting" : ""}`} aria-hidden>
                  <i />
                </span>
                <div className="worker-name">
                  <span className="worker-kicker">{[unit ? (unit.org.length ? unit.org.join(" › ") : LT.orgDirect) : null, ...who].filter(Boolean).join(" · ")}</span>
                  <h1 title={title}>{title}</h1>
                </div>
                <div className="spacer" />
                {run && <span className={`worker-pill ${run.state} ${run.wait ? "wait" : ""}`}>{run.stopped ? LT.failed : runState(run)}</span>}
                {run?.state === "run" && (
                  <button className="btn sm danger" onClick={() => api.agentCancel(task, shown).catch((e) => setError(errorText(e)))}>
                    <StopIcon /> {LT.stop}
                  </button>
                )}
              </div>
              {run && (
                <div className="worker-statusline">
                  {run.state === "run" ? (
                    <NowLine run={run} big />
                  ) : run.state === "fail" && run.error && !run.stopped ? (
                    <div className="agent-err" title={run.error}>
                      {run.error}
                    </div>
                  ) : (
                    <div className={`worker-now big done ${run.state}`}>{run.state === "ok" ? LT.doneLine : LT.failed}</div>
                  )}
                  <div className="worker-metrics">
                    <Metric label={LT.elapsed}>
                      <Elapsed run={run} />
                    </Metric>
                    <Metric label={TOKEN_TEXT.input}>{shortTokens(tok.input)}</Metric>
                    <Metric label={TOKEN_TEXT.output}>{shortTokens(tok.output)}</Metric>
                    <Metric label={TOKEN_TEXT.cost} accent>
                      {usd(tok.cost)}
                    </Metric>
                  </div>
                </div>
              )}
              {group && (
                <div className="worker-members" role="tablist" aria-label={LT.membersLabel}>
                  {keys.map((k, i) => {
                    const a = snap.runs[k];
                    const st = a ? shownState(a) : "waiting";
                    return (
                      <button key={k} type="button" role="tab" aria-selected={k === shown} className={`worker-member st-${st} ${k === shown ? "on" : ""}`} onClick={() => setShown(k)}>
                        <i className="agent-dot" aria-hidden />
                        <span className="worker-member-name">
                          <b>{memberName(i)}</b>
                          {i === 0 && <span className="tag">{LT.leader}</span>}
                        </span>
                        <span className="worker-member-sub">{a ? `${ENGINE_LABEL[a.engine] ?? a.label} · ${a.stopped ? LT.failed : runState(a)}` : LT.waitMail}</span>
                      </button>
                    );
                  })}
                </div>
              )}
            </header>

            <Segmented as="tab" label={LT.winTabs} className="worker-tabs" value={tabNow} onChange={setTab} options={tabs} />

            <section className="worker-pane" role="tabpanel">
              {tabNow === "output" && (
                <>
                  {text.length >= STREAM_KEEP - TRIM_STEP && <div className="agent-sub worker-cut">{LT.winCut}</div>}
                  <LiveMarkdown text={text} live={live} empty={LT.winNoOutput} jumpLabel={LT.winToBottom} />
                </>
              )}
              {tabNow === "activity" && <Timeline items={run?.activities ?? []} />}
              {tabNow === "files" &&
                (part && part.files.length > 0 ? (
                  <div className="agent-files worker-files">
                    {part.files.map((f) => (
                      <code key={f}>{f}</code>
                    ))}
                  </div>
                ) : (
                  <div className="worker-empty">{LT.emptyFiles}</div>
                ))}
              {tabNow === "talk" && <Talk log={snap.groupLog} />}
              {tabNow === "org" && unit && <OrgFeed log={orgLog} unit={unit} />}
              {tabNow === "commands" && (
                <ul className="worker-list">
                  {decisions
                    .slice()
                    .reverse()
                    .map((d) => (
                      <li key={d.id}>
                        <span className={`tag verdict ${d.verdict}`}>{VERDICT[d.verdict]}</span>
                        <code title={d.command}>{d.command}</code>
                      </li>
                    ))}
                </ul>
              )}
            </section>

            <footer className="worker-foot">
              {error && <div className="agent-err">{error}</div>}
              <div className="worker-compose">
                <Segmented
                  label={LT.winSendTarget}
                  value={target}
                  onChange={setTarget}
                  options={[
                    { value: "worker", label: group ? memberName(keys.indexOf(shown)) : LT.winToWorker },
                    { value: "lead", label: LT.winToLead },
                  ]}
                />
                <input
                  type="text"
                  value={msg}
                  onChange={(e) => setMsg(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && (e.preventDefault(), send())}
                  placeholder={target === "worker" ? LT.winWorkerPlaceholder : LT.reportPlaceholder}
                  aria-label={target === "worker" ? LT.winToWorker : LT.report}
                />
                <button className="icon" onClick={send} disabled={!msg.trim()} aria-label={target === "worker" ? LT.winToWorker : LT.report} title={target === "worker" ? LT.winToWorker : LT.report}>
                  <SendIcon />
                </button>
              </div>
            </footer>
          </>
        )}
      </main>
    </div>
  );
}

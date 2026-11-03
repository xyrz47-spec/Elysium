// 워커·그룹 하나를 따로 보여 주는 창. 다른 모니터로 옮길 수 있다. 상태는 메인 창에서 받아 채우고 이후 이벤트를 따른다.
import { emit } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { Elapsed, LT, NowLine, runState, splitActivity, VERDICT } from "./components/LeadPanel";
import ParticleOrb from "./components/ParticleOrb";
import Segmented from "./components/Segmented";
import { SendIcon, StopIcon } from "./components/Icons";
import { api, ENGINE_LABEL, errorText, type GroupNote, type LeadDecision } from "./lib/api";
import { useTauriEvent } from "./lib/events";
import { hideBlocks, STREAM_KEEP } from "./lib/panel";
import type { AgentRun, PartInfo } from "./store";

export interface WorkerSnapshot {
  label: string;
  runs: Record<string, AgentRun>;
  streams: Record<string, string>;
  parts: Record<string, PartInfo>;
  decisions: LeadDecision[];
  groupLog: GroupNote[];
}

type Target = "worker" | "lead";

/** 글자 조각을 창에 반영하는 간격(ms). 조각마다 다시 그리지 않는다. */
const FLUSH_MS = 50;

function keysOf(key: string): string[] {
  return key.startsWith("group:") ? key.split(":").slice(2).join(":").split(",") : [key];
}

/** 실시간 출력. 맨 아래에 있으면 새 글을 따라가고, 위로 올려 읽는 중이면 그대로 둔다. */
function LiveOutput({ text, live }: { text: string; live: boolean }) {
  const ref = useRef<HTMLPreElement>(null);
  const [follow, setFollow] = useState(true);
  useEffect(() => {
    const el = ref.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [text, follow]);
  const onScroll = () => {
    const el = ref.current;
    if (el) setFollow(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
  };
  if (!text) return <div className="worker-empty">{LT.winNoOutput}</div>;
  return (
    <div className="worker-live">
      <pre ref={ref} className={`worker-out ${live ? "streaming" : ""}`} onScroll={onScroll}>
        {hideBlocks(text)}
        {live && <i className="worker-caret" aria-hidden />}
      </pre>
      {!follow && (
        <button className="btn sm worker-jump" onClick={() => setFollow(true)}>
          ↓ {LT.winToBottom}
        </button>
      )}
    </div>
  );
}

/** 사용한 도구를 최신 것부터 */
function Timeline({ items }: { items: string[] }) {
  return (
    <ol className="worker-timeline">
      {items
        .slice(-60)
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

export default function WorkerWindow({ task, windowKey }: { task: string; windowKey: string }) {
  const label = useRef(crypto.randomUUID()).current;
  const keys = keysOf(windowKey);
  const groupIndex = windowKey.startsWith("group:") ? Number(windowKey.split(":")[1]) : null;
  const [snap, setSnap] = useState<WorkerSnapshot | null>(null);
  const [shown, setShown] = useState(keys[0]);
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
        for (const [k, d] of Object.entries(add)) streams[k] = ((streams[k] ?? "") + d).slice(-STREAM_KEEP);
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
  useTauriEvent<{ taskId: string; key: string; state: AgentRun["state"]; stopped?: boolean; error?: string | null; label: string; engine: AgentRun["engine"]; role: string; model?: string | null; effort?: string | null; ms?: number }>("task:agent", (p) => {
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
        return { ...s, runs: { ...s.runs, [p.key]: run } };
      });
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
  const part = snap?.parts[shown];
  const text = snap?.streams[shown] ?? "";
  const decisions = snap?.decisions.filter((d) => d.key === shown) ?? [];
  const live = run?.state === "run" && !run.wait;
  const who = run ? [ENGINE_LABEL[run.engine] ?? run.label, run.model, run.effort].filter(Boolean) : [];
  return (
    <div className="worker-win">
      <ParticleOrb mode={live ? "thinking" : "idle"} />
      <main className={`worker-inner ${run?.state ?? ""}`}>
        {!snap ? (
          <div className="worker-empty">{LT.winConnecting}</div>
        ) : (
          <>
            <header className="worker-head">
              <div className="worker-title">
                <i className={`agent-dot ${run?.state ?? ""}`} aria-hidden />
                <h1 title={part?.title ?? run?.label}>{part?.title ?? run?.label}</h1>
                <div className="spacer" />
                {run && <Elapsed run={run} />}
                {run?.state === "run" && (
                  <button className="btn sm danger" onClick={() => api.agentCancel(task, shown).catch((e) => setError(errorText(e)))}>
                    <StopIcon /> {LT.stop}
                  </button>
                )}
              </div>
              <div className="worker-meta">
                {who.map((w) => (
                  <span key={w} className="tag">
                    {w}
                  </span>
                ))}
                {run && <span className={`agent-state ${run.wait ? "wait" : ""}`}>{run.stopped ? LT.failed : runState(run)}</span>}
              </div>
              {keys.length > 1 && (
                <Segmented
                  as="tab"
                  label={LT.member}
                  value={shown}
                  onChange={setShown}
                  options={keys.map((k, i) => ({ value: k, label: `${LT.member} ${i + 1}` }))}
                />
              )}
            </header>

            <div className="worker-body">
              <section className="worker-main">
                {run?.state === "run" && (
                  <div className="worker-card">
                    <h5>{LT.winNow}</h5>
                    <NowLine run={run} big />
                  </div>
                )}
                <div className="worker-card grow">
                  <h5>
                    {LT.winLive}
                    {text.length >= STREAM_KEEP && <span className="agent-sub"> · {LT.winCut}</span>}
                  </h5>
                  <LiveOutput text={text} live={live} />
                </div>
              </section>

              <aside className="worker-side">
                {run && run.activities.length > 0 && (
                  <div className="worker-card">
                    <h5>{LT.winTimeline}</h5>
                    <Timeline items={run.activities} />
                  </div>
                )}
                {part && part.files.length > 0 && (
                  <div className="worker-card">
                    <h5>{LT.winFiles}</h5>
                    <div className="agent-files">
                      {part.files.map((f) => (
                        <code key={f}>{f}</code>
                      ))}
                    </div>
                  </div>
                )}
                {decisions.length > 0 && (
                  <div className="worker-card">
                    <h5>{LT.decisions}</h5>
                    <ul className="worker-list">
                      {decisions
                        .slice(-10)
                        .reverse()
                        .map((d) => (
                          <li key={d.id}>
                            <span className={`tag verdict ${d.verdict}`}>{VERDICT[d.verdict]}</span>
                            <code title={d.command}>{d.command}</code>
                          </li>
                        ))}
                    </ul>
                  </div>
                )}
                {groupIndex !== null && (
                  <div className="worker-card">
                    <h5>{LT.winTalk}</h5>
                    {snap.groupLog.length === 0 ? (
                      <div className="agent-sub">{LT.noTalk}</div>
                    ) : (
                      <ul className="lead-said worker-talk">
                        {snap.groupLog.map((g, i) => (
                          <li key={i}>
                            <b>{g.who}</b> {hideBlocks(g.text)}
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>
                )}
              </aside>
            </div>

            <footer className="worker-foot">
              {error && <div className="agent-err">{error}</div>}
              <div className="worker-compose">
                <Segmented
                  label={LT.winSendTarget}
                  value={target}
                  onChange={setTarget}
                  options={[
                    { value: "worker", label: LT.winToWorker },
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

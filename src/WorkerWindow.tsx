// 워커·그룹 하나를 따로 보여 주는 창. 다른 모니터로 옮길 수 있다. 상태는 메인 창에서 받아 채우고 이후 이벤트를 따른다.
import { emit } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { LT, runState } from "./components/LeadPanel";
import ParticleOrb from "./components/ParticleOrb";
import { SendIcon, StopIcon } from "./components/Icons";
import { api, errorText, type GroupNote, type LeadDecision } from "./lib/api";
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

function keysOf(key: string): string[] {
  return key.startsWith("group:") ? key.split(":").slice(2).join(":").split(",") : [key];
}

function Output({ text }: { text: string }) {
  const ref = useRef<HTMLPreElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);
  return text ? (
    <pre ref={ref} className="agent-out worker-out">
      {hideBlocks(text)}
    </pre>
  ) : (
    <div className="agent-sub">{LT.winNoOutput}</div>
  );
}

export default function WorkerWindow({ task, windowKey }: { task: string; windowKey: string }) {
  const label = useRef(crypto.randomUUID()).current;
  const keys = keysOf(windowKey);
  const groupIndex = windowKey.startsWith("group:") ? Number(windowKey.split(":")[1]) : null;
  const [snap, setSnap] = useState<WorkerSnapshot | null>(null);
  const [shown, setShown] = useState(keys[0]);
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
  useTauriEvent<{ taskId: string; key: string; delta: string }>("task:agent-delta", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => ({ ...s, streams: { ...s.streams, [p.key]: ((s.streams[p.key] ?? "") + p.delta).slice(-STREAM_KEEP) } }));
  });
  useTauriEvent<{ taskId: string; key: string; text: string }>("task:agent-activity", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => (s.runs[p.key] ? { ...s, runs: { ...s.runs, [p.key]: { ...s.runs[p.key], activities: [...s.runs[p.key].activities, p.text].slice(-200) } } } : s));
  });
  useTauriEvent<{ taskId: string; key: string; note: string }>("task:agent-wait", (p) => {
    if (p.taskId === task && keys.includes(p.key)) patch((s) => (s.runs[p.key] ? { ...s, runs: { ...s.runs, [p.key]: { ...s.runs[p.key], state: "run", wait: p.note } } } : s));
  });
  useTauriEvent<{ taskId: string; key: string; state: AgentRun["state"]; stopped?: boolean; error?: string | null; label: string; engine: AgentRun["engine"]; role: string }>("task:agent", (p) => {
    if (p.taskId === task && keys.includes(p.key))
      patch((s) => {
        const prev = s.runs[p.key];
        const run: AgentRun = { key: p.key, engine: p.engine, label: p.label, role: p.role, model: prev?.model ?? null, effort: prev?.effort ?? null, startedAt: prev?.startedAt ?? Date.now(), ms: prev?.ms ?? null, chars: prev?.chars ?? null, activities: prev?.activities ?? [], sent: prev?.sent ?? [], state: p.state, stopped: !!p.stopped, error: p.error ?? null, wait: null };
        return { ...s, runs: { ...s.runs, [p.key]: run } };
      });
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
    await api.leadSend(task, t, shown).catch((e) => {
      setMsg(t);
      setError(errorText(e));
    });
  };

  const run = snap?.runs[shown];
  const part = snap?.parts[shown];
  const text = snap?.streams[shown] ?? "";
  const decisions = snap?.decisions.filter((d) => d.key === shown) ?? [];
  return (
    <div className="worker-win">
      <ParticleOrb mode={run?.state === "run" ? "thinking" : "idle"} />
      <main className="worker-inner">
        {!snap ? (
          <div className="agent-sub">{LT.winConnecting}</div>
        ) : (
          <>
            <header className="lead-line worker-head">
              <i className={`agent-dot ${run?.state ?? ""}`} aria-hidden />
              <b>{part?.title ?? run?.label}</b>
              {run && <span className="agent-role">{[run.label, run.model, run.effort].filter(Boolean).join(" · ")}</span>}
              {run && <span className={`agent-state ${run.wait ? "wait" : ""}`}>{run.stopped ? LT.failed : runState(run)}</span>}
              <div className="spacer" />
              {run?.state === "run" && (
                <button className="btn sm danger" onClick={() => api.agentCancel(task, shown).catch((e) => setError(errorText(e)))}>
                  <StopIcon /> {LT.stop}
                </button>
              )}
            </header>
            {keys.length > 1 && (
              <nav className="worker-tabs">
                {keys.map((k, i) => (
                  <button key={k} className={`btn sm ${k === shown ? "primary" : "ghost"}`} onClick={() => setShown(k)}>
                    {LT.member} {i + 1}
                  </button>
                ))}
              </nav>
            )}
            {groupIndex !== null && (
              <section className="worker-sec">
                <h5>{LT.winTalk}</h5>
                {snap.groupLog.length === 0 ? (
                  <div className="agent-sub">{LT.noTalk}</div>
                ) : (
                  <ul className="lead-said worker-talk">
                    {snap.groupLog.map((g, i) => (
                      <li key={i}>
                        <b>{g.who}</b> {g.text}
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            )}
            <section className="worker-sec grow">
              <h5>{LT.winOutput}</h5>
              {text.length >= STREAM_KEEP && <div className="agent-sub">{LT.winCut}</div>}
              <Output text={text} />
            </section>
            {part && part.files.length > 0 && (
              <section className="worker-sec">
                <h5>{LT.winFiles}</h5>
                <div className="agent-files">
                  {part.files.map((f) => (
                    <code key={f}>{f}</code>
                  ))}
                </div>
              </section>
            )}
            {run && run.activities.length > 0 && (
              <section className="worker-sec">
                <h5>{LT.winActivity}</h5>
                <div className="worker-activity">
                  {run.activities.slice(-30).map((a, i) => (
                    <div key={i} className="agent-act full">
                      {a}
                    </div>
                  ))}
                </div>
              </section>
            )}
            {decisions.length > 0 && (
              <section className="worker-sec">
                <h5>{LT.decisions}</h5>
                <div className="worker-activity">
                  {decisions.slice(-10).map((d) => (
                    <div key={d.id} className="agent-act full">
                      {d.verdict} · {d.command}
                    </div>
                  ))}
                </div>
              </section>
            )}
            {error && <div className="agent-err">{error}</div>}
            <div className="agent-talk">
              <input type="text" value={msg} onChange={(e) => setMsg(e.target.value)} onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && (e.preventDefault(), send())} placeholder={LT.reportPlaceholder} aria-label={LT.report} />
              <button className="icon" onClick={send} disabled={!msg.trim()} aria-label={LT.report} title={LT.report}>
                <SendIcon />
              </button>
            </div>
          </>
        )}
      </main>
    </div>
  );
}

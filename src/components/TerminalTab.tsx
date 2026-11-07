import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import { useApp } from "../store";
import { CloseIcon, PlusIcon } from "./Icons";

export const T = {
  title: "PowerShell",
  tip: "PowerShell: 앱 안에서 명령 실행",
  restart: "다시 시작",
  clear: "지우기",
  interrupt: "멈춤 (Ctrl+C)",
  exited: "PowerShell이 종료되었습니다. 다시 시작을 누르세요.",
  failed: "PowerShell을 시작하지 못했습니다",
  add: "새 터미널",
  close: "터미널 닫기",
  split: "나란히 보기",
  tabs: "터미널 탭",
  full: "최대 {n}개까지만 열 수 있습니다.",
};

/** Replay buffer kept per terminal between view switches (characters). */
const KEEP = 400_000;
/** Shells running at once (each is a PowerShell process). */
const MAX_TERMS = 8;

interface Session {
  id: string;
  n: number;
  gen: number;
  buffer: string;
  running: boolean;
  started: boolean;
  sink: null | ((s: string) => void);
  onState: null | (() => void);
}

// Shells live for the whole app run. Views are recreated on every tab switch, so output is buffered here and replayed.
let seq = 0;
let sessions: Session[] = [];
let active: string | null = null;
let split = false;
const subs = new Set<() => void>();
let snapshot: { sessions: Session[]; active: string | null; split: boolean } = { sessions, active, split };
const changed = () => {
  snapshot = { sessions, active, split };
  subs.forEach((f) => f());
};
const byId = (id: string) => sessions.find((s) => s.id === id);

function add(): Session | null {
  if (sessions.length >= MAX_TERMS) return null;
  seq += 1;
  const s: Session = { id: `t${seq}`, n: seq, gen: 0, buffer: "", running: false, started: false, sink: null, onState: null };
  sessions = [...sessions, s];
  active = s.id;
  changed();
  return s;
}

function close(id: string) {
  invoke("term_close", { id }).catch(() => {});
  const i = sessions.findIndex((s) => s.id === id);
  sessions = sessions.filter((s) => s.id !== id);
  if (active === id) active = (sessions[i] ?? sessions[i - 1])?.id ?? null;
  changed();
}

let listening = false;
function push(s: Session, data: string) {
  s.buffer += data;
  if (s.buffer.length > KEEP) s.buffer = s.buffer.slice(s.buffer.length - KEEP);
  s.sink?.(data);
}

function ensureListen() {
  if (listening) return;
  listening = true;
  // Generations only grow, so a newer one replaces the old shell even if its first output beats term_open's reply.
  listen<{ id: string; gen: number; data: string }>("term:data", (e) => {
    const p = e.payload;
    const s = byId(p.id);
    if (!s || p.gen < s.gen) return;
    s.gen = p.gen;
    push(s, p.data);
  });
  listen<{ id: string; gen: number; code: number | null }>("term:exit", (e) => {
    const p = e.payload;
    const s = byId(p.id);
    if (!s || p.gen < s.gen) return;
    s.running = false;
    push(s, `\r\n\x1b[2m${T.exited}\x1b[0m\r\n`);
    s.onState?.();
    changed();
  });
}

async function start(s: Session, cols: number, rows: number, cwd: string | null) {
  s.buffer = "";
  s.started = true;
  s.running = true;
  s.onState?.();
  changed();
  try {
    const gen = await invoke<number>("term_open", { id: s.id, cols, rows, cwd });
    if (gen > s.gen) s.gen = gen;
  } catch (e) {
    s.running = false;
    push(s, `\x1b[31m${T.failed}: ${String(e)}\x1b[0m\r\n`);
    s.onState?.();
    changed();
  }
}

const write = (id: string, data: string) => invoke("term_write", { id, data }).catch(() => {});

const THEME = {
  background: "rgba(0, 0, 0, 0)",
  foreground: "#d8e6e2",
  cursor: "#9eead9",
  cursorAccent: "#071014",
  selectionBackground: "rgba(120, 203, 184, 0.32)",
  black: "#1b2026",
  red: "#e87870",
  green: "#78cbb8",
  yellow: "#e7c98b",
  blue: "#6cb6ff",
  magenta: "#b39ddb",
  cyan: "#9eead9",
  white: "#d8e6e2",
  brightBlack: "#6b737d",
  brightRed: "#f2958e",
  brightGreen: "#9eead9",
  brightYellow: "#f1dba8",
  brightBlue: "#a9d4ff",
  brightMagenta: "#cdbdf0",
  brightCyan: "#c4f2e7",
  brightWhite: "#ffffff",
};

const nameOf = (s: Session) => `${T.title} ${s.n}`;

/** One shell's screen: its own bar (state, folder, stop/clear/restart/close) and an xterm view. */
function TermView({ s, cwd, compact, focus }: { s: Session; cwd: string | null; compact: boolean; focus: boolean }) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<{ term: Terminal; fit: FitAddon } | null>(null);
  const [running, setRunning] = useState(s.running);
  const cwdRef = useRef(cwd);
  cwdRef.current = cwd;

  useEffect(() => {
    ensureListen();
    const el = host.current!;
    const term = new Terminal({
      fontFamily: '"JetBrains Mono Variable", "Cascadia Mono", Consolas, "Pretendard Variable", monospace',
      fontSize: 12.5,
      // 1이어야 박스·블록 문자(─│█▀)가 위아래로 틈 없이 이어진다.
      lineHeight: 1,
      letterSpacing: 0,
      cursorBlink: false,
      allowTransparency: true,
      scrollback: 5000,
      theme: THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    view.current = { term, fit };
    try {
      fit.fit();
    } catch {
      /* hidden or zero size; the observer fits again */
    }
    if (s.buffer) term.write(s.buffer);
    s.sink = (d) => term.write(d);
    s.onState = () => setRunning(s.running);
    setRunning(s.running);
    // Ctrl+C copies when text is selected, otherwise it goes to the shell as an interrupt.
    term.attachCustomKeyEventHandler((ev) => {
      if (ev.type === "keydown" && ev.ctrlKey && ev.key.toLowerCase() === "c" && term.hasSelection()) {
        navigator.clipboard?.writeText(term.getSelection()).catch(() => {});
        term.clearSelection();
        return false;
      }
      return true;
    });
    const sub = term.onData((d) => write(s.id, d));
    const pick = () => {
      if (active !== s.id) {
        active = s.id;
        changed();
      }
    };
    el.addEventListener("focusin", pick);
    let timer = 0;
    const ro = new ResizeObserver(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        try {
          fit.fit();
        } catch {
          return;
        }
        invoke("term_resize", { id: s.id, cols: term.cols, rows: term.rows }).catch(() => {});
      }, 60);
    });
    ro.observe(el);
    if (!s.started) start(s, term.cols, term.rows, cwdRef.current);
    if (focus) term.focus();
    return () => {
      window.clearTimeout(timer);
      ro.disconnect();
      sub.dispose();
      el.removeEventListener("focusin", pick);
      if (s.sink) s.sink = null;
      s.onState = null;
      term.dispose();
      view.current = null;
    };
    // One view per session; it is keyed by session id.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const restart = () => {
    const v = view.current;
    if (!v) return;
    v.term.reset();
    start(s, v.term.cols, v.term.rows, cwdRef.current);
    v.term.focus();
  };
  const clear = () => {
    s.buffer = "";
    view.current?.term.clear();
    view.current?.term.focus();
  };
  const interrupt = () => {
    write(s.id, "\x03");
    view.current?.term.focus();
  };

  return (
    <div className={`term-pane ${focus ? "on" : ""}`}>
      <div className="term-bar">
        <span className={`term-state ${running ? "on" : ""}`} aria-hidden />
        {compact && <b className="term-name">{nameOf(s)}</b>}
        <span className="term-cwd" title={cwd ?? undefined}>
          {cwd ?? "~"}
        </span>
        <div className="spacer" />
        <button className="btn sm ghost" onClick={interrupt} disabled={!running}>
          {T.interrupt}
        </button>
        <button className="btn sm ghost" onClick={clear}>
          {T.clear}
        </button>
        <button className="btn sm" onClick={restart}>
          {T.restart}
        </button>
        {compact && (
          <button className="icon" onClick={() => close(s.id)} aria-label={`${T.close} · ${nameOf(s)}`} title={T.close}>
            <CloseIcon />
          </button>
        )}
      </div>
      {/* App-wide shortcuts (Ctrl+B, Ctrl+F, Esc…) belong to the shell while typing here; Ctrl+J still closes the panel. */}
      <div
        className="term-box"
        ref={host}
        onKeyDown={(e) => {
          if (!((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "j")) e.stopPropagation();
        }}
      />
    </div>
  );
}

export default function TerminalTab() {
  const { conversations, conversationId, settings } = useApp();
  const st = useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => snapshot,
  );
  const cwd = conversations.find((c) => c.id === conversationId)?.workspaceDir ?? settings?.workspaceDir ?? null;
  // Always at least one shell when the panel opens.
  useEffect(() => {
    if (sessions.length === 0) add();
  }, [st.sessions.length]);
  const full = st.sessions.length >= MAX_TERMS;
  const shown = st.split ? st.sessions : st.sessions.filter((s) => s.id === st.active);

  return (
    <div className="tool-body term">
      <div className="term-tabs" role="tablist" aria-label={T.tabs}>
        {st.sessions.map((s) => (
          <div key={s.id} className={`term-tab ${s.id === st.active ? "on" : ""}`}>
            <button
              type="button"
              role="tab"
              aria-selected={s.id === st.active}
              onClick={() => {
                active = s.id;
                changed();
              }}
            >
              <span className={`term-state ${s.running ? "on" : ""}`} aria-hidden />
              {nameOf(s)}
            </button>
            <button type="button" className="icon term-tab-x" onClick={() => close(s.id)} aria-label={`${T.close} · ${nameOf(s)}`} title={T.close}>
              <CloseIcon />
            </button>
          </div>
        ))}
        <button type="button" className="btn sm ghost term-add" onClick={() => add()} disabled={full} title={full ? T.full.replace("{n}", String(MAX_TERMS)) : T.add}>
          <PlusIcon /> {T.add}
        </button>
        <div className="spacer" />
        {st.sessions.length > 1 && (
          <label className="term-split">
            <input
              type="checkbox"
              className="switch"
              role="switch"
              checked={st.split}
              onChange={(e) => {
                split = e.target.checked;
                changed();
              }}
            />
            {T.split}
          </label>
        )}
      </div>
      <div className={`term-grid ${st.split && shown.length > 1 ? "split" : ""}`} style={{ "--n": shown.length } as React.CSSProperties}>
        {shown.map((s) => (
          <TermView key={s.id} s={s} cwd={cwd} compact={st.split && shown.length > 1} focus={s.id === st.active} />
        ))}
      </div>
    </div>
  );
}

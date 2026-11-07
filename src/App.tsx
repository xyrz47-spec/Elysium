import { useCallback, useEffect, useRef, useState } from "react";
import { baseName } from "./lib/api";
import { useApp } from "./store";
import SessionPanel, { Dock } from "./components/SessionRail";
import ParticleOrb, { type OrbMode } from "./components/ParticleOrb";
import Starfield from "./components/Starfield";
import UsageDialog from "./components/UsageDialog";
import ConfirmDialog from "./components/ConfirmDialog";
import PowerBanner from "./components/PowerBanner";
import MemoryButton from "./components/MemoryButton";
import { A as AGENT_TEXT, AgentsTab, ToolDock, ToolPanel, type ToolTab } from "./components/Tools";
import Segmented from "./components/Segmented";
import { clampPanel, DOCK, PANEL_DEFAULT } from "./lib/panel";
import Thread from "./components/Thread";
import Composer from "./components/Composer";
import SettingsDialog from "./components/SettingsDialog";
import SkillsDialog from "./components/SkillsDialog";
import { FolderIcon } from "./components/Icons";
import SleepScreen from "./components/SleepScreen";
import { GlassDefs, useGlassLight, useLiquidRefraction } from "./lib/glass";

export type MainView = "chat" | "agents";

const VIEW_TEXT = {
  chat: "채팅",
  label: "화면 전환",
};

/** 세션 머리: 제목과 이 세션의 작업 폴더(눌러서 바꾸기) */
function SessionHead({ view, onView, running }: { view: MainView; onView: (v: MainView) => void; running: number }) {
  const { conversations, conversationId, chooseFolder, live } = useApp();
  const c = conversations.find((x) => x.id === conversationId);
  const busy = live.some((l) => l.conversationId === conversationId);
  return (
    <header className="session-head" key={conversationId ?? "new"}>
      <div className="head-title">
        {/* 대화 중에는 배경의 구가 이 자리로 줄어들어 AI 상태(대기·처리 중·허락 대기·오류)를 보여 준다 */}
        {c ? <span className="head-orb" data-orb-anchor="mini" aria-hidden /> : <span className={`head-dot ${busy ? "busy" : ""}`} aria-hidden />}
        <span className="head-title-text" title={c?.title}>
          {c ? c.title : "새 세션"}
        </span>
      </div>
      {c && (
        <button className="head-folder" onClick={() => chooseFolder(c.id)} title={`${c.workspaceDir ?? "기본 작업 폴더"} — 눌러서 이 세션의 폴더 바꾸기`}>
          <FolderIcon /> {c.workspaceDir ? baseName(c.workspaceDir) : "기본 폴더"}
        </button>
      )}
      {c && <MemoryButton conversationId={c.id} />}
      <div className="spacer" />
      <Segmented
        label={VIEW_TEXT.label}
        className="view-switch"
        value={view}
        onChange={onView}
        options={[
          { value: "chat", label: VIEW_TEXT.chat },
          { value: "agents", label: <>{AGENT_TEXT.title}{running > 0 && <span className="dock-count inline">{running}</span>}</> },
        ]}
      />
    </header>
  );
}

const PANEL_KEY = "elysium.panel";
const TOOLS_WIDTH_KEY = "elysium.toolsWidth";

function loadToolsWidth(): number {
  try {
    const v = localStorage.getItem(TOOLS_WIDTH_KEY);
    return v ? Number(v) : PANEL_DEFAULT;
  } catch {
    return PANEL_DEFAULT;
  }
}

/** 도구 창 폭: 사용자가 정한 값을 기억하고, 창이 좁으면 대화가 남도록 줄여서 쓴다. */
function useToolsWidth() {
  const [stored, setStored] = useState(loadToolsWidth);
  const [windowWidth, setWindowWidth] = useState(window.innerWidth);
  const [dragging, setDragging] = useState(false);
  useEffect(() => {
    const on = () => setWindowWidth(window.innerWidth);
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, []);
  const save = (w: number) => {
    const next = clampPanel(w, window.innerWidth);
    setStored(next);
    try {
      localStorage.setItem(TOOLS_WIDTH_KEY, String(next));
    } catch {
      /* 저장 못 해도 이번 실행에서는 동작한다 */
    }
  };
  const onResizeStart = (e: React.PointerEvent) => {
    e.preventDefault();
    setDragging(true);
    const move = (m: PointerEvent) => save(window.innerWidth - m.clientX - DOCK);
    const up = () => {
      setDragging(false);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  const onResizeKey = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowLeft") save(clampPanel(stored, windowWidth) + 24);
    else if (e.key === "ArrowRight") save(clampPanel(stored, windowWidth) - 24);
  };
  return { width: clampPanel(stored, windowWidth), dragging, onResizeStart, onResizeKey };
}

/** 세션 패널은 본문 위에 떠 있다. 본문과 겹치지 않을 만큼 넓은 창에서만 기본으로 펼친다. */
const WIDE = 1560;

function loadPanel(): boolean {
  try {
    const v = localStorage.getItem(PANEL_KEY);
    return v ? v === "open" : window.innerWidth >= WIDE;
  } catch {
    return window.innerWidth >= WIDE;
  }
}

export default function App() {
  const { dialog, setDialog, toasts, openConversation, engines, live, approvals, conversationId } = useApp();
  useGlassLight();
  useLiquidRefraction();
  const [draft, setDraft] = useState<string | null>(null);
  const clearDraft = useCallback(() => setDraft(null), []);
  const [panel, setPanel] = useState(loadPanel);
  const togglePanel = useCallback(() => {
    setPanel((p) => {
      try {
        localStorage.setItem(PANEL_KEY, p ? "closed" : "open");
      } catch {
        /* 저장 못 해도 이번 실행에서는 동작한다 */
      }
      return !p;
    });
  }, []);
  // 오른쪽 도구 패널(세션 정보·에이전트·곁질문·목차·찾기·내보내기)
  const [tool, setTool] = useState<ToolTab | null>(null);
  const lastTool = useRef<ToolTab>("info");
  const toolsWidth = useToolsWidth();
  if (tool) lastTool.current = tool;
  // Main area: chat or the agent board. A multi-agent task in this session brings the board to the front once.
  const [view, setView] = useState<MainView>("chat");
  const agentTask = live.find((l) => l.conversationId === conversationId && (l.lead || Object.keys(l.agents).length >= 2));
  const autoFor = useRef<string | null>(null);
  useEffect(() => {
    if (agentTask && autoFor.current !== agentTask.taskId) {
      autoFor.current = agentTask.taskId;
      setView("agents");
    }
  }, [agentTask?.taskId]);
  useEffect(() => {
    if (!agentTask) setView("chat");
    // Only when the session changes; staying on the board after the task ends is fine.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId]);
  const running = live.reduce((n, l) => n + Object.values(l.agents).filter((a) => a.state === "run").length, 0);
  const pickView = setView;
  // 구의 상태: 허락 대기 > 처리 중 > 대기, 쓸 AI가 없으면 꺼짐
  const orb: OrbMode = approvals.length ? "approval" : live.length ? "thinking" : engines.some((e) => e.installed) ? "idle" : "off";

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
        e.preventDefault();
        openConversation(null);
      } else if (e.key === "Escape" && dialog) {
        setDialog(null);
      } else if (e.key === "Escape" && tool) {
        setTool(null);
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "j") {
        e.preventDefault();
        setTool((t) => (t ? null : lastTool.current));
      } else if ((e.ctrlKey || e.metaKey) && e.key === "/") {
        e.preventDefault();
        setDialog(dialog === "keys" ? null : "keys");
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "b") {
        e.preventDefault();
        togglePanel();
      } else if ((e.ctrlKey || e.metaKey) && e.key === ",") {
        e.preventDefault();
        setDialog("settings");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [dialog, openConversation, setDialog, togglePanel, tool]);

  return (
    <div className={`app ${panel ? "panel-open" : ""} ${tool ? "tools-open" : ""} ${toolsWidth.dragging ? "tools-dragging" : ""}`} style={{ "--tools-w": `${toolsWidth.width}px` } as React.CSSProperties}>
      <GlassDefs />
      <Starfield />
      <ParticleOrb mode={orb} />
      <Dock panelOpen={panel} onTogglePanel={togglePanel} />
      <SessionPanel
        open={panel}
        onPicked={() => {
          // 좁은 창에서는 세션을 고르면 패널을 닫아 가운데 본문을 가리지 않는다.
          if (window.innerWidth < WIDE) setPanel(false);
        }}
      />
      <ToolDock tab={tool} onTab={setTool} />
      <ToolPanel tab={tool} onTab={setTool} onResizeStart={toolsWidth.onResizeStart} onResizeKey={toolsWidth.onResizeKey} />
      <main className="main">
        <SessionHead view={view} onView={pickView} running={running} />
        {view === "agents" ? (
          <div className="agents-main">
            <AgentsTab main onClose={() => {}} />
          </div>
        ) : (
          <Thread onPick={setDraft} />
        )}
        <Composer draft={draft} onDraftUsed={clearDraft} />
      </main>
      {(dialog === "settings" || dialog === "keys") && <SettingsDialog focusKeys={dialog === "keys"} />}
      {dialog === "skills" && <SkillsDialog />}
      {dialog === "usage" && <UsageDialog />}
      <ConfirmDialog />
      <PowerBanner />
      <SleepScreen />
      <div className="toasts" role="status">
        {toasts.map((t) => (
          <div key={t.id} className={`toast ${t.kind}`}>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}

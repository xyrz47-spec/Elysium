import { useCallback, useEffect, useRef, useState } from "react";
import { baseName } from "./lib/api";
import { useApp } from "./store";
import SessionPanel, { Dock } from "./components/SessionRail";
import ParticleOrb, { type OrbMode } from "./components/ParticleOrb";
import UsageDialog from "./components/UsageDialog";
import ConfirmDialog from "./components/ConfirmDialog";
import MemoryButton from "./components/MemoryButton";
import { ToolDock, ToolPanel, type ToolTab } from "./components/Tools";
import Thread from "./components/Thread";
import Composer from "./components/Composer";
import SettingsDialog from "./components/SettingsDialog";
import SkillsDialog from "./components/SkillsDialog";
import { FolderIcon } from "./components/Icons";

/** 세션 머리: 제목과 이 세션의 작업 폴더(눌러서 바꾸기) */
function SessionHead() {
  const { conversations, conversationId, chooseFolder, engines, live } = useApp();
  const c = conversations.find((x) => x.id === conversationId);
  const busy = live.some((l) => l.conversationId === conversationId);
  return (
    <header className="session-head" key={conversationId ?? "new"}>
      <div className="head-title">
        <span className={`head-dot ${busy ? "busy" : ""}`} aria-hidden />
        {c ? c.title : "새 세션"}
      </div>
      {c && (
        <button className="head-folder" onClick={() => chooseFolder(c.id)} title={`${c.workspaceDir ?? "기본 작업 폴더"} — 눌러서 이 세션의 폴더 바꾸기`}>
          <FolderIcon /> {c.workspaceDir ? baseName(c.workspaceDir) : "기본 폴더"}
        </button>
      )}
      {c && <MemoryButton conversationId={c.id} />}
      <div className="spacer" />
      <div className="head-engines" title="쓸 수 있는 AI">
        {engines
          .filter((e) => e.installed)
          .map((e) => (
            <span key={e.id} className={`mini-eng ${e.kind}`}>
              {e.label}
            </span>
          ))}
      </div>
    </header>
  );
}

const PANEL_KEY = "elysium.panel";

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
  const { dialog, setDialog, toasts, openConversation, engines, live, approvals } = useApp();
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
  // 오른쪽 도구 패널(세션 정보·목차·찾기·빠른 문구·내보내기·단축키)
  const [tool, setTool] = useState<ToolTab | null>(null);
  const [findKey, setFindKey] = useState(0);
  const lastTool = useRef<ToolTab>("info");
  if (tool) lastTool.current = tool;
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
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setTool("find");
        setFindKey((k) => k + 1);
      } else if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "j") {
        e.preventDefault();
        setTool((t) => (t ? null : lastTool.current));
      } else if ((e.ctrlKey || e.metaKey) && e.key === "/") {
        e.preventDefault();
        setTool((t) => (t === "keys" ? null : "keys"));
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
    <div className={`app ${panel ? "panel-open" : ""}`}>
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
      <ToolPanel
        tab={tool}
        onTab={setTool}
        focusKey={findKey}
        onPick={(t) => {
          setDraft(t);
          // 좁은 창에서는 문구를 넣으면 패널을 닫아 입력창을 가리지 않는다.
          if (window.innerWidth < WIDE) setTool(null);
        }}
      />
      <main className="main">
        <SessionHead />
        <Thread onPick={setDraft} />
        <Composer draft={draft} onDraftUsed={clearDraft} />
      </main>
      {dialog === "settings" && <SettingsDialog />}
      {dialog === "skills" && <SkillsDialog />}
      {dialog === "usage" && <UsageDialog />}
      <ConfirmDialog />
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

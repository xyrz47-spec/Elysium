import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { applyTheme, loadTheme, watchSystem, type ThemeChoice } from "../lib/theme";
import { baseName } from "../lib/api";
import { ask } from "../lib/confirm";
import { useApp } from "../store";
import { AutoIcon, ChartIcon, FolderIcon, GearIcon, MoonIcon, PanelIcon, PlusIcon, PuzzleIcon, SunIcon, TrashIcon } from "./Icons";

const THEME_NEXT: Record<ThemeChoice, ThemeChoice> = { system: "dark", dark: "light", light: "system" };
const THEME_LABEL: Record<ThemeChoice, string> = { system: "시스템 따라가기", dark: "다크 모드", light: "화이트 모드" };

function ThemeButton() {
  const [t, setT] = useState<ThemeChoice>(loadTheme);
  const ref = useRef(t);
  ref.current = t;
  useEffect(() => watchSystem(() => ref.current), []);
  return (
    <button
      className="dock-btn"
      onClick={() => {
        const n = THEME_NEXT[t];
        setT(n);
        applyTheme(n);
      }}
      aria-label={`테마: ${THEME_LABEL[t]}`}
      data-tip={`${THEME_LABEL[t]} (누르면 바뀜)`}
    >
      <span className="dock-swap" key={t}>
        {t === "light" ? <SunIcon /> : t === "dark" ? <MoonIcon /> : <AutoIcon />}
      </span>
    </button>
  );
}

function ago(iso: string): string {
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 60) return "방금";
  if (s < 3600) return `${Math.floor(s / 60)}분 전`;
  if (s < 86400) return `${Math.floor(s / 3600)}시간 전`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)}일 전`;
  return new Date(iso).toLocaleDateString("ko-KR", { month: "short", day: "numeric" });
}

/** 왼쪽 도크: 로고, 새 세션, 세션 패널 열고 닫기, 사용량, 스킬, 테마, 설정 */
export function Dock({ panelOpen, onTogglePanel }: { panelOpen: boolean; onTogglePanel: () => void }) {
  const { openConversation, setDialog, engines, live } = useApp();
  const ready = engines.filter((e) => e.installed).length;
  return (
    <nav className="dock" aria-label="엘리시움">
      <div className={`dock-logo ${live.length ? "busy" : ""}`} title="Elysium" aria-hidden>
        <img src="/logo.svg" alt="" draggable={false} />
      </div>
      <button className="dock-btn primary" onClick={() => openConversation(null)} aria-label="새 세션" data-tip="새 세션 (Ctrl+N)">
        <PlusIcon />
      </button>
      <button className={`dock-btn ${panelOpen ? "on" : ""}`} onClick={onTogglePanel} aria-label="세션 목록" aria-pressed={panelOpen} data-tip={panelOpen ? "세션 목록 접기" : "세션 목록 펼치기"}>
        <PanelIcon />
      </button>
      <div className="dock-gap" />
      <button className="dock-btn" onClick={() => setDialog("usage")} aria-label="사용량" data-tip="CLI·API별 사용량">
        <ChartIcon />
      </button>
      <button className="dock-btn" onClick={() => setDialog("skills")} aria-label="스킬과 플러그인" data-tip="스킬 · 플러그인">
        <PuzzleIcon />
      </button>
      <ThemeButton />
      <button className="dock-btn" onClick={() => setDialog("settings")} aria-label="설정" data-tip="설정 (Ctrl+,)">
        <GearIcon />
        <span className="dock-count" title="쓸 수 있는 AI 수">
          {ready}
        </span>
      </button>
    </nav>
  );
}

/** 세션 패널: 세션마다 카드 한 장. 카드에 그 세션의 작업 폴더가 보인다. */
export default function SessionPanel({ open, onPicked }: { open: boolean; onPicked: () => void }) {
  const { conversations, conversationId, openConversation, deleteConversation, live } = useApp();
  const [q, setQ] = useState("");
  const busy = new Set(live.map((l) => l.conversationId));
  const list = useMemo(() => conversations.filter((c) => !q || c.title.toLowerCase().includes(q.toLowerCase()) || (c.workspaceDir ?? "").toLowerCase().includes(q.toLowerCase())), [conversations, q]);

  return (
    <aside className={`panel ${open ? "open" : ""}`} aria-label="세션" aria-hidden={!open} inert={!open}>
      <div className="panel-inner">
        <div className="panel-head">
          <span className="panel-brand">
            <img src="/logo.svg" alt="" aria-hidden draggable={false} />
            <span className="panel-word">Elysium</span>
          </span>
          <span className="panel-count">{conversations.length}개 세션</span>
        </div>
        <input type="text" className="rail-search" placeholder="세션·폴더 검색" value={q} onChange={(e) => setQ(e.target.value)} aria-label="세션 검색" />
        <nav className="sessions">
          {list.length === 0 && <div className="rail-empty">{q ? "찾는 세션이 없습니다" : "첫 세션을 시작해 보세요"}</div>}
          {list.map((c, i) => (
            <div
              key={c.id}
              className={`session ${c.id === conversationId ? "on" : ""} ${busy.has(c.id) ? "busy" : ""}`}
              style={{ "--i": i } as CSSProperties}
              onClick={() => {
                openConversation(c.id);
                onPicked();
              }}
              role="button"
              tabIndex={0}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  openConversation(c.id);
                  onPicked();
                }
              }}
            >
              <div className="session-title">{c.title}</div>
              <div className="session-meta">
                <span className="session-folder" title={c.workspaceDir ?? "기본 작업 폴더"}>
                  <FolderIcon /> {c.workspaceDir ? baseName(c.workspaceDir) : "기본 폴더"}
                </span>
                <span>{busy.has(c.id) ? "작업 중" : ago(c.updatedAt)}</span>
              </div>
              <button
                className="session-del"
                aria-label="세션 삭제"
                title="삭제"
                onClick={async (e) => {
                  e.stopPropagation();
                  const ok = await ask({
                    title: `'${c.title}' 세션을 삭제할까요?`,
                    body: "이 세션의 대화, 답, 세션 메모리(요약), 과정 기록이 모두 지워지고 되돌릴 수 없습니다. 작업 폴더의 파일은 지우지 않습니다.",
                    confirmLabel: "영구 삭제",
                    danger: true,
                  });
                  if (ok) deleteConversation(c.id);
                }}
              >
                <TrashIcon />
              </button>
            </div>
          ))}
        </nav>
      </div>
    </aside>
  );
}

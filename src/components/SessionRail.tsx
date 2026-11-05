import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { applyTheme, loadTheme, watchSystem, type ThemeChoice } from "../lib/theme";
import { api, baseName, type Conversation } from "../lib/api";
import { ask } from "../lib/confirm";
import { useApp } from "../store";
import { AutoIcon, ChartIcon, CloseIcon, EditIcon, FolderIcon, GearIcon, MoonIcon, PanelIcon, PinIcon, PlusIcon, PuzzleIcon, SleepIcon, SunIcon, TrashIcon } from "./Icons";
import { setSleep } from "../lib/sleep";

const SLEEP_TIP = "취침 모드: 화면 끄고 AI 작업 유지";

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
      <button className="dock-btn" onClick={() => setSleep(true)} aria-label={SLEEP_TIP} data-tip={SLEEP_TIP}>
        <SleepIcon />
      </button>
      <button className="dock-btn" onClick={() => setDialog("settings")} aria-label="설정" data-tip="설정 (Ctrl+,)">
        <GearIcon />
        <span className="dock-count" title="쓸 수 있는 AI 수">
          {ready}
        </span>
      </button>
    </nav>
  );
}

const DEL_LABEL = "세션 삭제";
const DEL_TIP = "삭제";

/** 우클릭 메뉴: 누른 자리에 뜨고, 바깥을 누르거나 Esc·스크롤·창 크기 변경이면 닫힌다. */
function ContextMenu({ x, y, label, onClose, children }: { x: number; y: number; label: string; onClose: () => void; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    // 화면 밖으로 넘치지 않게 맞춘다.
    const r = el.getBoundingClientRect();
    setPos({ left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)), top: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)) });
    el.querySelector<HTMLElement>("[role=menuitem]")?.focus();
  }, [x, y]);
  useEffect(() => {
    const down = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("pointerdown", down, true);
    window.addEventListener("keydown", key, true);
    window.addEventListener("resize", onClose);
    window.addEventListener("blur", onClose);
    document.addEventListener("scroll", onClose, true);
    return () => {
      window.removeEventListener("pointerdown", down, true);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", onClose);
      window.removeEventListener("blur", onClose);
      document.removeEventListener("scroll", onClose, true);
    };
  }, [onClose]);
  // 패널의 유리 효과 안에서는 fixed가 패널 기준이 되므로 body에 그린다.
  return createPortal(
    <div ref={ref} className="ctx-menu" role="menu" aria-label={label} style={pos} onContextMenu={(e) => e.preventDefault()}>
      {children}
    </div>,
    document.body,
  );
}

const PIN_KEY = "elysium.pinned";
/** 내용까지 찾기 시작하는 글자 수와 기다리는 시간 */
const DEEP_MIN = 2;
const DEEP_DELAY_MS = 250;

/** 고정한 세션(이 PC에만 기억한다) */
function usePins(): [Set<string>, (id: string) => void] {
  const [pins, setPins] = useState<Set<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem(PIN_KEY) ?? "[]") as string[]);
    } catch {
      return new Set();
    }
  });
  const toggle = (id: string) =>
    setPins((cur) => {
      const next = new Set(cur);
      if (!next.delete(id)) next.add(id);
      localStorage.setItem(PIN_KEY, JSON.stringify([...next]));
      return next;
    });
  return [pins, toggle];
}

/** 제목·폴더에 더해 대화 내용에서도 찾는다. 늦게 온 답은 버린다. */
function useDeepSearch(q: string): Map<string, string> {
  const [hits, setHits] = useState<Map<string, string>>(new Map());
  useEffect(() => {
    const t = q.trim();
    if (t.length < DEEP_MIN) {
      setHits(new Map());
      return;
    }
    let stale = false;
    const timer = setTimeout(() => {
      api
        .conversationSearch(t)
        .then((list) => !stale && setHits(new Map(list.map((h) => [h.conversationId, h.snippet]))))
        .catch(() => {});
    }, DEEP_DELAY_MS);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [q]);
  return hits;
}

/** 세션 패널: 세션마다 카드 한 장. 고정한 세션이 위에 오고, 찾기는 대화 내용까지 본다. */
export default function SessionPanel({ open, onPicked }: { open: boolean; onPicked: () => void }) {
  const { conversations, conversationId, openConversation, deleteConversation, renameConversation, live } = useApp();
  const [q, setQ] = useState("");
  const [pins, togglePin] = usePins();
  const [editing, setEditing] = useState<string | null>(null);
  const deep = useDeepSearch(q);
  const busy = new Set(live.map((l) => l.conversationId));
  const list = useMemo(() => {
    const t = q.trim().toLowerCase();
    const found = conversations.filter((c) => !t || c.title.toLowerCase().includes(t) || (c.workspaceDir ?? "").toLowerCase().includes(t) || deep.has(c.id));
    return [...found.filter((c) => pins.has(c.id)), ...found.filter((c) => !pins.has(c.id))];
  }, [conversations, q, deep, pins]);
  const [menu, setMenu] = useState<{ c: Conversation; x: number; y: number } | null>(null);
  const remove = async (c: Conversation) => {
    const ok = await ask({
      title: `'${c.title}' 세션을 삭제할까요?`,
      body: "이 세션의 대화, 답, 세션 메모리(요약), 과정 기록이 모두 지워지고 되돌릴 수 없습니다. 작업 폴더의 파일은 지우지 않습니다.",
      confirmLabel: "영구 삭제",
      danger: true,
    });
    if (ok) deleteConversation(c.id);
  };
  const pick = (c: Conversation) => {
    openConversation(c.id);
    onPicked();
  };
  const firstUnpinned = list.findIndex((c) => !pins.has(c.id));

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
        <div className="rail-search-wrap">
          <input
            type="text"
            className="rail-search"
            placeholder="제목·폴더·대화 내용 검색"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && q && (e.stopPropagation(), setQ(""))}
            aria-label="세션 검색"
          />
          {q && (
            <button type="button" className="rail-search-x" onClick={() => setQ("")} aria-label="검색어 지우기" title="검색어 지우기">
              <CloseIcon />
            </button>
          )}
        </div>
        <nav className="sessions">
          {list.length === 0 && <div className="rail-empty">{q ? "찾는 세션이 없습니다" : "첫 세션을 시작해 보세요"}</div>}
          {list.map((c, i) => {
            const pinned = pins.has(c.id);
            const snippet = q ? deep.get(c.id) : undefined;
            return (
              <div key={c.id} className="session-slot">
                {i === 0 && pinned && <div className="rail-label">고정됨</div>}
                {i === firstUnpinned && firstUnpinned > 0 && <div className="rail-label">최근</div>}
                <div
                  className={`session ${c.id === conversationId ? "on" : ""} ${busy.has(c.id) ? "busy" : ""} ${pinned ? "pinned" : ""}`}
                  style={{ "--i": i } as CSSProperties}
                  onClick={() => editing !== c.id && pick(c)}
                  onDoubleClick={() => setEditing(c.id)}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    setMenu({ c, x: e.clientX, y: e.clientY });
                  }}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => {
                    if (editing === c.id) return;
                    if (e.key === "Enter") pick(c);
                    else if (e.key === "F2") setEditing(c.id);
                    else if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
                      e.preventDefault();
                      const r = e.currentTarget.getBoundingClientRect();
                      setMenu({ c, x: r.left + 16, y: r.top + r.height / 2 });
                    }
                  }}
                >
                  {editing === c.id ? (
                    <RenameBox
                      title={c.title}
                      onDone={(t) => {
                        setEditing(null);
                        if (t && t !== c.title) renameConversation(c.id, t);
                      }}
                    />
                  ) : (
                    <div className="session-title" title={c.title}>
                      {pinned && <PinIcon />}
                      {c.title}
                    </div>
                  )}
                  {snippet && <div className="session-snippet">{snippet}</div>}
                  <div className="session-meta">
                    <span className="session-folder" title={c.workspaceDir ?? "기본 작업 폴더"}>
                      <FolderIcon /> {c.workspaceDir ? baseName(c.workspaceDir) : "기본 폴더"}
                    </span>
                    <span>{busy.has(c.id) ? "작업 중" : ago(c.updatedAt)}</span>
                  </div>
                  <button
                    className="session-del"
                    aria-label={DEL_LABEL}
                    title={DEL_TIP}
                    onClick={(e) => {
                      e.stopPropagation();
                      remove(c);
                    }}
                  >
                    <TrashIcon />
                  </button>
                </div>
              </div>
            );
          })}
        </nav>
      </div>
      {menu && (
        <ContextMenu x={menu.x} y={menu.y} label={menu.c.title} onClose={() => setMenu(null)}>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              togglePin(menu.c.id);
              setMenu(null);
            }}
          >
            <PinIcon /> {pins.has(menu.c.id) ? "고정 해제" : "맨 위에 고정"}
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setEditing(menu.c.id);
              setMenu(null);
            }}
          >
            <EditIcon /> 이름 바꾸기 <kbd>F2</kbd>
          </button>
          <button
            type="button"
            role="menuitem"
            className="danger"
            onClick={() => {
              const c = menu.c;
              setMenu(null);
              remove(c);
            }}
          >
            <TrashIcon /> {DEL_TIP}
          </button>
        </ContextMenu>
      )}
    </aside>
  );
}

/** 세션 이름 바꾸기: Enter·바깥 누르기로 저장, Esc로 취소 */
function RenameBox({ title, onDone }: { title: string; onDone: (t: string | null) => void }) {
  const [v, setV] = useState(title);
  const done = useRef(false);
  const finish = (t: string | null) => {
    if (done.current) return;
    done.current = true;
    onDone(t);
  };
  return (
    <input
      className="session-rename"
      autoFocus
      value={v}
      maxLength={80}
      aria-label="세션 이름"
      onFocus={(e) => e.currentTarget.select()}
      onChange={(e) => setV(e.target.value)}
      onClick={(e) => e.stopPropagation()}
      onBlur={() => finish(v.trim() || null)}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter" && !e.nativeEvent.isComposing) finish(v.trim() || null);
        else if (e.key === "Escape") finish(null);
      }}
    />
  );
}

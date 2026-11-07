// 에이전트가 많아도 누가 무엇을 하는지 한눈에 보는 판: 상태별 개수(누르면 거르기)·찾기·상태 지도와,
// 사용자가 실행 중에 에이전트를 끌어다 묶는 그룹(폴더). 그룹은 보기만 바꾸며 실행에는 영향이 없다.
import { createContext, Fragment, useCallback, useContext, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import type { AgentRun, Live } from "../store";
import { groupOps, useAllAgentGroups, type AgentGroup } from "../lib/agentGroups";
import { shownState, type Shown } from "../lib/progress";
import { ChevronIcon, CloseIcon, EditIcon, FolderIcon, GripIcon, PlusIcon, SearchIcon } from "./Icons";
import Segmented from "./Segmented";

export const B = {
  all: "전체 {n}",
  run: "작업 중 {n}",
  wait: "대기 {n}",
  done: "완료 {n}",
  fail: "실패 {n}",
  filter: "상태별 에이전트 필터",
  search: "이름, 파일, 작업 검색",
  clear: "검색어 지우기",
  map: "에이전트 상태 현황판",
  mapHint: "클릭하여 바로 이동하고 드래그해 그룹에 추가합니다.",
  newGroup: "새 그룹",
  groupN: "그룹 {n}",
  rename: "이름 변경",
  ungroup: "그룹 삭제",
  dropNew: "여기에 놓아 새 그룹 생성",
  dropOut: "여기에 놓아 그룹에서 제외",
  moveTo: "그룹으로 이동",
  outOf: "그룹에서 제외",
  emptyGroup: "에이전트를 여기로 드래그하거나 폴더 버튼으로 이동하세요.",
  noMatch: "일치하는 에이전트가 없습니다.",
  drag: "드래그하여 그룹으로 이동",
  count: "{n}개",
  layout: "보기 방식",
  cards: "카드",
  list: "목록",
};

type Layout = "cards" | "list";
const LAYOUT_KEY = "elysium.agentLayout";
/** With this many agents the board starts as a list, unless the user picked a layout. */
const LIST_AUTO = 10;

function loadLayout(): Layout | null {
  try {
    const v = localStorage.getItem(LAYOUT_KEY);
    return v === "cards" || v === "list" ? v : null;
  } catch {
    return null;
  }
}

const STATE: Record<Shown, string> = { working: "작업 중", waiting: "대기 중", idle: "메시지 대기 중", ok: "완료", fail: "중단됨" };

type Bucket = "all" | "run" | "wait" | "done" | "fail";
const BUCKETS: Bucket[] = ["all", "run", "wait", "done", "fail"];
const BUCKET: Record<Shown, Exclude<Bucket, "all">> = { working: "run", waiting: "wait", idle: "done", ok: "done", fail: "fail" };
/** 에이전트가 이보다 적으면 판(거르기·지도·그룹)을 띄우지 않는다. */
const BOARD_MIN = 4;

interface Drag {
  keys: string[];
  label: string;
  x: number;
  y: number;
  over: string | null;
}

interface Board {
  task: string;
  groups: AgentGroup[];
  groupOf: Map<string, AgentGroup>;
  passes: (key: string) => boolean;
  num: (key: string) => number;
  startDrag: (e: ReactPointerEvent, keys: string[], label: string) => void;
  focus: { key: string; at: number } | null;
}

const BoardCtx = createContext<Board | null>(null);
/** 그룹 칸 안에서 그리는 중인지 */
const InFolder = createContext(false);

const nextName = (groups: AgentGroup[]) => B.groupN.replace("{n}", String(groups.length + 1));

/** 이 자리에 에이전트를 그릴지 묻는 함수: 거르기에 맞고, 그룹에 든 에이전트는 그룹 칸에서만 그린다. 판이 없으면 늘 그린다. */
export function useShownCheck(): (key: string) => boolean {
  const b = useContext(BoardCtx);
  const inFolder = useContext(InFolder);
  return (key) => !b || (b.passes(key) && (inFolder || !b.groupOf.has(key)));
}

export function useAgentShown(key: string): boolean {
  return useShownCheck()(key);
}

/** 상태 지도에서 고른 에이전트면 보이는 곳으로 옮기고 펼친 뒤 잠깐 빛나게 한다. */
export function useBoardFocus(key: string, ref: RefObject<HTMLElement | null>, open: () => void) {
  const focus = useContext(BoardCtx)?.focus;
  const at = focus?.key === key ? focus.at : 0;
  useEffect(() => {
    const el = ref.current;
    if (!at || !el) return;
    open();
    el.scrollIntoView({ block: "nearest", behavior: "smooth" });
    el.classList.add("flash");
    const t = window.setTimeout(() => el.classList.remove("flash"), 1400);
    return () => window.clearTimeout(t);
    // 고른 순간에만 돈다
  }, [at]);
}

/** 끌어서 그룹으로 옮기는 손잡이. 에이전트 하나면 번호를, 여럿(팀)이면 점 무늬를 보인다. */
export function Grip({ keys, label }: { keys: string[]; label: string }) {
  const b = useContext(BoardCtx);
  if (!b || !keys.length) return null;
  const one = keys.length === 1;
  return (
    <span className={`aline-grip ${one ? "num" : ""}`} title={B.drag} aria-hidden onPointerDown={(e) => b.startDrag(e, keys, label)}>
      {one ? b.num(keys[0]) : <GripIcon />}
    </span>
  );
}

/** 그룹으로 옮기기 메뉴(키보드로도 쓸 수 있게). 패널 유리 효과 안에서 잘리지 않게 body에 띄운다. */
export function GroupMenu({ keys, label }: { keys: string[]; label: string }) {
  const b = useContext(BoardCtx);
  const [at, setAt] = useState<{ top: number; left: number } | null>(null);
  const btn = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!at) return;
    const off = (e: PointerEvent) => {
      if (!list.current?.contains(e.target as Node) && !btn.current?.contains(e.target as Node)) setAt(null);
    };
    const key = (e: KeyboardEvent) => e.key === "Escape" && setAt(null);
    const close = () => setAt(null);
    window.addEventListener("pointerdown", off, true);
    window.addEventListener("keydown", key);
    window.addEventListener("scroll", close, true);
    list.current?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => {
      window.removeEventListener("pointerdown", off, true);
      window.removeEventListener("keydown", key);
      window.removeEventListener("scroll", close, true);
    };
  }, [at]);
  if (!b) return null;
  const cur = keys.every((k) => b.groupOf.get(k)?.id === b.groupOf.get(keys[0])?.id) ? (b.groupOf.get(keys[0])?.id ?? null) : null;
  const inAny = keys.some((k) => b.groupOf.has(k));
  const pick = (id: string | null) => {
    groupOps.assign(b.task, keys, id);
    setAt(null);
  };
  const toggle = () => {
    if (at) return setAt(null);
    const r = btn.current!.getBoundingClientRect();
    setAt({ top: r.bottom + 4, left: Math.max(8, Math.min(r.right - 200, window.innerWidth - 208)) });
  };
  return (
    <>
      <button ref={btn} type="button" className={`icon gmenu-btn ${inAny ? "on" : ""}`} style={cur ? ({ "--hue": b.groupOf.get(keys[0])!.hue } as CSSProperties) : undefined} aria-haspopup="menu" aria-expanded={!!at} title={B.moveTo} aria-label={`${B.moveTo} · ${label}`} onClick={toggle}>
        <FolderIcon />
      </button>
      {at &&
        createPortal(
          <div ref={list} className="gmenu-list" role="menu" aria-label={B.moveTo} style={at}>
            {b.groups.map((g) => (
              <button key={g.id} type="button" role="menuitemradio" aria-checked={g.id === cur} className={g.id === cur ? "on" : ""} style={{ "--hue": g.hue } as CSSProperties} onClick={() => pick(g.id)}>
                <i className="gdot" aria-hidden />
                <span>{g.name}</span>
              </button>
            ))}
            <button
              type="button"
              role="menuitem"
              onClick={() => {
                groupOps.create(b.task, nextName(b.groups), keys);
                setAt(null);
              }}
            >
              <PlusIcon />
              <span>{B.newGroup}</span>
            </button>
            {inAny && (
              <button type="button" role="menuitem" onClick={() => pick(null)}>
                <CloseIcon />
                <span>{B.outOf}</span>
              </button>
            )}
          </div>,
          document.body,
        )}
    </>
  );
}

/** 에이전트 판. children은 그룹에 들지 않은 에이전트를 원래 구조(조직도·팀·워커)대로 그린다. row는 그룹 칸 안에서 에이전트 하나를 그린다. */
export function AgentBoard({ l, runs, name, now, row, children }: { l: Live; runs: AgentRun[]; name: (a: AgentRun) => string; now: (a: AgentRun) => string; row: (a: AgentRun) => ReactNode; children: ReactNode }) {
  const all = useAllAgentGroups();
  const task = l.taskId;
  const groups = useMemo(() => all.filter((g) => g.task === task), [all, task]);
  const [bucket, setBucket] = useState<Bucket>("all");
  const [query, setQuery] = useState("");
  const [focus, setFocus] = useState<Board["focus"]>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [picked, setPicked] = useState<Layout | null>(loadLayout);
  const pickLayout = (v: Layout) => {
    setPicked(v);
    try {
      localStorage.setItem(LAYOUT_KEY, v);
    } catch {
      /* not saved: this run only */
    }
  };
  const groupsRef = useRef(groups);
  groupsRef.current = groups;

  const byKey = useMemo(() => new Map(runs.map((a) => [a.key, a])), [runs]);
  const groupOf = useMemo(() => {
    const m = new Map<string, AgentGroup>();
    for (const g of groups) for (const k of g.keys) if (byKey.has(k)) m.set(k, g);
    return m;
  }, [groups, byKey]);
  const order = useMemo(() => new Map(runs.map((a, i) => [a.key, i + 1])), [runs]);
  const q = query.trim().toLowerCase();
  const passes = (key: string) => {
    const a = byKey.get(key);
    if (!a) return false;
    if (bucket !== "all" && BUCKET[shownState(a)] !== bucket) return false;
    if (!q) return true;
    return [name(a), a.label, a.model, a.effort, now(a), ...(l.parts[a.key]?.files ?? [])].filter(Boolean).join(" ").toLowerCase().includes(q);
  };

  const startDrag = useCallback(
    (e: ReactPointerEvent, keys: string[], label: string) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const x0 = e.clientX;
      const y0 = e.clientY;
      let moving = false;
      const target = (x: number, y: number) => document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-drop]")?.dataset.drop ?? null;
      const move = (ev: PointerEvent) => {
        if (!moving && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return;
        if (!moving) document.body.classList.add("agent-dragging");
        moving = true;
        setDrag({ keys, label, x: ev.clientX, y: ev.clientY, over: target(ev.clientX, ev.clientY) });
      };
      const end = (ev: PointerEvent, drop: boolean) => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        window.removeEventListener("pointercancel", cancel);
        document.body.classList.remove("agent-dragging");
        setDrag(null);
        if (!moving) return;
        // 끌기 끝에 따라오는 클릭이 카드를 펼치지 않게 한 번만 막는다.
        const swallow = (c: MouseEvent) => {
          c.stopPropagation();
          c.preventDefault();
        };
        window.addEventListener("click", swallow, true);
        window.setTimeout(() => window.removeEventListener("click", swallow, true), 0);
        if (!drop) return;
        const t = target(ev.clientX, ev.clientY);
        if (t === "new") groupOps.create(task, nextName(groupsRef.current), keys);
        else if (t === "none") groupOps.assign(task, keys, null);
        else if (t) groupOps.assign(task, keys, t);
      };
      const up = (ev: PointerEvent) => end(ev, true);
      const cancel = (ev: PointerEvent) => end(ev, false);
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
      window.addEventListener("pointercancel", cancel);
    },
    [task],
  );

  if (runs.length < BOARD_MIN) return <>{children}</>;

  const counts: Record<Bucket, number> = { all: runs.length, run: 0, wait: 0, done: 0, fail: 0 };
  for (const a of runs) counts[BUCKET[shownState(a)]]++;
  const visible = runs.filter((a) => passes(a.key)).length;
  const reveal = (key: string) => {
    setBucket("all");
    setQuery("");
    const g = groupOf.get(key);
    if (g && !g.open) groupOps.toggle(g.id, true);
    setFocus({ key, at: Date.now() });
  };
  const board: Board = { task, groups, groupOf, passes, num: (k) => order.get(k) ?? 0, startDrag, focus };
  const fromGroup = !!drag?.keys.some((k) => groupOf.has(k));
  const layout: Layout = picked ?? (runs.length >= LIST_AUTO ? "list" : "cards");
  return (
    <BoardCtx.Provider value={board}>
      <div className={`aboard layout-${layout}`}>
        <div className="aboard-bar">
          <div className="aboard-filters" role="radiogroup" aria-label={B.filter}>
            {BUCKETS.filter((x) => x === "all" || counts[x] > 0 || bucket === x).map((x) => (
              <button key={x} type="button" role="radio" aria-checked={bucket === x} className={`achip b-${x} ${bucket === x ? "on" : ""}`} onClick={() => setBucket(bucket === x ? "all" : x)}>
                {B[x].replace("{n}", String(counts[x]))}
              </button>
            ))}
          </div>
          <div className="aboard-tools">
            <label className="aboard-search">
              <SearchIcon />
              <input type="text" value={query} onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => e.key === "Escape" && setQuery("")} placeholder={B.search} aria-label={B.search} />
              {query && (
                <button type="button" className="icon" onClick={() => setQuery("")} aria-label={B.clear} title={B.clear}>
                  <CloseIcon />
                </button>
              )}
            </label>
            <Segmented
              label={B.layout}
              className="aboard-layout"
              value={layout}
              onChange={pickLayout}
              options={[
                { value: "cards", label: B.cards },
                { value: "list", label: B.list },
              ]}
            />
            <button type="button" className="btn sm ghost" onClick={() => groupOps.create(task, nextName(groups))}>
              <PlusIcon /> {B.newGroup}
            </button>
          </div>
        </div>
        <div className="amap" role="group" aria-label={B.map} title={B.mapHint}>
          {runs.map((a, i) => {
            const st = shownState(a);
            const g = groupOf.get(a.key);
            const tip = [`#${i + 1} ${name(a)}`, STATE[st], now(a), g?.name].filter(Boolean).join("\n");
            return (
              <button
                key={a.key}
                type="button"
                className={`amap-tile st-${st} ${passes(a.key) ? "" : "dim"} ${g ? "in" : ""}`}
                style={g ? ({ "--hue": g.hue } as CSSProperties) : undefined}
                title={tip}
                aria-label={`#${i + 1} ${name(a)} · ${STATE[st]}`}
                onPointerDown={(e) => startDrag(e, [a.key], name(a))}
                onClick={() => reveal(a.key)}
              >
                {i + 1}
              </button>
            );
          })}
        </div>
        {drag && (
          <div className="aboard-drops">
            <div className={`adrop ${drag.over === "new" ? "over" : ""}`} data-drop="new">
              <PlusIcon /> {B.dropNew}
            </div>
            {fromGroup && (
              <div className={`adrop ${drag.over === "none" ? "over" : ""}`} data-drop="none">
                <CloseIcon /> {B.dropOut}
              </div>
            )}
          </div>
        )}
        {groups.map((g) => (
          <Folder key={g.id} g={g} runs={g.keys.map((k) => byKey.get(k)).filter((a): a is AgentRun => !!a)} row={row} over={drag?.over === g.id} />
        ))}
        <div className="aboard-rest" data-drop="none">
          {children}
        </div>
        {visible === 0 && <div className="agent-sub">{B.noMatch}</div>}
      </div>
      {drag &&
        createPortal(
          <div className="adrag-ghost" style={{ left: drag.x + 14, top: drag.y + 10 }}>
            <FolderIcon /> {drag.label}
            {drag.keys.length > 1 && <span className="agent-count">{B.count.replace("{n}", String(drag.keys.length))}</span>}
          </div>,
          document.body,
        )}
    </BoardCtx.Provider>
  );
}

/** 사용자가 만든 그룹 하나: 이름·개수·상태 점, 펼치면 안의 에이전트 */
function Folder({ g, runs, row, over }: { g: AgentGroup; runs: AgentRun[]; row: (a: AgentRun) => ReactNode; over: boolean }) {
  const b = useContext(BoardCtx)!;
  const [editing, setEditing] = useState(false);
  const keep = useRef(true);
  const shown = runs.filter((a) => b.passes(a.key));
  // 거르는 중이면 맞는 에이전트가 없는 그룹은 숨긴다(빈 그룹은 그대로 보인다).
  if (runs.length > 0 && shown.length === 0) return null;
  return (
    <section className={`afolder ${g.open ? "open" : ""} ${over ? "over" : ""}`} data-drop={g.id} style={{ "--hue": g.hue } as CSSProperties} aria-label={g.name}>
      <div className="afolder-head">
        <Grip keys={runs.map((a) => a.key)} label={g.name} />
        {editing ? (
          <input
            type="text"
            className="afolder-name"
            autoFocus
            defaultValue={g.name}
            aria-label={B.rename}
            onFocus={(e) => {
              keep.current = true;
              e.target.select();
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.nativeEvent.isComposing) e.currentTarget.blur();
              if (e.key === "Escape") {
                keep.current = false;
                e.currentTarget.blur();
              }
            }}
            onBlur={(e) => {
              if (keep.current) groupOps.rename(g.id, e.target.value);
              setEditing(false);
            }}
          />
        ) : (
          <button type="button" className="afolder-toggle" onClick={() => groupOps.toggle(g.id)} onDoubleClick={() => setEditing(true)} aria-expanded={g.open}>
            <FolderIcon />
            <b title={g.name}>{g.name}</b>
          </button>
        )}
        <span className="agent-count">{runs.length}</span>
        <span className="org-dots" aria-hidden>
          {runs.map((a) => (
            <i key={a.key} className={`agent-dot st-${shownState(a)}`} />
          ))}
        </span>
        <div className="spacer" />
        <button type="button" className="icon" onClick={() => setEditing(true)} title={B.rename} aria-label={`${B.rename} · ${g.name}`}>
          <EditIcon />
        </button>
        <button type="button" className="icon" onClick={() => groupOps.remove(g.id)} title={B.ungroup} aria-label={`${B.ungroup} · ${g.name}`}>
          <CloseIcon />
        </button>
        <button type="button" className="icon card-fold" onClick={() => groupOps.toggle(g.id)} aria-expanded={g.open} aria-label={g.name}>
          <ChevronIcon open={g.open} />
        </button>
      </div>
      {g.open &&
        (runs.length === 0 ? (
          <div className="afolder-empty">{B.emptyGroup}</div>
        ) : (
          <InFolder.Provider value={true}>
            <div className="lead-workers">
              {shown.map((a) => (
                <Fragment key={a.key}>{row(a)}</Fragment>
              ))}
            </div>
          </InFolder.Provider>
        ))}
    </section>
  );
}

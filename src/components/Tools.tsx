import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { api, baseName, ENGINE_LABEL, errorText, type EngineId, type MemoryView, type Message } from "../lib/api";
import { ask } from "../lib/confirm";
import { useTauriEvent } from "../lib/events";
import { useApp } from "../store";
import { ArrowDownIcon, ArrowUpIcon, BookmarkIcon, CloseIcon, DownloadIcon, FolderIcon, InfoIcon, KeyboardIcon, ListIcon, PlusIcon, SearchIcon, TrashIcon } from "./Icons";
import { pair } from "./Thread";

export type ToolTab = "info" | "outline" | "find" | "snippets" | "export" | "keys";

const TABS: { id: ToolTab; label: string; icon: ReactNode; tip: string }[] = [
  { id: "info", label: "세션 정보", icon: <InfoIcon />, tip: "세션 정보: 폴더·AI·토큰·비용·기억" },
  { id: "outline", label: "목차", icon: <ListIcon />, tip: "목차: 질문으로 바로 이동" },
  { id: "find", label: "찾기", icon: <SearchIcon />, tip: "세션에서 찾기 (Ctrl+F)" },
  { id: "snippets", label: "빠른 문구", icon: <BookmarkIcon />, tip: "빠른 문구: 자주 쓰는 요청 넣기" },
  { id: "export", label: "내보내기", icon: <DownloadIcon />, tip: "마크다운으로 복사·저장" },
  { id: "keys", label: "단축키", icon: <KeyboardIcon />, tip: "단축키 (Ctrl+/)" },
];

const label = (id: unknown) => (typeof id === "string" ? (ENGINE_LABEL[id as EngineId] ?? id) : null);
const answeredBy = (m: Message | null) => (m?.data ? label(m.data.judgedBy) ?? label(m.data.reviewedBy) ?? label(m.data.draftedBy) : null);

/** 본문의 그 카드로 부드럽게 옮기고 잠깐 빛나게 한다. */
function jumpTo(key: string) {
  const el = document.getElementById(`ex-${key}`);
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: "start" });
  el.classList.remove("flash");
  void el.offsetWidth;
  el.classList.add("flash");
}

function scrollThread(to: "top" | "bottom") {
  const t = document.querySelector<HTMLElement>(".thread");
  t?.scrollTo({ top: to === "top" ? 0 : t.scrollHeight, behavior: "smooth" });
}

function tokens(n: number) {
  return n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n);
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="tool-empty">{children}</p>;
}

// ---------------------------------------------------------------------------

function InfoTab() {
  const { conversations, conversationId, messages, chooseFolder, toast } = useApp();
  const [mem, setMem] = useState<MemoryView | null>(null);
  const c = conversations.find((x) => x.id === conversationId);
  const load = () => {
    if (conversationId) api.memoryGet(conversationId).then(setMem).catch(() => setMem(null));
  };
  useEffect(load, [conversationId, messages.length]);
  useTauriEvent<{ conversationId: string }>("memory:updated", (p) => p.conversationId === conversationId && load());

  const stats = useMemo(() => {
    const by = new Map<string, { label: string; calls: number; tokens: number; cost: number; roles: Set<string> }>();
    let tokensAll = 0;
    let cost = 0;
    let ms = 0;
    for (const m of messages) {
      for (const s of m.data?.steps ?? []) {
        const t = (s.inputTokens ?? 0) + (s.outputTokens ?? 0);
        const e = by.get(s.engine) ?? { label: s.label, calls: 0, tokens: 0, cost: 0, roles: new Set<string>() };
        e.calls += 1;
        e.tokens += t;
        e.cost += s.costUsd ?? 0;
        e.roles.add(s.role);
        by.set(s.engine, e);
        tokensAll += t;
        cost += s.costUsd ?? 0;
        ms += s.ms;
      }
      cost += m.data?.jev?.costUsd ?? 0;
    }
    return { engines: [...by.values()].sort((a, b) => b.calls - a.calls), tokensAll, cost, ms };
  }, [messages]);

  if (!c) return <Empty>세션을 열면 폴더, 쓰인 AI, 토큰과 비용, 세션 기억이 여기에 보입니다.</Empty>;
  const asks = messages.filter((m) => m.role === "user").length;
  const ROLE: Record<string, string> = { draft: "초안", review: "검토", debate: "토론", rebuttal: "반론", judge: "판정" };
  return (
    <div className="tool-body">
      <section className="tool-sec">
        <h4>작업 폴더</h4>
        <div className="tool-folder" title={c.workspaceDir ?? "기본 작업 폴더"}>
          <FolderIcon /> <span>{c.workspaceDir ? baseName(c.workspaceDir) : "기본 작업 폴더"}</span>
        </div>
        <div className="row">
          <button className="btn sm" onClick={() => api.openPath(`session:${c.id}`).catch((e) => toast(errorText(e), "error"))}>
            폴더 열기
          </button>
          <button className="btn sm ghost" onClick={() => chooseFolder(c.id)}>
            바꾸기
          </button>
        </div>
      </section>
      <section className="tool-sec">
        <h4>이 세션</h4>
        <div className="tool-stats">
          <div>
            <b>{asks}</b>
            <span>질문</span>
          </div>
          <div>
            <b>{tokens(stats.tokensAll)}</b>
            <span>토큰</span>
          </div>
          <div>
            <b>{stats.cost ? `$${stats.cost < 0.01 ? stats.cost.toFixed(4) : stats.cost.toFixed(2)}` : "$0"}</b>
            <span>추정 비용</span>
          </div>
          <div>
            <b>{stats.ms >= 60000 ? `${(stats.ms / 60000).toFixed(1)}분` : `${Math.round(stats.ms / 1000)}초`}</b>
            <span>AI 시간</span>
          </div>
        </div>
        {stats.engines.length > 0 && (
          <ul className="tool-engines">
            {stats.engines.map((e) => (
              <li key={e.label}>
                <b>{e.label}</b>
                <span>
                  {e.calls}회 · {[...e.roles].map((r) => ROLE[r] ?? r).join("/")}
                  {e.tokens > 0 && ` · ${tokens(e.tokens)}`}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="tool-sec">
        <h4>세션 기억</h4>
        {mem?.summary ? <div className="tool-mem">{mem.summary}</div> : <Empty>아직 요약이 없습니다. 대화가 길어지면 오래된 부분을 요약해 모든 AI가 같이 씁니다.</Empty>}
      </section>
    </div>
  );
}

function OutlineTab() {
  const { messages, conversationId } = useApp();
  const ex = pair(messages).filter((e) => e.user);
  if (!conversationId || ex.length === 0) return <Empty>질문을 보내면 여기에 목차가 생깁니다. 누르면 그 답으로 이동합니다.</Empty>;
  return (
    <div className="tool-body">
      <div className="row tool-jump">
        <button className="btn sm ghost" onClick={() => scrollThread("top")}>
          <ArrowUpIcon /> 맨 위
        </button>
        <button className="btn sm ghost" onClick={() => scrollThread("bottom")}>
          <ArrowDownIcon /> 맨 아래
        </button>
      </div>
      <ol className="tool-outline">
        {ex.map((e, i) => (
          <li key={e.key} style={{ "--i": i } as CSSProperties}>
            <button onClick={() => jumpTo(e.key)}>
              <span className="tool-num">{i + 1}</span>
              <span className="tool-q">{e.user!.content}</span>
              <span className="tool-by">{e.answer ? answeredBy(e.answer) ?? "답함" : "답 기다리는 중"}</span>
            </button>
          </li>
        ))}
      </ol>
    </div>
  );
}

function highlight(text: string, q: string): ReactNode {
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return text.slice(0, 90);
  const start = Math.max(0, i - 30);
  return (
    <>
      {start > 0 && "…"}
      {text.slice(start, i)}
      <mark>{text.slice(i, i + q.length)}</mark>
      {text.slice(i + q.length, i + q.length + 60)}…
    </>
  );
}

function FindTab({ focusKey }: { focusKey: number }) {
  const { messages, conversationId } = useApp();
  const [q, setQ] = useState("");
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), [focusKey]);
  const ex = pair(messages);
  const hits = q.trim().length < 1 ? [] : ex.flatMap((e) => [e.user, e.answer].filter((m): m is Message => !!m && m.content.toLowerCase().includes(q.trim().toLowerCase())).map((m) => ({ key: e.key, m })));
  return (
    <div className="tool-body">
      <div className="tool-search">
        <SearchIcon />
        <input ref={input} type="text" value={q} onChange={(e) => setQ(e.target.value)} placeholder="이 세션에서 찾기" aria-label="세션에서 찾기" onKeyDown={(e) => e.key === "Enter" && hits[0] && jumpTo(hits[0].key)} />
      </div>
      {!conversationId ? (
        <Empty>세션을 열면 그 세션의 질문과 답에서 찾습니다.</Empty>
      ) : q.trim() === "" ? (
        <Empty>찾을 낱말을 입력하세요. Enter를 누르면 첫 결과로 이동합니다.</Empty>
      ) : hits.length === 0 ? (
        <Empty>"{q}"가 이 세션에 없습니다.</Empty>
      ) : (
        <>
          <p className="tool-count">{hits.length}곳</p>
          <ul className="tool-hits">
            {hits.map((h, i) => (
              <li key={`${h.m.id}-${i}`}>
                <button onClick={() => jumpTo(h.key)}>
                  <span className="tool-by">{h.m.role === "user" ? "질문" : `답${answeredBy(h.m) ? ` · ${answeredBy(h.m)}` : ""}`}</span>
                  <span className="tool-snip">{highlight(h.m.content, q.trim())}</span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

interface Snippet {
  id: string;
  title: string;
  text: string;
}

const SNIP_KEY = "elysium.snippets";
const SNIP_DEFAULT: Snippet[] = [
  { id: "d1", title: "핵심만 요약", text: "위 내용을 핵심만 짧게 요약해 줘." },
  { id: "d2", title: "사실 확인", text: "/fact-check " },
  { id: "d3", title: "원인 찾기", text: "/root-cause " },
  { id: "d4", title: "성능 개선", text: "/perf-tune " },
  { id: "d5", title: "화면 만들기", text: "/ui-craft " },
];

function loadSnippets(): Snippet[] {
  try {
    const v = localStorage.getItem(SNIP_KEY);
    return v ? (JSON.parse(v) as Snippet[]) : SNIP_DEFAULT;
  } catch {
    return SNIP_DEFAULT;
  }
}

function SnippetsTab({ onPick }: { onPick: (t: string) => void }) {
  const [list, setList] = useState<Snippet[]>(loadSnippets);
  const [title, setTitle] = useState("");
  const [text, setText] = useState("");
  const save = (l: Snippet[]) => {
    setList(l);
    try {
      localStorage.setItem(SNIP_KEY, JSON.stringify(l));
    } catch {
      /* 저장 못 해도 이번 실행에서는 쓸 수 있다 */
    }
  };
  return (
    <div className="tool-body">
      <ul className="tool-snips">
        {list.map((s, i) => (
          <li key={s.id} style={{ "--i": i } as CSSProperties}>
            <button className="tool-snip-use" onClick={() => onPick(s.text)} title="입력창에 넣기">
              <b>{s.title}</b>
              <span>{s.text}</span>
            </button>
            <button
              className="tool-x"
              aria-label={`'${s.title}' 지우기`}
              onClick={async () => {
                if (await ask({ title: `'${s.title}' 문구를 지울까요?`, body: "빠른 문구 목록에서만 지워집니다.", confirmLabel: "지우기", danger: true })) save(list.filter((x) => x.id !== s.id));
              }}
            >
              <TrashIcon />
            </button>
          </li>
        ))}
      </ul>
      <form
        className="tool-add"
        onSubmit={(e) => {
          e.preventDefault();
          if (!text.trim()) return;
          save([...list, { id: `${Date.now()}`, title: title.trim() || text.trim().slice(0, 16), text: text.trim() + (text.trim().startsWith("/") ? " " : "") }]);
          setTitle("");
          setText("");
        }}
      >
        <input type="text" value={title} onChange={(e) => setTitle(e.target.value)} placeholder="이름(선택)" aria-label="문구 이름" />
        <textarea value={text} onChange={(e) => setText(e.target.value)} placeholder="자주 쓰는 요청 (예: /report 또는 문장)" rows={2} aria-label="문구 내용" />
        <button className="btn sm" type="submit" disabled={!text.trim()}>
          <PlusIcon /> 문구 추가
        </button>
      </form>
    </div>
  );
}

function ExportTab() {
  const { conversationId, conversations, toast } = useApp();
  const c = conversations.find((x) => x.id === conversationId);
  const [busy, setBusy] = useState(false);
  if (!c) return <Empty>세션을 열면 그 세션의 질문과 답을 마크다운으로 복사하거나 파일로 저장할 수 있습니다.</Empty>;
  return (
    <div className="tool-body">
      <p className="tool-help">질문, 답(누가 답했는지), 세션 요약을 마크다운으로 만듭니다.</p>
      <button
        className="btn wide"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            await navigator.clipboard.writeText(await api.conversationMarkdown(c.id));
            toast("마크다운을 복사했습니다.");
          } catch (e) {
            toast(errorText(e), "error");
          } finally {
            setBusy(false);
          }
        }}
      >
        마크다운으로 복사
      </button>
      <button
        className="btn wide primary"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            const p = await api.conversationExport(c.id);
            toast(`저장했습니다: ${baseName(p)}`);
          } catch (e) {
            toast(errorText(e), "error");
          } finally {
            setBusy(false);
          }
        }}
      >
        작업 폴더에 .md로 저장
      </button>
      <p className="tool-help">저장 위치: 이 세션의 작업 폴더 안 "Elysium 내보내기". 저장하면 탐색기에서 파일을 보여 줍니다.</p>
    </div>
  );
}

const KEYS: [string, string][] = [
  ["Ctrl + N", "새 세션"],
  ["Ctrl + B", "세션 목록 펼치기·접기"],
  ["Ctrl + J", "도구 패널 열기·닫기"],
  ["Ctrl + F", "세션에서 찾기"],
  ["Ctrl + /", "단축키 보기"],
  ["Ctrl + ,", "설정"],
  ["Enter", "보내기"],
  ["Shift + Enter", "줄 바꿈"],
  ["/", "스킬 고르기(입력창 맨 앞)"],
  ["Esc", "창·패널 닫기"],
];

function KeysTab() {
  return (
    <div className="tool-body">
      <dl className="tool-keys">
        {KEYS.map(([k, v]) => (
          <div key={k}>
            <dt>
              {k.split(" + ").map((p, i) => (
                <span key={p}>
                  {i > 0 && " + "}
                  <kbd>{p}</kbd>
                </span>
              ))}
            </dt>
            <dd>{v}</dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

// ---------------------------------------------------------------------------

/** 오른쪽 도구 도크(왼쪽 도크와 대칭) */
export function ToolDock({ tab, onTab }: { tab: ToolTab | null; onTab: (t: ToolTab | null) => void }) {
  return (
    <nav className="dock tools-dock" aria-label="도구">
      {TABS.map((t) => (
        <button key={t.id} className={`dock-btn ${tab === t.id ? "on" : ""}`} onClick={() => onTab(tab === t.id ? null : t.id)} aria-label={t.label} aria-pressed={tab === t.id} data-tip={t.tip}>
          {t.icon}
        </button>
      ))}
    </nav>
  );
}

/** 오른쪽 도구 패널: 본문을 밀지 않고 위에 떠 있다. */
export function ToolPanel({ tab, onTab, onPick, focusKey }: { tab: ToolTab | null; onTab: (t: ToolTab | null) => void; onPick: (t: string) => void; focusKey: number }) {
  const [shown, setShown] = useState<ToolTab>(tab ?? "info");
  useEffect(() => {
    if (tab) setShown(tab);
  }, [tab]);
  const t = TABS.find((x) => x.id === shown)!;
  return (
    <aside className={`tools-panel ${tab ? "open" : ""}`} aria-label={t.label} aria-hidden={!tab} inert={!tab}>
      <div className="tools-inner" key={shown}>
        <div className="tools-head">
          <span className="tools-title">
            {t.icon} {t.label}
          </span>
          <button className="icon" onClick={() => onTab(null)} aria-label="도구 패널 닫기">
            <CloseIcon />
          </button>
        </div>
        {shown === "info" && <InfoTab />}
        {shown === "outline" && <OutlineTab />}
        {shown === "find" && <FindTab focusKey={focusKey} />}
        {shown === "snippets" && <SnippetsTab onPick={onPick} />}
        {shown === "export" && <ExportTab />}
        {shown === "keys" && <KeysTab />}
      </div>
    </aside>
  );
}

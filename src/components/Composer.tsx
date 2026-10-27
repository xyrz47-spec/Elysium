import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { baseName, type EngineChoice } from "../lib/api";
import { useApp } from "../store";
import { ChatIcon, ChevronIcon, CloseIcon, FolderIcon, SendIcon, SparkIcon, StopIcon, WrenchIcon } from "./Icons";
import Segmented from "./Segmented";

/** 대화창 아래 입력 상자: 엔진 고르기·대화/작업 모드·/스킬 자동 완성 */
export default function Composer({ draft, onDraftUsed }: { draft?: string | null; onDraftUsed?: () => void }) {
  const { send, engines, live, conversationId, cancel, skills, conversations, newFolder, chooseFolder, resetFolder } = useApp();
  const [text, setText] = useState("");
  const [engine, setEngine] = useState<EngineChoice>("auto");
  const [work, setWork] = useState(false);
  const [menu, setMenu] = useState(false);
  const [pick, setPick] = useState(0);
  const ref = useRef<HTMLTextAreaElement>(null);

  const running = conversationId ? live.find((l) => l.conversationId === conversationId) : undefined;
  const installed = engines.filter((e) => e.installed);
  // 이 세션(또는 새 세션)의 폴더
  const folder = conversationId ? (conversations.find((c) => c.id === conversationId)?.workspaceDir ?? null) : newFolder;

  useEffect(() => ref.current?.focus(), [conversationId]);
  useEffect(() => {
    if (draft) {
      // 이미 쓴 글은 지우지 않는다: 스킬(/…)은 앞에, 일반 문구는 다음 줄에 붙인다.
      setText((cur) => {
        const c = cur.trim();
        if (!c || c === draft.trim()) return draft;
        if (draft.startsWith("/")) return c.startsWith("/") ? `${draft.trimEnd()} ${c.replace(/^\/\S+\s*/, "")}` : `${draft.trimEnd()} ${c}`;
        return `${cur.trimEnd()}\n${draft}`;
      });
      onDraftUsed?.();
      ref.current?.focus();
    }
  }, [draft, onDraftUsed]);
  // 입력에 맞춰 높이를 늘린다.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  }, [text]);

  // "/"로 시작하고 아직 띄어쓰기 전이면 스킬 목록을 보여 준다.
  const slash = /^\/(\S*)$/.exec(text)?.[1];
  const matches = useMemo(() => {
    if (slash === undefined || !skills) return [];
    const q = slash.toLowerCase();
    return skills.skills.filter((s) => s.enabled && (s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q))).slice(0, 7);
  }, [slash, skills]);
  useEffect(() => setPick(0), [slash]);

  async function submit() {
    const t = text.trim();
    if (!t || running) return;
    setText("");
    await send(t, engine, work);
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (matches.length > 0) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setPick((i) => (i + (e.key === "ArrowDown" ? 1 : matches.length - 1)) % matches.length);
        return;
      }
      if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) {
        e.preventDefault();
        setText(`/${matches[pick].name} `);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    } else if (e.key === "Escape" && running) {
      e.preventDefault();
      cancel(running.taskId);
    }
  }

  const engineLabel = engine === "auto" ? "자동" : (engines.find((e) => e.id === engine)?.label ?? engine);
  return (
    <div className="composer-wrap">
      {matches.length > 0 && (
        <div className="slash-menu" role="listbox" aria-label="스킬">
          {matches.map((s, i) => (
            <button key={s.id} role="option" aria-selected={i === pick} className={i === pick ? "on" : ""} onMouseEnter={() => setPick(i)} onClick={() => setText(`/${s.name} `)}>
              <SparkIcon size={15} />
              <b>/{s.name}</b>
              <span>{s.description}</span>
              {s.source !== "user" && <em>{s.source}</em>}
            </button>
          ))}
        </div>
      )}
      <div className={`composer ${work ? "work" : ""}`}>
        <textarea
          ref={ref}
          value={text}
          rows={1}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
          placeholder={installed.length ? "무엇이든 물어보세요 — / 로 스킬 사용" : "먼저 설정에서 AI CLI 연결을 확인하세요"}
          aria-label="메시지 입력"
        />
        <div className="composer-bar">
          <div className="pill-menu">
            <button className="pill" onClick={() => setMenu((m) => !m)} aria-haspopup="menu" aria-expanded={menu} title="답할 AI">
              {engineLabel} <ChevronIcon open={menu} />
            </button>
            {menu && (
              <div className="menu" role="menu" onMouseLeave={() => setMenu(false)}>
                <button role="menuitemradio" aria-checked={engine === "auto"} className={engine === "auto" ? "on" : ""} onClick={() => (setEngine("auto"), setMenu(false))}>
                  <b>자동</b>
                  <span>요청에 맞는 AI가 답하고, 복잡하면 다른 AI가 검토</span>
                </button>
                {(["cli", "api"] as const).map((k) => (
                  <div key={k} className="menu-group">
                    <div className="menu-title">{k === "cli" ? "CLI · 파일 작업 가능" : "API 키"}</div>
                    {engines
                      .filter((e) => e.kind === k)
                      .map((e) => (
                        <button
                          key={e.id}
                          role="menuitemradio"
                          aria-checked={engine === e.id}
                          disabled={!e.installed || (work && k === "api")}
                          className={engine === e.id ? "on" : ""}
                          onClick={() => (setEngine(e.id), setMenu(false))}
                        >
                          <b>{e.label}</b>
                          <span>{e.installed ? `${e.version ?? "사용 가능"}${e.verified ? "" : " · 미검증"}` : k === "cli" ? "설치 안 됨" : "키 없음"}</span>
                        </button>
                      ))}
                  </div>
                ))}
              </div>
            )}
          </div>
          <Segmented
            label="모드"
            className={`mode ${work ? "is-work" : ""}`}
            value={work ? "work" : "chat"}
            options={[
              { value: "chat", label: <><ChatIcon /> 대화</>, title: "읽기 전용: 파일을 고치지 않습니다" },
              { value: "work", label: <><WrenchIcon /> 작업</>, title: "작업 폴더 안의 파일을 고칠 수 있습니다(실행 전에 허락을 받습니다)" },
            ]}
            onChange={(v) => {
              setWork(v === "work");
              // API로는 파일을 고칠 수 없으므로 자동으로 되돌린다.
              if (v === "work" && engines.find((x) => x.id === engine)?.kind === "api") setEngine("auto");
            }}
          />
          <div className="folder-chip">
            <button className="pill" onClick={() => chooseFolder(conversationId)} title={folder ?? "기본 작업 폴더(설정에서 변경) — 눌러서 이 세션의 폴더 고르기"}>
              <FolderIcon /> {folder ? baseName(folder) : "기본 폴더"}
            </button>
            {folder && (
              <button className="pill-x" onClick={() => resetFolder(conversationId)} aria-label="기본 폴더로" title="기본 폴더로 되돌리기">
                <CloseIcon />
              </button>
            )}
          </div>
          <div className="spacer" />
          {running ? (
            <button className="send stop" onClick={() => cancel(running.taskId)} aria-label="중지" title="중지 (Esc)">
              <StopIcon />
            </button>
          ) : (
            <button className="send" onClick={submit} disabled={!text.trim() || installed.length === 0} aria-label="보내기" title="보내기 (Enter)">
              <SendIcon />
            </button>
          )}
        </div>
      </div>
      <div className="composer-note">AI는 실수할 수 있습니다. 중요한 내용은 확인하세요.</div>
    </div>
  );
}

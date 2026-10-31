import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { api, baseName, errorText, type EngineChoice } from "../lib/api";
import { useApp } from "../store";
import { ChatIcon, ChevronIcon, CloseIcon, CouncilIcon, FolderIcon, SendIcon, SparkIcon, StopIcon, WrenchIcon } from "./Icons";
import Segmented from "./Segmented";

/** 대화창 아래 입력 상자: 대화/작업 모드 · 보내기 · 만들기(폴더 안에서 끝까지 완성) · 엔진 고르기 · /스킬 자동 완성 */
export default function Composer({ draft, onDraftUsed }: { draft?: string | null; onDraftUsed?: () => void }) {
  const { send, engines, live, conversationId, cancel, skills, conversations, newFolder, chooseFolder, resetFolder, toast } = useApp();
  const [text, setText] = useState("");
  const [engine, setEngine] = useState<EngineChoice>("auto");
  // 작업 모드: 보내기(Enter)로도 파일을 고칠 수 있다(실행 전에 허락을 받는다).
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

  // 만들기는 CLI든 API든 쓸 수 있다(API는 엘리시움이 파일·PowerShell 도구를 대신 실행한다).
  const canBuild = installed.length > 0;
  const buildHint = !canBuild
    ? "먼저 설정에서 AI CLI나 API 키를 연결하세요"
    : `${folder ?? (conversationId ? "기본 작업 폴더" : "새 프로젝트 폴더")} 안에서 파일을 만들고 고쳐 끝까지 완성합니다 (Ctrl+Enter)`;

  // 회의는 AI가 둘 이상 있으면 가장 좋고, 하나뿐이면 같은 AI를 새 세션으로 여럿 띄운다.
  const councilHint = installed.length >= 2 ? "여러 AI가 서로 의견을 주고받으며 최적의 해결책을 함께 논의합니다. (Ctrl+Shift+Enter)" : "연결된 AI가 하나일 때도 여러 세션이 서로 의견을 주고받으며 논의합니다. (Ctrl+Shift+Enter)";

  async function submit(build: boolean, council = false) {
    const t = text.trim();
    if (!t || (build && !canBuild)) return;
    // 작업 중이면 보내기는 작업 중 메시지가 된다(API 작업 모드가 다음 차례에 읽는다). 만들기는 끝난 뒤에 한다.
    if (running) {
      if (build) return;
      setText("");
      await api.taskSteer(running.taskId, t).catch((e) => {
        setText(t);
        toast(errorText(e), "error");
      });
      return;
    }
    setText("");
    await send(t, engine, !council && (work || build), build && !council, council);
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.nativeEvent.isComposing) {
      e.preventDefault();
      if (e.shiftKey) submit(false, true);
      else submit(true);
      return;
    }
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
      submit(false);
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
          placeholder={
            !installed.length
              ? "먼저 설정에서 AI CLI 연결을 확인하세요"
              : running
                ? "작업 중입니다. Enter로 보내면 API 작업 AI가 다음 단계에서 읽습니다"
              : work
                ? "고칠 내용을 적고 Enter로 보내세요. 파일을 고치기 전에 허락을 받습니다"
                : "만들 것을 적고 만들기를 누르세요. 질문은 Enter로 보냅니다"
          }
          aria-label="메시지 입력"
        />
        <div className="composer-bar">
          <div className="pill-menu">
            <button className="pill" onClick={() => setMenu((m) => !m)} aria-haspopup="menu" aria-expanded={menu} title="답할 AI">
              <span className="pill-text">{engineLabel}</span> <ChevronIcon open={menu} />
            </button>
            {menu && (
              <div className="menu" role="menu" onMouseLeave={() => setMenu(false)}>
                <button role="menuitemradio" aria-checked={engine === "auto"} className={engine === "auto" ? "on" : ""} onClick={() => (setEngine("auto"), setMenu(false))}>
                  <b>자동</b>
                  <span>요청에 맞는 AI가 답하고, 복잡하면 다른 AI가 검토</span>
                </button>
                {(["cli", "api"] as const).map((k) => (
                  <div key={k} className="menu-group">
                    <div className="menu-title">{k === "cli" ? "CLI" : "API 키 · 엘리시움 도구로 파일 작업"}</div>
                    {engines
                      .filter((e) => e.kind === k)
                      .map((e) => (
                        <button
                          key={e.id}
                          role="menuitemradio"
                          aria-checked={engine === e.id}
                          disabled={!e.installed}
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
              { value: "chat", label: <><ChatIcon /> 대화</>, title: "읽기 전용: 보내기(Enter)로는 파일을 고치지 않습니다" },
              { value: "work", label: <><WrenchIcon /> 작업</>, title: "보내기(Enter)로 작업 폴더 안의 파일을 고칩니다. 고치기 전에 허락을 받습니다" },
            ]}
            onChange={(v) => setWork(v === "work")}
          />
          <div className="folder-chip">
            <button
              className="pill"
              onClick={() => chooseFolder(conversationId)}
              title={folder ?? (conversationId ? "기본 작업 폴더(설정에서 변경) — 눌러서 이 세션의 폴더 고르기" : "고르지 않으면 만들기는 기본 작업 폴더 안에 새 프로젝트 폴더를 만듭니다 — 눌러서 폴더 고르기")}
            >
              <FolderIcon /> <span className="pill-text">{folder ? baseName(folder) : conversationId ? "기본 폴더" : "새 폴더"}</span>
            </button>
            {folder && (
              <button className="pill-x" onClick={() => resetFolder(conversationId)} aria-label="기본 폴더로" title="기본 폴더로 되돌리기">
                <CloseIcon />
              </button>
            )}
          </div>
          <div className="spacer" />
          {running ? (
            <>
              <button className="send quiet" onClick={() => submit(false)} disabled={!text.trim()} aria-label="작업 중 메시지 보내기" title="작업 중 메시지 보내기 (Enter)">
                <SendIcon />
              </button>
              <button className="send stop" onClick={() => cancel(running.taskId)} aria-label="중지" title="중지 (Esc)">
                <StopIcon />
              </button>
            </>
          ) : (
            <>
              <button
                className="send quiet"
                onClick={() => submit(false)}
                disabled={!text.trim() || installed.length === 0}
                aria-label={work ? "작업 보내기" : "질문 보내기"}
                title={work ? "작업 보내기: 허락을 받은 뒤 폴더 안의 파일을 고칩니다 (Enter)" : "질문 보내기: 파일은 고치지 않습니다 (Enter)"}
              >
                <SendIcon />
              </button>
              <button className="council-btn" onClick={() => submit(false, true)} disabled={!text.trim() || installed.length === 0} title={councilHint} aria-label="AI 회의 시작">
                <CouncilIcon /> 회의
              </button>
              <button className="build-btn" onClick={() => submit(true)} disabled={!text.trim() || !canBuild} title={buildHint}>
                <WrenchIcon /> 만들기
              </button>
            </>
          )}
        </div>
      </div>
      <div className="composer-note">Enter {work ? "작업(허락 후 수정)" : "질문"} · Ctrl+Enter 만들기 · Ctrl+Shift+Enter 회의 · / 스킬 — AI는 실수할 수 있으니 결과를 확인하세요.</div>
    </div>
  );
}

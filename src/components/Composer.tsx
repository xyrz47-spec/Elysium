import { useEffect, useMemo, useRef, useState, type ClipboardEvent, type KeyboardEvent } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, baseName, errorText, toBase64, type Attachment, type EngineChoice, type Suggestion } from "../lib/api";
import { useApp } from "../store";
import { DraftAttachments } from "./Attachments";
import { BookIcon, ChatIcon, ChevronIcon, ClipIcon, CloseIcon, CouncilIcon, FolderIcon, SendIcon, SparkIcon, StopIcon, WrenchIcon } from "./Icons";
import Segmented from "./Segmented";

/** 한 메시지에 붙일 수 있는 파일 수(백엔드 attachments::MAX_PER_MESSAGE와 같다) */
const MAX_FILES = 10;
/** 이 확신도 이상일 때만 입력창 추천을 보여 준다 */
const SUGGEST_MIN = 0.6;
const SUGGEST_DELAY_MS = 700;
const SUGGEST_MIN_CHARS = 8;

/** 대화창 아래 입력 상자: 대화/작업 모드 · 보내기 · 만들기(폴더 안에서 끝까지 완성) · 엔진 고르기 · /스킬 자동 완성 */
const SEND_MORE = "전송 방식 선택";

export default function Composer({ draft, onDraftUsed }: { draft?: string | null; onDraftUsed?: () => void }) {
  const { send, engines, live, conversationId, cancel, skills, conversations, newFolder, chooseFolder, resetFolder, toast, settings, saveSettings, setDialog } = useApp();
  const [text, setText] = useState("");
  const [files, setFiles] = useState<Attachment[]>([]);
  const [dropping, setDropping] = useState(false);
  const [suggestion, setSuggestion] = useState<Suggestion | null>(null);
  const [engine, setEngine] = useState<EngineChoice>("auto");
  // 작업 모드: 보내기(Enter)로도 파일을 고칠 수 있다(실행 전에 허락을 받는다).
  const [work, setWork] = useState(false);
  const [menu, setMenu] = useState(false);
  const [sendMenu, setSendMenu] = useState(false);
  const [promptMenu, setPromptMenu] = useState(false);
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

  const addFiles = (added: Attachment[]) => {
    setFiles((cur) => {
      const room = Math.max(0, MAX_FILES - cur.length);
      for (const extra of added.slice(room)) api.attachmentRemove(extra.id).catch(() => {});
      if (added.length > room) toast("파일은 메시지당 최대 10개까지 첨부할 수 있습니다.", "error");
      return [...cur, ...added.slice(0, room)];
    });
  };
  const guardAdd = (p: Promise<Attachment[]>) => p.then(addFiles).catch((e) => toast(errorText(e), "error"));
  const removeFile = (id: string) => {
    setFiles((cur) => cur.filter((f) => f.id !== id));
    api.attachmentRemove(id).catch(() => {});
  };

  // 창에 끌어다 놓은 파일. Tauri가 드롭을 가로채므로 HTML 드롭 대신 이 이벤트를 쓴다.
  useEffect(() => {
    let off: (() => void) | undefined;
    let gone = false;
    getCurrentWebview()
      .onDragDropEvent((e) => {
        const t = e.payload.type;
        if (t === "enter" || t === "over") setDropping(true);
        else setDropping(false);
        if (e.payload.type === "drop" && e.payload.paths.length) guardAdd(api.attachmentAddPaths(e.payload.paths));
      })
      .then((f) => (gone ? f() : (off = f)))
      .catch(() => {});
    return () => {
      gone = true;
      off?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 붙여 넣은 파일(스크린샷 등). 글만 붙여 넣으면 그대로 둔다.
  function onPaste(e: ClipboardEvent<HTMLTextAreaElement>) {
    const pasted = Array.from(e.clipboardData.files);
    if (pasted.length === 0) return;
    e.preventDefault();
    for (const f of pasted) {
      const stamp = new Date().toISOString().replace(/[-:T]/g, "").slice(0, 14);
      const name = f.name && f.name !== "image.png" ? f.name : `screenshot-${stamp}.png`;
      f.arrayBuffer()
        .then((b) => api.attachmentAddBytes(name, toBase64(b)))
        .then((a) => addFiles([a]))
        .catch((err) => toast(errorText(err), "error"));
    }
  }

  // 입력하는 동안 JEV가 모드·스킬을 추천한다. 늦게 온 답은 버린다.
  useEffect(() => {
    setSuggestion(null);
    const t = text.trim();
    if (!settings?.jevEnabled || !settings.jevSuggest || running || t.startsWith("/") || t.length < SUGGEST_MIN_CHARS) return;
    let stale = false;
    const timer = setTimeout(() => {
      api.jevSuggest(t, files.length > 0).then((s) => !stale && setSuggestion(s)).catch(() => {});
    }, SUGGEST_DELAY_MS);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [text, files.length, settings?.jevEnabled, settings?.jevSuggest, !!running]);

  // 만들기는 CLI든 API든 쓸 수 있다(API는 엘리시움이 파일·PowerShell 도구를 대신 실행한다).
  const canBuild = installed.length > 0;
  const buildHint = !canBuild
    ? "먼저 설정에서 AI CLI나 API 키를 연결하세요"
    : `${folder ?? (conversationId ? "기본 작업 폴더" : "새 프로젝트 폴더")} 안에서 파일을 만들고 고쳐 끝까지 완성합니다 (Ctrl+Enter)`;

  // 회의는 AI가 둘 이상 있으면 가장 좋고, 하나뿐이면 같은 AI를 새 세션으로 여럿 띄운다.
  const councilHint = installed.length >= 2 ? "여러 AI가 서로 의견을 주고받으며 최적의 해결책을 함께 논의합니다. (Ctrl+Shift+Enter)" : "연결된 AI가 하나일 때도 여러 세션이 서로 의견을 주고받으며 논의합니다. (Ctrl+Shift+Enter)";

  async function submit(build: boolean, council = false) {
    const t = text.trim();
    if ((!t && files.length === 0) || (build && !canBuild)) return;
    // 작업 중이면 보내기는 실행 중인 에이전트에게 보내는 메시지가 된다. 만들기는 끝난 뒤에 한다.
    if (running) {
      if (build || !t) return;
      setText("");
      await api.taskSteer(running.taskId, t).catch((e) => {
        setText(t);
        toast(errorText(e), "error");
      });
      return;
    }
    const sending = files;
    setText("");
    setFiles([]);
    const ok = await send(t, engine, !council && (work || build), build && !council, council, sending.map((f) => f.id));
    // 보내지 못했으면 쓴 글과 파일을 되돌린다.
    if (!ok) {
      setText((cur) => cur || t);
      setFiles((cur) => [...sending, ...cur]);
    }
  }

  const modeHint = suggestion && (suggestion.modeConfidence ?? 0) >= SUGGEST_MIN && !running ? suggestion.mode : null;
  const skillHint = suggestion && (suggestion.skillFit ?? 0) >= SUGGEST_MIN && !running ? skills?.skills.find((s) => s.id === suggestion.skill && s.enabled) : undefined;
  const showMode = (modeHint === "work" && !work) || modeHint === "build" || modeHint === "council";

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
  const prompts = settings?.prompts ?? [];
  // 자주 쓰는 프롬프트: 비어 있으면 그대로 넣고, 쓰던 글이 있으면 다음 줄에 붙인다.
  const usePrompt = (t: string) => {
    setText((cur) => (cur.trim() ? `${cur.trimEnd()}\n${t}` : t));
    setPromptMenu(false);
    ref.current?.focus();
  };
  const savePrompt = async () => {
    const t = text.trim();
    if (!t || !settings) return;
    if (prompts.some((p) => p.text === t)) return toast("이미 저장된 프롬프트입니다.");
    await saveSettings({ ...settings, prompts: [...prompts, { title: t.split("\n")[0].slice(0, 24), text: t }] });
    setPromptMenu(false);
    toast("자주 쓰는 프롬프트에 저장했습니다.");
  };
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
      {sendMenu && !running && (
        <div className="menu send-menu" role="menu" onMouseLeave={() => setSendMenu(false)}>
          <button role="menuitem" disabled={(!text.trim() && files.length === 0) || installed.length === 0} onClick={() => (setSendMenu(false), submit(false))}>
            <b>
              <SendIcon /> {work ? "작업 보내기" : "질문 보내기"}
            </b>
            <span>{work ? "작업 보내기: 허락을 받은 뒤 폴더 안의 파일을 고칩니다 (Enter)" : "질문 보내기: 파일은 고치지 않습니다 (Enter)"}</span>
          </button>
          <button role="menuitem" disabled={(!text.trim() && files.length === 0) || installed.length === 0} onClick={() => (setSendMenu(false), submit(false, true))}>
            <b>
              <CouncilIcon size={14} /> 회의
            </b>
            <span>{councilHint}</span>
          </button>
          <button role="menuitem" disabled={(!text.trim() && files.length === 0) || !canBuild} onClick={() => (setSendMenu(false), submit(true))}>
            <b>
              <WrenchIcon /> 만들기
            </b>
            <span>{buildHint}</span>
          </button>
        </div>
      )}
      {promptMenu && (
        <div className="menu prompt-menu" role="menu" onMouseLeave={() => setPromptMenu(false)}>
          <div className="menu-title">자주 쓰는 프롬프트</div>
          {prompts.length === 0 && <div className="menu-empty">저장한 프롬프트가 없습니다. 입력창에 쓴 글을 아래에서 저장해 두세요.</div>}
          {prompts.map((p, i) => (
            <button key={`${i}-${p.title}`} role="menuitem" onClick={() => usePrompt(p.text)} title={p.text}>
              <b>{p.title}</b>
              <span>{p.text.replace(/\s+/g, " ").slice(0, 80)}</span>
            </button>
          ))}
          <div className="menu-actions">
            <button type="button" className="btn sm ghost" disabled={!text.trim()} onClick={savePrompt}>
              지금 입력한 글 저장
            </button>
            <button
              type="button"
              className="btn sm ghost"
              onClick={() => {
                setPromptMenu(false);
                setDialog("settings");
              }}
            >
              관리
            </button>
          </div>
        </div>
      )}
      {/* .composer가 backdrop-filter의 경계라 그 안에 두면 유리가 뒤 화면을 보지 못한다: slash-menu처럼 밖에 둔다 */}
      {menu && (
        <div className="menu" role="menu" onMouseLeave={() => setMenu(false)}>
          <button role="menuitemradio" aria-checked={engine === "auto"} className={engine === "auto" ? "on" : ""} onClick={() => (setEngine("auto"), setMenu(false))}>
            <b>자동</b>
            <span>{"JEV가 가장 효율적인 AI와 작업 분담을 고릅니다"}</span>
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
      {(showMode || skillHint) && (
        <div className="jev-suggest" role="status">
          <SparkIcon size={13} />
          <span className="jev-suggest-tag">JEV</span>
          {showMode && (
            <button
              onClick={() => {
                if (modeHint === "work") setWork(true);
                else if (modeHint === "build") submit(true);
                else submit(false, true);
                setSuggestion(null);
              }}
            >
              {modeHint === "work"
                ? "작업 모드로 전환"
                : modeHint === "build"
                  ? "새 앱 바로 만들기"
                  : "AI 회의로 바로 전송"}
            </button>
          )}
          {skillHint && (
            <button
              onClick={() => {
                setText((cur) => `/${skillHint.name} ${cur.trim()}`);
                setSuggestion(null);
                ref.current?.focus();
              }}
              title={skillHint.description}
            >
              /{skillHint.name}
            </button>
          )}
        </div>
      )}
      <div className={`composer ${work ? "work" : ""} ${dropping ? "dropping" : ""}`}>
        <DraftAttachments items={files} onRemove={removeFile} />
        <textarea
          ref={ref}
          value={text}
          rows={1}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
          onPaste={onPaste}
          placeholder={
            !installed.length
              ? "먼저 설정에서 AI CLI 연결을 확인하세요"
              : running
                ? "작업 중인 AI에게 전할 말 (Enter)"
                : work
                  ? "고칠 내용을 적으세요. 고치기 전에 허락을 받습니다"
                  : "무엇이든 물어보세요. 만들기는 Ctrl+Enter"
          }
          aria-label="메시지 입력"
        />
        <div className="composer-bar">
          <button
            className="pill attach-btn"
            onClick={() => guardAdd(api.attachmentPick())}
            aria-label={"파일 첨부"}
            title={"클릭, 드래그 또는 붙여넣기로 파일(개당 최대 50MB, 최대 10개)을 첨부할 수 있습니다."}
          >
            <ClipIcon />
          </button>
          <button
            className={`pill attach-btn ${promptMenu ? "on" : ""}`}
            onClick={() => setPromptMenu((m) => !m)}
            aria-haspopup="menu"
            aria-expanded={promptMenu}
            aria-label="자주 쓰는 프롬프트"
            title="자주 쓰는 프롬프트"
          >
            <BookIcon />
          </button>
          <div className="pill-menu">
            <button className="pill" onClick={() => setMenu((m) => !m)} aria-haspopup="menu" aria-expanded={menu} title="답할 AI">
              <span className="pill-text">{engineLabel}</span> <ChevronIcon open={menu} />
            </button>
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
            <div className="send-group">
              <button
                className="send quiet"
                onClick={() => submit(false)}
                disabled={(!text.trim() && files.length === 0) || installed.length === 0}
                aria-label={work ? "작업 보내기" : "질문 보내기"}
                title={work ? "작업 보내기: 허락을 받은 뒤 폴더 안의 파일을 고칩니다 (Enter)" : "질문 보내기: 파일은 고치지 않습니다 (Enter)"}
              >
                <SendIcon />
              </button>
              <button className="send-more" onClick={() => setSendMenu((m) => !m)} aria-label={SEND_MORE} title={SEND_MORE} aria-haspopup="menu" aria-expanded={sendMenu}>
                <ChevronIcon open={sendMenu} />
              </button>
            </div>
          )}
        </div>
      </div>
      <div className="composer-note" title="AI는 실수할 수 있으니 결과를 확인하세요.">
        <span>
          <kbd>Enter</kbd> {running ? "AI에게 전달" : work ? "작업" : "질문"}
        </span>
        <span>
          <kbd>Shift+Enter</kbd> 줄바꿈
        </span>
        {!running && (
          <>
            <span>
              <kbd>Ctrl+Enter</kbd> 만들기
            </span>
            <span>
              <kbd>Ctrl+Shift+Enter</kbd> 회의
            </span>
            <span>
              <kbd>/</kbd> 스킬
            </span>
          </>
        )}
      </div>
    </div>
  );
}

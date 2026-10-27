import { useEffect, useRef, useState } from "react";
import { api, errorText, type MemoryView } from "../lib/api";
import { ask } from "../lib/confirm";
import { useTauriEvent } from "../lib/events";
import { useApp } from "../store";

/** 세션 머리의 "기억" 버튼: 이 세션의 모든 AI가 같이 쓰는 세션 메모리를 보여 준다. */
export default function MemoryButton({ conversationId }: { conversationId: string }) {
  const { toast } = useApp();
  const [open, setOpen] = useState(false);
  const [m, setM] = useState<MemoryView | null>(null);
  const box = useRef<HTMLDivElement>(null);

  const load = () => api.memoryGet(conversationId).then(setM).catch((e) => toast(errorText(e), "error"));
  useEffect(() => {
    setM(null);
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId]);
  useTauriEvent<{ conversationId: string }>("memory:updated", (p) => {
    if (p.conversationId === conversationId) load();
  });
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  return (
    <div className="mem" ref={box}>
      <button className={`head-folder mem-btn ${m?.summary ? "has" : ""}`} onClick={() => setOpen((o) => !o)} aria-expanded={open} title="이 세션의 모든 AI가 같이 쓰는 기억">
        <i className="mem-dot" aria-hidden /> 기억
      </button>
      {open && (
        <div className="mem-pop" role="dialog" aria-label="세션 메모리">
          <b>세션 메모리</b>
          <p className="mem-help">이 세션의 모든 AI(초안·검토·토론·판정, CLI·API)가 같은 기억을 받습니다. 최근 12개 메시지는 그대로, 그보다 오래된 대화는 아래 요약으로 전달합니다.</p>
          {m?.summary ? <div className="mem-sum">{m.summary}</div> : <div className="mem-empty">아직 요약이 없습니다. 대화가 길어지면 가장 싼 AI가 오래된 부분을 요약합니다.</div>}
          <div className="mem-foot">
            <span>메시지 {m?.messages ?? 0}개</span>
            {m?.summary && (
              <button
                className="btn sm ghost"
                onClick={async () => {
                  const ok = await ask({ title: "세션 요약을 지울까요?", body: "대화 기록은 그대로 남고, 요약만 지워집니다. 대화가 다시 길어지면 새로 요약합니다.", confirmLabel: "요약 지우기", danger: true });
                  if (ok) api.memoryClear(conversationId).then(setM).catch((e) => toast(errorText(e), "error"));
                }}
              >
                요약 지우기
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

// 워커가 실시간으로 쓰는 글을 마크다운으로 그린다. 글을 덩어리로 나눠 새 글이 붙은 마지막 덩어리만 다시 그린다.
import { memo, useEffect, useMemo, useRef, useState } from "react";
import Markdown from "./Markdown";
import { mdChunks } from "../lib/mdchunks";

/** 덩어리 하나. 글이 같으면 다시 그리지 않는다. */
const Piece = memo(function Piece({ text }: { text: string }) {
  return <Markdown text={text} />;
});

/** 실시간 출력. 맨 아래에 있으면 새 글을 따라가고, 위로 올려 읽는 중이면 그대로 둔다. */
export default function LiveMarkdown({ text, live, empty, jumpLabel, className = "" }: { text: string; live: boolean; empty: string; jumpLabel: string; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const [follow, setFollow] = useState(true);
  const chunks = useMemo(() => mdChunks(text), [text]);
  useEffect(() => {
    const el = ref.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [text, follow]);
  const onScroll = () => {
    const el = ref.current;
    if (el) setFollow(el.scrollHeight - el.scrollTop - el.clientHeight < 24);
  };
  if (!text.trim()) return <div className="worker-empty">{empty}</div>;
  return (
    <div className={`worker-live ${className}`}>
      <div ref={ref} className={`worker-out md-out ${live ? "streaming" : ""}`} onScroll={onScroll} tabIndex={0}>
        {chunks.map((c) => (
          <Piece key={c.at} text={c.text} />
        ))}
        {live && <i className="worker-caret" aria-hidden />}
      </div>
      {!follow && (
        <button type="button" className="btn sm worker-jump" onClick={() => setFollow(true)}>
          ↓ {jumpLabel}
        </button>
      )}
    </div>
  );
}

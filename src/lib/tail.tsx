// Logs render only their newest part; older parts load on demand ("show earlier" at the top).
import { useLayoutEffect, useRef, useState, type RefObject } from "react";

export const TAIL_TEXT = {
  items: "이전 로그 {n}개 불러오기",
  lines: "이전 {n}줄 불러오기",
};

/** Shows the last `step` items, `more()` reveals `step` older ones. */
export function useTail(total: number, step: number) {
  const [n, setN] = useState(step);
  const shown = Math.min(total, n);
  return { shown, hidden: total - shown, more: () => setN((x) => x + step) };
}

/** Keeps the reader's place when older content is added above (scroll container must be `box`). */
export function useKeepScroll(box: RefObject<HTMLElement | null>, shown: number) {
  const before = useRef<number | null>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (el && before.current != null) el.scrollTop += el.scrollHeight - before.current;
    before.current = null;
  }, [box, shown]);
  return () => {
    before.current = box.current?.scrollHeight ?? null;
  };
}

export function LoadEarlier({ hidden, label, onMore }: { hidden: number; label: string; onMore: () => void }) {
  if (hidden <= 0) return null;
  return (
    <button type="button" className="link load-earlier" onClick={onMore}>
      {label.replace("{n}", String(hidden))}
    </button>
  );
}

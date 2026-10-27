import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";

/** Tauri 이벤트를 구독한다. 핸들러는 매 렌더 최신 것을 쓴다. */
export function useTauriEvent<T>(name: string, handler: (payload: T) => void) {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    let off: (() => void) | undefined;
    let alive = true;
    listen<T>(name, (e) => ref.current(e.payload)).then((u) => {
      if (alive) off = u;
      else u();
    });
    return () => {
      alive = false;
      off?.();
    };
  }, [name]);
}

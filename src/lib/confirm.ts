// 앱 안의 확인 창(브라우저 기본 confirm 대신). ask()가 사용자의 답(true/false)을 돌려준다.
import { useSyncExternalStore } from "react";

export interface Ask {
  title: string;
  /** 무엇이 사라지는지·되돌릴 수 있는지 */
  body: string;
  confirmLabel: string;
  /** 지우기처럼 되돌릴 수 없는 일이면 true(빨간 버튼, 기본 초점은 취소) */
  danger?: boolean;
  resolve: (ok: boolean) => void;
}

let current: Ask | null = null;
const subs = new Set<() => void>();
const notify = () => subs.forEach((f) => f());

export function ask(opts: Omit<Ask, "resolve">): Promise<boolean> {
  return new Promise((resolve) => {
    current?.resolve(false);
    current = { ...opts, resolve };
    notify();
  });
}

export function answer(ok: boolean) {
  const c = current;
  current = null;
  notify();
  c?.resolve(ok);
}

export function useAsk(): Ask | null {
  return useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => current,
  );
}

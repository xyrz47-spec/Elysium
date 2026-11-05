// 오른쪽 총괄 패널과 메인 대화 요약의 구성(사용자 설정). 이 기기의 localStorage에 둔다.
import { useSyncExternalStore } from "react";

export type SectionId = "progress" | "lead" | "workers" | "decisions";
export type Density = "compact" | "full";

export interface PanelLayout {
  order: SectionId[];
  hidden: SectionId[];
  density: Density;
  /** 메인 대화 카드에 병렬 작업 요약을 보인다 */
  brief: boolean;
}

export const SECTIONS: SectionId[] = ["progress", "lead", "workers", "decisions"];
export const LAYOUT_DEFAULT: PanelLayout = { order: SECTIONS, hidden: [], density: "compact", brief: true };
const KEY = "elysium.panelLayout";

/** 저장된 값이 깨졌거나 섹션이 늘어도 모든 섹션이 한 번씩 들어가게 맞춘다. */
export function normalize(raw: unknown): PanelLayout {
  const v = (raw && typeof raw === "object" ? raw : {}) as Partial<PanelLayout>;
  const known = (xs: unknown) => (Array.isArray(xs) ? xs.filter((x): x is SectionId => SECTIONS.includes(x)) : []);
  const order = [...new Set(known(v.order))];
  for (const s of SECTIONS) if (!order.includes(s)) order.push(s);
  return {
    order,
    hidden: [...new Set(known(v.hidden))],
    density: v.density === "full" ? "full" : "compact",
    brief: v.brief !== false,
  };
}

export function move(order: SectionId[], id: SectionId, by: -1 | 1): SectionId[] {
  const i = order.indexOf(id);
  const j = i + by;
  if (i < 0 || j < 0 || j >= order.length) return order;
  const next = [...order];
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

let current: PanelLayout = (() => {
  try {
    return normalize(JSON.parse(localStorage.getItem(KEY) ?? "null"));
  } catch {
    return LAYOUT_DEFAULT;
  }
})();
const subs = new Set<() => void>();

export function setLayout(next: PanelLayout) {
  current = normalize(next);
  try {
    localStorage.setItem(KEY, JSON.stringify(current));
  } catch {
    // 저장 공간이 막혀도 이번 실행 동안은 바뀐 구성을 쓴다.
  }
  subs.forEach((f) => f());
}

export function usePanelLayout(): [PanelLayout, (next: PanelLayout) => void] {
  const v = useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => current,
  );
  return [v, setLayout];
}

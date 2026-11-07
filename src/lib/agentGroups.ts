// 실행 중인 에이전트를 보기 좋게 묶는 그룹(폴더). 화면에서만 쓰며 실행에는 영향이 없다.
// 작업(taskId)마다 따로 두고, 작업이 끝나면 지운다. 앱 메모리에만 둔다(에이전트 key는 작업마다 새로 생긴다).
import { useSyncExternalStore } from "react";

export interface AgentGroup {
  id: string;
  task: string;
  name: string;
  /** 그룹 색(색상환 각도) */
  hue: number;
  open: boolean;
  /** 에이전트 실행 key(넣은 순서) */
  keys: string[];
}

const HUES = [205, 275, 330, 150, 32, 52, 185, 0];

let groups: AgentGroup[] = [];
let seq = 0;
const subs = new Set<() => void>();

function set(next: AgentGroup[]) {
  groups = next;
  subs.forEach((f) => f());
}

/** 같은 작업의 다른 그룹에서 keys를 뺀다(에이전트는 그룹 하나에만 들어간다). */
export function without(list: AgentGroup[], task: string, keys: string[]): AgentGroup[] {
  return list.map((g) => (g.task === task && g.keys.some((k) => keys.includes(k)) ? { ...g, keys: g.keys.filter((k) => !keys.includes(k)) } : g));
}

export const groupOps = {
  /** 새 그룹을 만들고 keys를 옮긴다. 만든 그룹 id를 돌려준다. */
  create(task: string, name: string, keys: string[] = []): string {
    const id = `g${++seq}`;
    const hue = HUES[groups.filter((g) => g.task === task).length % HUES.length];
    set([...without(groups, task, keys), { id, task, name, hue, open: true, keys: [...new Set(keys)] }]);
    return id;
  },
  rename(id: string, name: string) {
    const n = name.trim();
    if (n) set(groups.map((g) => (g.id === id ? { ...g, name: n } : g)));
  },
  /** 그룹만 지운다. 안의 에이전트는 그룹 밖으로 돌아간다. */
  remove(id: string) {
    set(groups.filter((g) => g.id !== id));
  },
  toggle(id: string, open?: boolean) {
    set(groups.map((g) => (g.id === id ? { ...g, open: open ?? !g.open } : g)));
  },
  /** keys를 그룹 id로 옮긴다. id가 null이면 그룹에서 뺀다. */
  assign(task: string, keys: string[], id: string | null) {
    const next = without(groups, task, keys);
    set(id ? next.map((g) => (g.id === id ? { ...g, keys: [...new Set([...g.keys, ...keys])] } : g)) : next);
  },
  /** 끝난 작업의 그룹을 지운다. */
  prune(liveTasks: string[]) {
    if (groups.some((g) => !liveTasks.includes(g.task))) set(groups.filter((g) => liveTasks.includes(g.task)));
  },
};

/** 모든 작업의 그룹(바뀔 때만 새 배열) */
export function useAllAgentGroups(): AgentGroup[] {
  return useSyncExternalStore(
    (f) => {
      subs.add(f);
      return () => subs.delete(f);
    },
    () => groups,
  );
}

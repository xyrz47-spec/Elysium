// 작업 진행 상황 계산(순수 함수). PC 오른쪽 창과 폰이 같이 쓴다.

export interface RunLike {
  key: string;
  role: string;
  state: "run" | "ok" | "fail";
  wait?: string | null;
  stopped?: boolean;
}

export interface Counts {
  total: number;
  ok: number;
  run: number;
  wait: number;
  fail: number;
}

export type Phase = "split" | "work" | "integrate" | "done";

/** 하위 작업을 맡은 워커만 센다. 병렬 작업이 아니면 총괄을 뺀 모든 실행을 센다. */
export function workerCounts(runs: RunLike[]): Counts {
  const workers = runs.some((a) => a.key.startsWith("agent:")) ? runs.filter((a) => a.key.startsWith("agent:")) : runs.filter((a) => !a.key.startsWith("lead"));
  const c: Counts = { total: workers.length, ok: 0, run: 0, wait: 0, fail: 0 };
  for (const a of workers) {
    if (a.state === "ok") c.ok++;
    else if (a.state === "fail" || a.stopped) c.fail++;
    // 할 일을 끝내고 메시지를 기다리는 워커는 끝난 것과 같이 본다.
    else if (a.wait === "idle") c.ok++;
    else if (a.wait) c.wait++;
    else c.run++;
  }
  return c;
}

/** 지금 단계: 나누기 → 워커 작업 → 통합 → 끝 */
export function phaseOf(runs: RunLike[]): Phase {
  const running = (f: (a: RunLike) => boolean) => runs.some((a) => a.state === "run" && f(a));
  if (running((a) => a.role === "synth" || a.key.startsWith("synth"))) return "integrate";
  if (running((a) => a.key.startsWith("agent:"))) return "work";
  if (running((a) => a.role === "plan" || a.role === "roadmap")) return "split";
  if (runs.some((a) => a.state === "run")) return "work";
  return runs.length ? "done" : "split";
}

/** 끝난 비율(0~1): 완료·실패한 워커 수 / 전체 */
export function doneRatio(c: Counts): number {
  return c.total ? (c.ok + c.fail) / c.total : 0;
}

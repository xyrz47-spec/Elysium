// 실시간 토큰 사용량: 실행(key)마다 끝난 차례의 합계와 지금 차례의 값(실행 중에 늘어난다)을 따로 쌓는다.

export interface Tok {
  input: number;
  output: number;
  /** 추정 비용(USD) */
  cost: number;
}

export interface RunUsage {
  done: Tok;
  cur: Tok;
  /** `done` when the current run started */
  base?: Tok;
  /** Turn the live value belongs to: one run can have several turns (messages from the lead, restarts). */
  turn?: number;
}

export const ZERO: Tok = { input: 0, output: 0, cost: 0 };

export const add = (a: Tok, b: Tok): Tok => ({ input: a.input + b.input, output: a.output + b.output, cost: a.cost + b.cost });
const max = (a: Tok, b: Tok): Tok => ({ input: Math.max(a.input, b.input), output: Math.max(a.output, b.output), cost: Math.max(a.cost, b.cost) });

export const total = (r: RunUsage | undefined): Tok => (r ? add(r.done, r.cur) : ZERO);

export const sumUsage = (m: Record<string, RunUsage>): Tok => Object.values(m).reduce((t, r) => add(t, total(r)), ZERO);

/** task:agent-usage: 이번 차례에서 지금까지 쓴 값 */
export function liveTurn(r: RunUsage | undefined, p: { input: number; output: number; costUsd?: number | null; turn?: number }): RunUsage {
  const cur = { input: p.input, output: p.output, cost: p.costUsd ?? 0 };
  // A new turn in the same run: keep the previous turn in the total instead of overwriting it.
  const next = r && p.turn != null && r.turn != null && p.turn !== r.turn;
  return { done: next ? add(r.done, r.cur) : (r?.done ?? ZERO), base: r?.base, cur, turn: p.turn ?? r?.turn };
}

/** task:agent: 차례가 시작하면 지금 값을 비우고, 끝나면 최종 값(없으면 실시간 값)을 합계에 더한다. */
export function turnEvent(r: RunUsage | undefined, p: { state: string; inputTokens?: number | null; outputTokens?: number | null; costUsd?: number | null }): RunUsage {
  const done = r?.done ?? ZERO;
  if (p.state === "run") return { done, base: done, cur: ZERO };
  const seen = add(done, r?.cur ?? ZERO);
  if (p.inputTokens == null && p.outputTokens == null) return { done: seen, cur: ZERO };
  // The final report covers every turn of this run; never show less than what was already seen live.
  const final: Tok = { input: p.inputTokens ?? 0, output: p.outputTokens ?? 0, cost: p.costUsd ?? r?.cur.cost ?? 0 };
  return { done: max(add(r?.base ?? ZERO, final), seen), cur: ZERO };
}

export function shortTokens(n: number): string {
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(n);
}

export function usd(n: number): string {
  if (n === 0) return "$0";
  if (n < 0.01) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(2)}`;
}

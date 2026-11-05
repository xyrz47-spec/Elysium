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
}

export const ZERO: Tok = { input: 0, output: 0, cost: 0 };

export const add = (a: Tok, b: Tok): Tok => ({ input: a.input + b.input, output: a.output + b.output, cost: a.cost + b.cost });

export const total = (r: RunUsage | undefined): Tok => (r ? add(r.done, r.cur) : ZERO);

export const sumUsage = (m: Record<string, RunUsage>): Tok => Object.values(m).reduce((t, r) => add(t, total(r)), ZERO);

/** task:agent-usage: 이번 차례에서 지금까지 쓴 값 */
export function liveTurn(r: RunUsage | undefined, p: { input: number; output: number; costUsd?: number | null }): RunUsage {
  return { done: r?.done ?? ZERO, cur: { input: p.input, output: p.output, cost: p.costUsd ?? 0 } };
}

/** task:agent: 차례가 시작하면 지금 값을 비우고, 끝나면 최종 값(없으면 실시간 값)을 합계에 더한다. */
export function turnEvent(r: RunUsage | undefined, p: { state: string; inputTokens?: number | null; outputTokens?: number | null; costUsd?: number | null }): RunUsage {
  const done = r?.done ?? ZERO;
  if (p.state === "run") return { done, cur: ZERO };
  const cur = r?.cur ?? ZERO;
  const final: Tok = p.inputTokens != null || p.outputTokens != null ? { input: p.inputTokens ?? 0, output: p.outputTokens ?? 0, cost: p.costUsd ?? cur.cost } : cur;
  return { done: add(done, final), cur: ZERO };
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

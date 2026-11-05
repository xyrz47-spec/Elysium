// 실시간 토큰 사용량 칩: 입력 · 출력 · 추정 비용. 실행 중이면 점이 맥박친다.
import { shortTokens, usd, type Tok } from "../lib/usage";

export const TOKEN_TEXT = {
  live: "토큰 사용량",
  input: "입력",
  output: "출력",
  cost: "예상 비용",
};

export default function TokenMeter({ t, live = false, compact = false }: { t: Tok; live?: boolean; compact?: boolean }) {
  if (t.input + t.output === 0) return null;
  const title = `${TOKEN_TEXT.input} ${t.input.toLocaleString()} · ${TOKEN_TEXT.output} ${t.output.toLocaleString()} · ${TOKEN_TEXT.cost} ${usd(t.cost)}`;
  return (
    <span className={`token-meter ${live ? "live" : ""} ${compact ? "compact" : ""}`} title={title} aria-label={title}>
      {live && <i aria-hidden />}
      {!compact && <b>{TOKEN_TEXT.live}</b>}
      <span>↑{shortTokens(t.input)}</span>
      <span>↓{shortTokens(t.output)}</span>
      {t.cost > 0 && <span className="token-cost">{usd(t.cost)}</span>}
    </span>
  );
}

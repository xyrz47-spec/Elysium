// 오른쪽 도구 창의 폭과 워커 글 다듬기(순수 함수)
export const PANEL_DEFAULT = 560;
const PANEL_MIN = 360;
/** 양쪽 도크(84px씩) */
export const DOCK = 84;
const DOCKS = DOCK * 2;
/** 창을 넓혀도 대화가 이만큼은 남게 한다 */
const CHAT_MIN = 420;

/** 워커가 지금까지 쓴 글은 끝부분만 둔다. 늦게 연 별도 창도 이만큼은 보여 준다. */
export const STREAM_KEEP = 300000;
export const ACTIVITY_KEEP = 200;

export function clampPanel(stored: number, windowWidth: number): number {
  const wanted = Number.isFinite(stored) ? stored : PANEL_DEFAULT;
  const max = Math.max(PANEL_MIN, windowWidth - DOCKS - CHAT_MIN);
  return Math.min(max, Math.max(PANEL_MIN, wanted));
}

/** 블록을 먼저 가리고 끝부분만 남긴다. 순서가 바뀌면 잘린 블록의 JSON이 드러난다. */
export function tailText(text: string, keep: number): string {
  const shown = hideBlocks(text);
  return shown.length > keep ? `…${shown.slice(-keep)}` : shown;
}

/** Elysium과 주고받는 블록은 사람에게 보이지 않는다. 아직 닫히지 않은 블록도 가린다. */
export function hideBlocks(text: string): string {
  return text.replace(/```elysium-(?:request|group)[\s\S]*?(?:```\r?\n?|$)/g, "");
}

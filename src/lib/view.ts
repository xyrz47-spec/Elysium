// 이 PC의 화면 크기 설정(글자·컨트롤 배율, 대화 폭). 테마처럼 이 PC의 앱에만 기억한다.
import { api } from "./api";

export type UiScale = 0.9 | 1 | 1.1 | 1.2;
export type ChatWidth = "normal" | "wide";

const SCALE_KEY = "elysium.scale";
const WIDTH_KEY = "elysium.chatWidth";
export const SCALES: UiScale[] = [0.9, 1, 1.1, 1.2];

export function loadScale(): UiScale {
  const v = Number(localStorage.getItem(SCALE_KEY));
  return (SCALES as number[]).includes(v) ? (v as UiScale) : 1;
}

export function applyScale(s: UiScale) {
  localStorage.setItem(SCALE_KEY, String(s));
  api.uiZoom(s).catch(() => {});
}

export function loadWidth(): ChatWidth {
  return localStorage.getItem(WIDTH_KEY) === "wide" ? "wide" : "normal";
}

export function applyWidth(w: ChatWidth) {
  localStorage.setItem(WIDTH_KEY, w);
  document.documentElement.dataset.chatWidth = w;
}

/** 앱을 켤 때 한 번 */
export function applyView() {
  const s = loadScale();
  if (s !== 1) api.uiZoom(s).catch(() => {});
  document.documentElement.dataset.chatWidth = loadWidth();
}

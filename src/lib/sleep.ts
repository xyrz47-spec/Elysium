// 취침 모드: PC를 쓰지 않는 동안 앱의 그리기(입자 구·애니메이션·실시간 글)를 멈추고 모니터를 끈다.
// 작업은 그대로 돌고(백엔드가 그동안 PC가 잠들지 않게 붙잡는다), 화면을 누르거나 키를 누르면 돌아온다.
import { useSyncExternalStore } from "react";
import { api } from "./api";

const AUTO_KEY = "elysium.autoSleep";
const MONITOR_KEY = "elysium.sleepMonitorOff";

let on = false;
const subs = new Set<() => void>();

export const isSleeping = () => on;

export function setSleep(v: boolean) {
  if (on === v) return;
  on = v;
  document.documentElement.toggleAttribute("data-sleep", v);
  window.dispatchEvent(new Event("elysium:sleep"));
  api.powerSleep(v, v && loadMonitorOff()).catch(() => {});
  subs.forEach((f) => f());
}

export function useSleep(): boolean {
  return useSyncExternalStore(
    (cb) => {
      subs.add(cb);
      return () => subs.delete(cb);
    },
    isSleeping,
  );
}

/** 자동 취침: 입력이 없은 지 몇 분 뒤(0이면 끔) */
export function loadAutoSleep(): number {
  return Number(localStorage.getItem(AUTO_KEY) ?? "0") || 0;
}
export function saveAutoSleep(min: number) {
  localStorage.setItem(AUTO_KEY, String(min));
}
export function loadMonitorOff(): boolean {
  return localStorage.getItem(MONITOR_KEY) !== "0";
}
export function saveMonitorOff(v: boolean) {
  localStorage.setItem(MONITOR_KEY, v ? "1" : "0");
}

/** 입력이 없으면 정한 시간 뒤 취침 모드로 들어간다. 돌려주는 함수로 멈춘다. */
export function watchIdle(): () => void {
  let last = Date.now();
  const touch = () => {
    last = Date.now();
  };
  const events = ["pointermove", "pointerdown", "keydown", "wheel"] as const;
  events.forEach((e) => window.addEventListener(e, touch, { passive: true }));
  const t = window.setInterval(() => {
    const min = loadAutoSleep();
    if (min > 0 && !on && Date.now() - last >= min * 60_000) setSleep(true);
  }, 30_000);
  return () => {
    events.forEach((e) => window.removeEventListener(e, touch));
    window.clearInterval(t);
  };
}

// 다크·화이트 모드. "system"이면 Windows 설정을 따른다. 고른 값은 이 PC의 앱에만 기억한다.
export type ThemeChoice = "system" | "dark" | "light";

const KEY = "elysium.theme";
const media = () => window.matchMedia("(prefers-color-scheme: light)");

export function loadTheme(): ThemeChoice {
  try {
    const v = localStorage.getItem(KEY);
    return v === "dark" || v === "light" ? v : "system";
  } catch {
    return "system";
  }
}

export function resolved(choice: ThemeChoice): "dark" | "light" {
  return choice === "system" ? (media().matches ? "light" : "dark") : choice;
}

export function applyTheme(choice: ThemeChoice) {
  document.documentElement.dataset.theme = resolved(choice);
  try {
    localStorage.setItem(KEY, choice);
  } catch {
    /* 저장할 수 없으면 이번 실행에만 */
  }
}

/** 시스템 설정이 바뀌면 "system"일 때 따라간다. */
export function watchSystem(get: () => ThemeChoice): () => void {
  const m = media();
  const on = () => get() === "system" && applyTheme("system");
  m.addEventListener("change", on);
  return () => m.removeEventListener("change", on);
}

export function isLight(): boolean {
  return document.documentElement.dataset.theme === "light";
}

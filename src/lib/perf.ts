// Low-spec mode: turns off animated backgrounds, glass refraction/blur and decorative animations.
// Remembered per PC in localStorage; other windows (worker windows) follow through the storage event.
const KEY = "elysium.lowSpec";
export const PERF_EVENT = "elysium:perf";

/** First run: weak CPUs (4 threads or fewer) start in low-spec mode. */
function guess(): boolean {
  const cores = navigator.hardwareConcurrency || 8;
  const mem = (navigator as Navigator & { deviceMemory?: number }).deviceMemory ?? 8;
  return cores <= 4 || mem <= 4;
}

export function loadLowSpec(): boolean {
  try {
    const v = localStorage.getItem(KEY);
    return v == null ? guess() : v === "1";
  } catch {
    return false;
  }
}

export function isLowSpec(): boolean {
  return document.documentElement.dataset.perf === "low";
}

function mark(on: boolean) {
  if (on) document.documentElement.dataset.perf = "low";
  else delete document.documentElement.dataset.perf;
  window.dispatchEvent(new Event(PERF_EVENT));
}

export function applyLowSpec(on: boolean) {
  try {
    localStorage.setItem(KEY, on ? "1" : "0");
  } catch {
    /* not saved: applies to this run only */
  }
  mark(on);
}

/** Once at startup: apply the saved choice and follow changes made in other windows. */
export function initLowSpec() {
  mark(loadLowSpec());
  window.addEventListener("storage", (e) => {
    if (e.key === KEY) mark(loadLowSpec());
  });
}

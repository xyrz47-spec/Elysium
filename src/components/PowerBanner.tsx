// Countdown shown before Elysium shuts down / restarts / logs off the PC after a task (see src-tauri/src/power.rs).
import { useEffect, useState } from "react";
import { api, errorText } from "../lib/api";
import { useTauriEvent } from "../lib/events";
import { useApp } from "../store";

type Kind = "shutdown" | "restart" | "logoff";

export const POWER_TEXT = {
  toggle: "작업 완료 시 종료 허용",
  help: "작업 요청 시 '다 끝나면 컴퓨터 꺼 줘'와 같이 지정하면, 모든 작업 완료 후 60초 카운트다운을 거쳐 PC를 종료·다시 시작·로그아웃합니다.",
  shutdown: "{s}초 후 PC가 종료됩니다.",
  restart: "{s}초 후 PC가 다시 시작됩니다.",
  logoff: "{s}초 후 로그아웃됩니다.",
  cancel: "취소",
  cancelled: "PC 종료 예약이 취소되었습니다.",
  skipped: "'작업 완료 시 종료 허용' 설정이 꺼져 있어 PC를 종료하지 않았습니다. 설정을 켜 주세요.",
  failed: "PC 종료 작업을 진행하지 못했습니다.",
};

export default function PowerBanner() {
  const { toast } = useApp();
  const [p, setP] = useState<{ kind: Kind; until: number } | null>(null);
  const [now, setNow] = useState(Date.now());
  useTauriEvent<{ kind: Kind; seconds: number }>("power:pending", (e) => setP({ kind: e.kind, until: Date.now() + e.seconds * 1000 }));
  useTauriEvent("power:cancelled", () => {
    setP(null);
    toast(POWER_TEXT.cancelled);
  });
  useTauriEvent("power:skipped", () => toast(POWER_TEXT.skipped));
  useTauriEvent<{ error: string }>("power:failed", (e) => {
    setP(null);
    toast(`${POWER_TEXT.failed} (${e.error})`, "error");
  });
  useEffect(() => {
    if (!p) return;
    const t = setInterval(() => setNow(Date.now()), 250);
    return () => clearInterval(t);
  }, [p]);
  if (!p) return null;
  const left = Math.max(0, Math.ceil((p.until - now) / 1000));
  return (
    <div className="power-banner" role="alertdialog" aria-live="assertive">
      <b>{POWER_TEXT[p.kind].replace("{s}", String(left))}</b>
      <button
        type="button"
        className="btn sm danger"
        autoFocus
        onClick={() =>
          api
            .powerCancel()
            .then(() => setP(null))
            .catch((e) => toast(errorText(e), "error"))
        }
      >
        {POWER_TEXT.cancel}
      </button>
    </div>
  );
}

/** The same switch as in settings, for the agent board. */
export function PowerToggle() {
  const { settings, saveSettings } = useApp();
  if (!settings) return null;
  return (
    <label className="power-toggle" title={POWER_TEXT.help}>
      <input type="checkbox" className="switch" role="switch" checked={settings.powerOnDone ?? false} onChange={(e) => saveSettings({ ...settings, powerOnDone: e.target.checked })} />
      {POWER_TEXT.toggle}
    </label>
  );
}

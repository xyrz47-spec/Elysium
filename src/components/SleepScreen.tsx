// 취침 화면: 검은 화면에 시각과 진행 중인 작업 수만 보인다. 누르거나 키를 누르면 돌아온다.
import { useEffect, useState } from "react";
import { setSleep, useSleep, watchIdle } from "../lib/sleep";
import { useApp } from "../store";

export const SLEEP_TEXT = {
  title: "취침 모드",
  running: "{n}개의 작업이 진행 중입니다.",
  idle: "진행 중인 작업이 없습니다.",
  wake: "화면을 클릭하거나 아무 키나 누르세요.",
};

const two = (n: number) => String(n).padStart(2, "0");

export default function SleepScreen() {
  const sleeping = useSleep();
  const { live } = useApp();
  const [now, setNow] = useState(() => new Date());
  useEffect(() => watchIdle(), []);
  useEffect(() => {
    if (!sleeping) return;
    setNow(new Date());
    const t = window.setInterval(() => setNow(new Date()), 30_000);
    const wake = (e: KeyboardEvent) => {
      // 깨우는 키는 앱의 다른 단축키로 넘기지 않는다.
      e.preventDefault();
      e.stopPropagation();
      setSleep(false);
    };
    window.addEventListener("keydown", wake, { capture: true });
    return () => {
      window.clearInterval(t);
      window.removeEventListener("keydown", wake, { capture: true });
    };
  }, [sleeping]);
  if (!sleeping) return null;
  return (
    <div className="sleep-screen" role="dialog" aria-label={SLEEP_TEXT.title} onPointerDown={() => setSleep(false)}>
      <div className="sleep-clock">
        {two(now.getHours())}:{two(now.getMinutes())}
      </div>
      <div className="sleep-title">{SLEEP_TEXT.title}</div>
      <div className="sleep-status">{live.length ? SLEEP_TEXT.running.replace("{n}", String(live.length)) : SLEEP_TEXT.idle}</div>
      <div className="sleep-wake">{SLEEP_TEXT.wake}</div>
    </div>
  );
}

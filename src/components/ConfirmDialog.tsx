import { useEffect, useRef } from "react";
import { answer, useAsk } from "../lib/confirm";

/** 확인 창: 되돌릴 수 없는 일(삭제 등) 앞에서 무엇이 사라지는지 알려 주고 한 번 더 묻는다. */
export default function ConfirmDialog() {
  const a = useAsk();
  const cancelRef = useRef<HTMLButtonElement>(null);
  const okRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!a) return;
    // 위험한 일이면 실수로 Enter를 눌러도 지워지지 않게 취소에 초점을 둔다.
    (a.danger ? cancelRef : okRef).current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        answer(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [a]);

  if (!a) return null;
  return (
    <div className="modal-back confirm-back" onMouseDown={(e) => e.target === e.currentTarget && answer(false)}>
      <div className={`modal confirm ${a.danger ? "danger" : ""}`} role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby="confirm-body">
        <div className="confirm-icon" aria-hidden>
          {a.danger ? "!" : "?"}
        </div>
        <h2 id="confirm-title">{a.title}</h2>
        <p id="confirm-body">{a.body}</p>
        <div className="confirm-actions">
          <button ref={cancelRef} className="btn" onClick={() => answer(false)}>
            취소
          </button>
          <button ref={okRef} className={`btn ${a.danger ? "danger solid" : "primary"}`} onClick={() => answer(true)}>
            {a.confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

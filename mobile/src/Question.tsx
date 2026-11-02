// 총괄의 질문에 폰에서 답한다. PC와 폰 중 먼저 온 답을 쓴다.
import { useState } from "react";

const QT = {
  title: "확인이 필요합니다",
  placeholder: "답변을 입력해 주세요",
  send: "전송",
};

export interface LeadQuestion {
  id: string;
  question: string;
  options: string[];
}

export function QuestionCard({ q, onAnswer }: { q: LeadQuestion; onAnswer: (id: string, text: string) => Promise<unknown> }) {
  const [text, setText] = useState("");
  return (
    <article className="approval">
      <b>{QT.title}</b>
      <p>{q.question}</p>
      {q.options.length > 0 && (
        <div className="row">
          {q.options.map((o) => (
            <button key={o} className="ghost-btn" onClick={() => onAnswer(q.id, o)}>
              {o}
            </button>
          ))}
        </div>
      )}
      <div className="row">
        <input type="text" value={text} onChange={(e) => setText(e.target.value)} placeholder={QT.placeholder} aria-label={QT.placeholder} />
        <button className="cta sm" disabled={!text.trim()} onClick={() => onAnswer(q.id, text.trim())}>
          {QT.send}
        </button>
      </div>
    </article>
  );
}

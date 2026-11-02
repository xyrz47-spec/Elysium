import { useEffect, useState, type CSSProperties } from "react";
import { api, errorText, formatSize, type Attachment } from "../lib/api";
import { useApp } from "../store";
import { ClipIcon, CloseIcon, DownloadIcon, FolderIcon, ImageIcon } from "./Icons";

/** 입력창 위 첨부 칩: 보내기 전에 뺄 수 있다 */
export function DraftAttachments({ items, onRemove }: { items: Attachment[]; onRemove: (id: string) => void }) {
  if (items.length === 0) return null;
  return (
    <div className="attach-row" role="list" aria-label={"첨부 파일 목록"}>
      {items.map((a) => (
        <div key={a.id} className="attach-chip" role="listitem" title={`${a.name} · ${formatSize(a.size)}`}>
          {a.kind === "image" ? <ImageIcon /> : <ClipIcon />}
          <span className="attach-name">{a.name}</span>
          <em>{formatSize(a.size)}</em>
          <button className="attach-x" onClick={() => onRemove(a.id)} aria-label={"첨부 파일 삭제"}>
            <CloseIcon />
          </button>
        </div>
      ))}
    </div>
  );
}

function Thumb({ a }: { a: Attachment }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    api.attachmentPreview(a.id).then((u) => live && setUrl(u)).catch(() => {});
    return () => {
      live = false;
    };
  }, [a.id]);
  return url ? <img className="attach-thumb" src={url} alt={a.name} loading="lazy" /> : <span className="attach-thumb empty"><ImageIcon /></span>;
}

/** 메시지에 붙은 파일: 눌러서 열고, 다른 곳에 저장하거나 탐색기에서 본다 */
export function MessageAttachments({ items, fromAgent = false }: { items: Attachment[] | undefined; fromAgent?: boolean }) {
  const { toast } = useApp();
  if (!items?.length) return null;
  const guard = (p: Promise<unknown>) => p.catch((e) => toast(errorText(e), "error"));
  return (
    <section className="attach-list">
      {fromAgent && <div className="attach-title"><ClipIcon /> {"에이전트가 첨부한 파일"}</div>}
      <div className="attach-cards">
        {items.map((a, i) => (
          <div key={a.id} className={`attach-card ${a.kind}`} style={{ "--i": i } as CSSProperties}>
            <button
              className="attach-open"
              title={a.name}
              onClick={() =>
                guard(
                  api.attachmentOpen(a.id).then((revealed) => {
                    if (revealed) toast("보안을 위해 파일을 실행하지 않고 파일 탐색기에 표시했습니다.");
                  }),
                )
              }
            >
              {a.kind === "image" ? <Thumb a={a} /> : <span className="attach-thumb empty"><ClipIcon /></span>}
              <span className="attach-meta">
                <b>{a.name}</b>
                <small>{formatSize(a.size)}</small>
              </span>
            </button>
            <div className="attach-actions">
              <button
                onClick={() =>
                  guard(
                    api.attachmentSaveAs(a.id).then((p) => {
                      if (p) toast("파일이 저장되었습니다.");
                    }),
                  )
                }
                aria-label={"다른 이름으로 저장"}
                title={"다른 이름으로 저장"}
              >
                <DownloadIcon />
              </button>
              <button
                onClick={() => guard(api.attachmentReveal(a.id))}
                aria-label={"파일 탐색기에서 표시"}
                title={"파일 탐색기에서 표시"}
              >
                <FolderIcon />
              </button>
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

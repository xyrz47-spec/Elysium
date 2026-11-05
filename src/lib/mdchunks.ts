// 실시간 마크다운을 덩어리로 나누는 순수 함수

/** 덩어리를 나누는 대략의 길이 */
const CHUNK = 4096;
/** 화면에 두는 글자 수. 더 앞은 그리지 않는다(아주 긴 글을 통째로 다시 배치하면 창이 버벅인다). */
const SHOW_CHARS = 120_000;

export interface MdChunk {
  /** 원래 글에서 이 덩어리가 시작하는 위치(키로 쓴다) */
  at: number;
  text: string;
}

const FENCE = /^(`{3,}|~{3,})/;

/**
 * 마크다운 덩어리. 줄 끝에서만 나누고, 코드 블록 안에서는 나누지 않는다(너무 긴 코드 블록만 닫았다가 다음 덩어리에서 다시 연다).
 * 경계는 앞 글만 보고 정하므로 글이 뒤에 붙어도 앞 덩어리는 그대로다.
 */
export function mdChunks(text: string, chunk = CHUNK, show = SHOW_CHARS): MdChunk[] {
  const out: MdChunk[] = [];
  let start = 0;
  let pos = 0;
  // 열린 코드 블록: 여는 줄과 닫는 표시
  let open: { line: string; mark: string } | null = null;
  let reopen = "";
  for (;;) {
    const nl = text.indexOf("\n", pos);
    if (nl === -1) break;
    const line = text.slice(pos, nl);
    const t = line.trimStart();
    const m = FENCE.exec(t);
    if (m) {
      if (!open) open = { line, mark: m[1] };
      else if (t.startsWith(open.mark) && t.slice(open.mark.length).trim() === "") open = null;
    }
    pos = nl + 1;
    const size = pos - start;
    const cut = open ? size >= chunk * 4 : size >= chunk && (t.trim() === "" || size >= chunk * 2);
    if (cut) {
      out.push({ at: start, text: reopen + text.slice(start, pos) + (open ? `${open.mark}\n` : "") });
      reopen = open ? `${open.line}\n` : "";
      start = pos;
    }
  }
  if (start < text.length) out.push({ at: start, text: reopen + text.slice(start) });
  let first = 0;
  while (first < out.length - 1 && text.length - out[first].at > show) first++;
  return out.slice(first);
}
